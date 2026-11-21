//! Learned volume steps of one soundbar: "at level L, volume up/down gives N".
//! Pure data; saving and loading are plain text so they are easy to inspect.

use std::collections::BTreeMap;

/// Confirmations needed before a step is used to predict the next level.
pub const CONFIRMED: u8 = 2;
/// Confirmed steps in one direction, all the same size, before that size is
/// also used for levels not learned yet.
pub const UNIFORM_AFTER: usize = 4;
/// Hit counts stop growing here, so a fully learned table stops being rewritten.
const MAX_HITS: u8 = 3;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Dir {
    Up,
    Down,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Entry {
    next: u8,
    hits: u8,
}

/// What a confirmation did to the table.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// Matched what was learned (or was new); `changed` is true when the table changed.
    Agreed { changed: bool },
    /// Contradicted a learned step; the step was replaced by the new value.
    Contradicted,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Steps {
    entries: BTreeMap<(u8, Dir), Entry>,
}

impl Steps {
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// The predicted level after a press: the learned step from this level once
    /// confirmed enough, or else the soundbar's uniform step size if it has one.
    pub fn predict(&self, level: u8, dir: Dir) -> Option<u8> {
        if let Some(e) = self.entries.get(&(level, dir)) {
            return (e.hits >= CONFIRMED).then_some(e.next);
        }
        let step = self.uniform_step(dir)? as i16;
        let next = level as i16 + if dir == Dir::Up { step } else { -step };
        Some(next.clamp(0, 100) as u8)
    }

    /// The step size when every learned step in this direction has the same
    /// size and enough of them are confirmed. Steps ending at 0 or 100 are
    /// left out, since the limit may have cut them short.
    pub fn uniform_step(&self, dir: Dir) -> Option<u8> {
        let mut size = None;
        let mut confirmed = 0;
        for ((level, d), e) in &self.entries {
            if *d != dir || e.next == 0 || e.next == 100 {
                continue;
            }
            let delta = (e.next as i16 - *level as i16).unsigned_abs() as u8;
            if delta == 0 || size.is_some_and(|s| s != delta) {
                return None;
            }
            size = Some(delta);
            if e.hits >= CONFIRMED {
                confirmed += 1;
            }
        }
        size.filter(|_| confirmed >= UNIFORM_AFTER)
    }

    /// Records what the soundbar actually reported after a single press from `level`.
    pub fn confirm(&mut self, level: u8, dir: Dir, actual: u8) -> Outcome {
        match self.entries.get_mut(&(level, dir)) {
            Some(e) if e.next == actual => {
                let changed = e.hits < MAX_HITS;
                e.hits = (e.hits + 1).min(MAX_HITS);
                Outcome::Agreed { changed }
            }
            Some(e) => {
                *e = Entry { next: actual, hits: 1 };
                Outcome::Contradicted
            }
            None => {
                self.entries.insert((level, dir), Entry { next: actual, hits: 1 });
                Outcome::Agreed { changed: true }
            }
        }
    }

    /// Adds entries from `other` that this table does not have yet.
    pub fn merge_missing(&mut self, other: &Steps) {
        for (k, v) in &other.entries {
            self.entries.entry(*k).or_insert(*v);
        }
    }

    /// One line per step: `level up|down next hits`.
    pub fn to_text(&self) -> String {
        let mut out = String::new();
        for ((level, dir), e) in &self.entries {
            let d = if *dir == Dir::Up { "up" } else { "down" };
            out.push_str(&format!("{} {} {} {}\n", level, d, e.next, e.hits));
        }
        out
    }

    /// Ignores malformed lines, so a damaged file only loses those steps.
    pub fn from_text(text: &str) -> Steps {
        let mut steps = Steps::default();
        for line in text.lines() {
            let f: Vec<&str> = line.split_whitespace().collect();
            if f.len() != 4 {
                continue;
            }
            let dir = match f[1] {
                "up" => Dir::Up,
                "down" => Dir::Down,
                _ => continue,
            };
            let (Ok(level), Ok(next), Ok(hits)) = (f[0].parse::<u8>(), f[2].parse::<u8>(), f[3].parse::<u8>()) else {
                continue;
            };
            if level > 100 || next > 100 || hits == 0 {
                continue;
            }
            steps.entries.insert((level, dir), Entry { next, hits: hits.min(MAX_HITS) });
        }
        steps
    }
}

/// A soundbar's CEC name (or vendor id), short and file-name safe.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Name {
    bytes: [u8; 24],
    len: u8,
    /// A real OSD name, as opposed to a vendor-id stand-in.
    pub strong: bool,
}

impl Name {
    pub fn new(raw: &str, strong: bool) -> Option<Name> {
        let mut bytes = [0u8; 24];
        let mut len = 0usize;
        for c in raw.trim().chars() {
            if len == bytes.len() {
                break;
            }
            let c = if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else if c == ' ' || c == '.' { '_' } else { continue };
            bytes[len] = c as u8;
            len += 1;
        }
        if len == 0 {
            return None;
        }
        Some(Name { bytes, len: len as u8, strong })
    }

    pub fn as_str(&self) -> &str {
        std::str::from_utf8(&self.bytes[..self.len as usize]).unwrap_or("unknown")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn predicts_only_after_confirmation() {
        let mut s = Steps::default();
        assert_eq!(s.predict(14, Dir::Up), None);
        assert_eq!(s.confirm(14, Dir::Up, 16), Outcome::Agreed { changed: true });
        assert_eq!(s.predict(14, Dir::Up), None);
        assert_eq!(s.confirm(14, Dir::Up, 16), Outcome::Agreed { changed: true });
        assert_eq!(s.predict(14, Dir::Up), Some(16));
        assert_eq!(s.confirm(14, Dir::Up, 16), Outcome::Agreed { changed: true });
        assert_eq!(s.confirm(14, Dir::Up, 16), Outcome::Agreed { changed: false });
        assert_eq!(s.predict(14, Dir::Down), None);
    }

    #[test]
    fn contradiction_relearns() {
        let mut s = Steps::default();
        s.confirm(20, Dir::Down, 18);
        s.confirm(20, Dir::Down, 18);
        assert_eq!(s.confirm(20, Dir::Down, 19), Outcome::Contradicted);
        assert_eq!(s.predict(20, Dir::Down), None);
        s.confirm(20, Dir::Down, 19);
        assert_eq!(s.predict(20, Dir::Down), Some(19));
    }

    #[test]
    fn text_round_trip_and_bad_lines() {
        let mut s = Steps::default();
        s.confirm(14, Dir::Up, 16);
        s.confirm(14, Dir::Up, 16);
        s.confirm(0, Dir::Down, 0);
        let back = Steps::from_text(&(s.to_text() + "junk\n200 up 1 1\n5 sideways 6 1\n"));
        assert_eq!(back, s);
    }

    #[test]
    fn names_are_file_safe() {
        assert_eq!(Name::new("HW-Q990B", true).unwrap().as_str(), "HW-Q990B");
        assert_eq!(Name::new("Polk ../Signa*S4", true).unwrap().as_str(), "Polk___SignaS4");
        assert!(Name::new("  ", true).is_none());
    }

    #[test]
    fn uniform_steps_cover_unvisited_levels() {
        let mut s = Steps::default();
        for level in [10u8, 12, 14] {
            s.confirm(level, Dir::Up, level + 2);
            s.confirm(level, Dir::Up, level + 2);
        }
        assert_eq!(s.predict(40, Dir::Up), None, "three steps are not enough");
        s.confirm(16, Dir::Up, 18);
        s.confirm(16, Dir::Up, 18);
        assert_eq!(s.predict(40, Dir::Up), Some(42));
        assert_eq!(s.predict(99, Dir::Up), Some(100));
        assert_eq!(s.predict(40, Dir::Down), None, "each direction is learned on its own");
        // A step at the limit does not break uniformity.
        s.confirm(98, Dir::Up, 100);
        assert_eq!(s.uniform_step(Dir::Up), Some(2));
        // One different step (even unconfirmed) does.
        s.confirm(30, Dir::Up, 31);
        assert_eq!(s.predict(40, Dir::Up), None);
        assert_eq!(s.predict(10, Dir::Up), Some(12), "confirmed steps are still used");
    }
}
