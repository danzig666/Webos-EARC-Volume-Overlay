//! Reads the TV's own CEC message log. Nothing is sent on the CEC bus.
//!
//! Relevant lines on an LG C3 look like:
//!   2026-11-07T07:44:24.384197Z [CECMSG] TM91432 TX054442   (TV -> soundbar: volume down key)
//!   2026-11-07T07:44:24.987624Z [CECMSG] TM325715 RX507A0E  (soundbar -> TV: Report Audio Status, 14)

use std::ffi::CString;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::os::unix::fs::MetadataExt;
use std::sync::mpsc::Sender;
use std::time::{Duration, Instant};

use crate::learn::Name;
use crate::{debug, log, Event, Key};

const MAX_BACKLOG: u64 = 65536;
const MISSING_GRACE: Duration = Duration::from_secs(5);
/// How much of the existing log is scanned at start to identify the soundbar.
const IDENTITY_SCAN: u64 = 512 * 1024;

/// Finds the audio system's (logical address 5) name in the log: its Set OSD
/// Name message (`RX5047` + ASCII hex), the TV's periodic device dump
/// (`LADDR:0x05` then `OSD(name)`), or as a weaker stand-in its vendor id.
#[derive(Default)]
pub struct Identity {
    after_laddr5: bool,
    best: Option<Name>,
}

impl Identity {
    fn parse(&mut self, line: &str) -> Option<Name> {
        if line.contains("LADDR:") {
            self.after_laddr5 = line.contains("LADDR:0x05");
            return None;
        }
        if self.after_laddr5 {
            if let Some(at) = line.find("OSD(") {
                self.after_laddr5 = false;
                let rest = &line[at + 4..];
                return Name::new(&rest[..rest.find(')')?], true);
            }
        }
        for token in line.split_ascii_whitespace() {
            if let Some(hex) = token.strip_prefix("RX5047") {
                if hex.len() % 2 != 0 || hex.is_empty() {
                    return None;
                }
                let text: String = (0..hex.len())
                    .step_by(2)
                    .filter_map(|i| u8::from_str_radix(&hex[i..i + 2], 16).ok())
                    .filter(|b| b.is_ascii_graphic() || *b == b' ')
                    .map(|b| b as char)
                    .collect();
                return Name::new(&text, true);
            }
            if let Some(hex) = token.strip_prefix("RX5F87") {
                if hex.len() == 6 && hex.chars().all(|c| c.is_ascii_hexdigit()) {
                    return Name::new(&format!("vendor-{}", hex.to_ascii_uppercase()), false);
                }
            }
        }
        None
    }

    /// Feeds one line; returns a name when the identity changed.
    pub fn feed(&mut self, line: &str) -> Option<Name> {
        let name = self.parse(line)?;
        let better = match self.best {
            None => true,
            Some(b) if b.strong => name.strong && b != name,
            Some(b) => name.strong || b != name,
        };
        if better {
            self.best = Some(name);
            Some(name)
        } else {
            None
        }
    }
}

/// Scans the end of the existing log for the soundbar's name.
fn identify_from_backlog(path: &str, identity: &mut Identity) -> Option<Name> {
    let mut file = File::open(path).ok()?;
    let len = file.metadata().ok()?.len();
    file.seek(SeekFrom::Start(len.saturating_sub(IDENTITY_SCAN))).ok()?;
    let mut text = Vec::new();
    file.read_to_end(&mut text).ok()?;
    let mut last_strong = None;
    let mut last_weak = None;
    let mut scan = Identity::default();
    for line in String::from_utf8_lossy(&text).lines() {
        if let Some(n) = scan.parse(line) {
            if n.strong { last_strong = Some(n) } else { last_weak = Some(n) }
        }
    }
    let found = last_strong.or(last_weak)?;
    identity.best = Some(found);
    Some(found)
}

/// Parses one log line into an event, if it is one we care about.
pub fn parse_line(line: &str) -> Option<Event> {
    for token in line.split_ascii_whitespace() {
        // Report Audio Status from the audio system (5) to the TV (0).
        // Operand: bit 7 = mute, bits 0-6 = volume 0-100 (0x7F = unknown).
        if let Some(hex) = token.strip_prefix("RX507A") {
            let byte = parse_hex_byte(hex)?;
            let volume = byte & 0x7F;
            if volume > 100 {
                return None;
            }
            return Some(Event::Report { volume, mute: byte & 0x80 != 0 });
        }
        // User Control Pressed from the TV (0) to the audio system (5).
        if let Some(hex) = token.strip_prefix("TX0544") {
            return match parse_hex_byte(hex)? {
                0x41 => Some(Event::Key(Key::VolumeUp)),
                0x42 => Some(Event::Key(Key::VolumeDown)),
                0x43 | 0x65 | 0x66 => Some(Event::Key(Key::Mute)),
                _ => None,
            };
        }
    }
    None
}

fn parse_hex_byte(s: &str) -> Option<u8> {
    if s.len() != 2 {
        return None;
    }
    u8::from_str_radix(s, 16).ok()
}

struct Inotify {
    fd: i32,
}

impl Inotify {
    /// Watches the log's directory so rotation and re-creation are seen too.
    fn new(dir: &str) -> Option<Inotify> {
        let fd = unsafe { libc::inotify_init1(libc::IN_CLOEXEC | libc::IN_NONBLOCK) };
        if fd < 0 {
            return None;
        }
        let path = CString::new(dir).ok()?;
        let mask = libc::IN_MODIFY | libc::IN_CREATE | libc::IN_MOVED_TO | libc::IN_CLOSE_WRITE;
        if unsafe { libc::inotify_add_watch(fd, path.as_ptr(), mask) } < 0 {
            unsafe { libc::close(fd) };
            return None;
        }
        Some(Inotify { fd })
    }

    /// Blocks until the named file changes or the timeout passes.
    fn wait(&self, name: &[u8], timeout_ms: i32) {
        loop {
            let mut pfd = libc::pollfd { fd: self.fd, events: libc::POLLIN, revents: 0 };
            let ready = unsafe { libc::poll(&mut pfd, 1, timeout_ms) };
            if ready <= 0 {
                return;
            }
            let mut buf = [0u8; 4096];
            let n = unsafe { libc::read(self.fd, buf.as_mut_ptr() as *mut libc::c_void, buf.len()) };
            if n <= 0 {
                return;
            }
            let mut offset = 0usize;
            let header = std::mem::size_of::<libc::inotify_event>();
            let mut hit = false;
            while offset + header <= n as usize {
                let event = unsafe { &*(buf.as_ptr().add(offset) as *const libc::inotify_event) };
                let len = event.len as usize;
                let raw = &buf[offset + header..(offset + header + len).min(n as usize)];
                let end = raw.iter().position(|&b| b == 0).unwrap_or(raw.len());
                if &raw[..end] == name {
                    hit = true;
                }
                offset += header + len;
            }
            if hit {
                return;
            }
            // Another file in the directory changed; keep waiting.
        }
    }
}

/// Follows the log forever, sending parsed events. Starts at the end of the
/// file so old entries are never replayed.
pub fn follow(path: &str, tx: Sender<Event>) {
    let (dir, name) = match path.rfind('/') {
        Some(i) => (&path[..i.max(1)], &path.as_bytes()[i + 1..]),
        None => (".", path.as_bytes()),
    };
    let watcher = Inotify::new(dir);
    if watcher.is_none() {
        log("inotify unavailable; polling the CEC log every 150 ms");
    }

    let (mut position, mut inode, mut present) = match std::fs::metadata(path) {
        Ok(m) => (m.len(), m.ino(), true),
        Err(_) => (0, 0, false),
    };
    if tx.send(Event::CecLog(present)).is_err() {
        return;
    }
    let mut identity = Identity::default();
    if let Some(name) = identify_from_backlog(path, &mut identity) {
        if tx.send(Event::Soundbar(name)).is_err() {
            return;
        }
    }
    let mut missing_since: Option<Instant> = None;
    let mut pending: Vec<u8> = Vec::new();

    loop {
        match &watcher {
            Some(w) => w.wait(name, 1000),
            None => std::thread::sleep(Duration::from_millis(150)),
        }
        let Ok(mut file) = File::open(path) else {
            // Rotation removes the file briefly; only a lasting absence counts.
            let since = *missing_since.get_or_insert_with(Instant::now);
            if present && since.elapsed() >= MISSING_GRACE {
                present = false;
                if tx.send(Event::CecLog(false)).is_err() {
                    return;
                }
            }
            continue;
        };
        missing_since = None;
        if !present {
            present = true;
            if tx.send(Event::CecLog(true)).is_err() {
                return;
            }
        }
        let Ok(meta) = file.metadata() else { continue };
        if meta.ino() != inode || meta.len() < position {
            // Rotated or truncated: the new file holds only new lines.
            position = 0;
            pending.clear();
            inode = meta.ino();
        }
        if meta.len() - position > MAX_BACKLOG {
            position = meta.len() - MAX_BACKLOG;
            pending.clear();
        }
        if meta.len() == position || file.seek(SeekFrom::Start(position)).is_err() {
            continue;
        }
        let mut chunk = Vec::with_capacity((meta.len() - position) as usize);
        let Ok(count) = file.read_to_end(&mut chunk) else { continue };
        position += count as u64;
        pending.extend_from_slice(&chunk);

        let mut start = 0;
        while let Some(i) = pending[start..].iter().position(|&b| b == b'\n') {
            let line = String::from_utf8_lossy(&pending[start..start + i]);
            if let Some(event) = parse_line(&line) {
                debug(&format!("cec: {}", line.trim()));
                if tx.send(event).is_err() {
                    return;
                }
            } else if let Some(name) = identity.feed(&line) {
                if tx.send(Event::Soundbar(name)).is_err() {
                    return;
                }
            }
            start += i + 1;
        }
        pending.drain(..start);
        if pending.len() > MAX_BACKLOG as usize {
            pending.clear();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_c3_lines() {
        let report = parse_line("2026-11-07T07:44:24.987624Z [CECMSG] TM325715 RX507A0E");
        assert_eq!(report, Some(Event::Report { volume: 14, mute: false }));
        let key = parse_line("2026-11-07T07:44:24.384197Z [CECMSG] TM91432 TX054442");
        assert_eq!(key, Some(Event::Key(Key::VolumeDown)));
        assert_eq!(parse_line("[CECMSG] TX054441"), Some(Event::Key(Key::VolumeUp)));
        assert_eq!(parse_line("[CECMSG] TX054443"), Some(Event::Key(Key::Mute)));
    }

    #[test]
    fn decodes_mute_bit_and_rejects_unknown() {
        assert_eq!(parse_line("[CECMSG] RX507A91"), Some(Event::Report { volume: 17, mute: true }));
        assert_eq!(parse_line("[CECMSG] RX507A7F"), None);
        assert_eq!(parse_line("[CECMSG] RX507AFF"), None);
    }

    #[test]
    fn ignores_other_messages() {
        assert_eq!(parse_line("[CECMSG] TX0545"), None);
        assert_eq!(parse_line("[CECMSG] RX5F7210"), None);
        assert_eq!(parse_line("[CECMSG] RX507A0E00"), None);
        assert_eq!(parse_line("[CECMSG] RX407A10"), None);
        assert_eq!(parse_line(""), None);
    }

    #[test]
    fn identifies_soundbar() {
        let mut id = Identity::default();
        // Vendor id first (weak), then the C2's device dump (strong).
        let v = id.feed("2026-11-07T10:45:49.011876Z [CECMSG] TM158 RX5F870000F0").unwrap();
        assert_eq!(v.as_str(), "vendor-0000F0");
        assert!(!v.strong);
        assert_eq!(id.feed("[INFO] [Display:403] LADDR:0x0B, FLAGs:0x2FE, 0:VendorId(0x00044B)"), None);
        assert_eq!(id.feed("[INFO] [Display:409]  + 3:vdId(0x0F), 5:OSD(SHIELD), 6:pAddr(4000)"), None);
        assert_eq!(id.feed("[INFO] [Display:403] LADDR:0x05, FLAGs:0x2FE(FIN?0xC0), 0:VendorId(0x0000F0)"), None);
        let n = id.feed("[INFO] [Display:409]  + 3:vdId(0x0F), 4:pSt(0x03), 5:OSD(HW-Q990B), 6:pAddr(2000)").unwrap();
        assert_eq!(n.as_str(), "HW-Q990B");
        assert!(n.strong);
        // Repeats and weaker names change nothing.
        assert_eq!(id.feed("[INFO] [Display:403] LADDR:0x05, FLAGs:0x2FE"), None);
        assert_eq!(id.feed("[INFO] [Display:409] 5:OSD(HW-Q990B)"), None);
        assert_eq!(id.feed("[CECMSG] RX5F870000F0"), None);
        // Set OSD Name: "Polk S4".
        let p = id.feed("[CECMSG] TM1 RX5047506F6C6B205334").unwrap();
        assert_eq!(p.as_str(), "Polk_S4");
    }
}
