//! Decides when to show the overlay and with which value. Pure logic, no I/O.

use std::time::{Duration, Instant};

use crate::learn::{Dir, Name, Outcome, Steps};
use crate::{Event, Key};

/// How long to wait for the soundbar's answer to a key press before the
/// dimmed value is shown as final (e.g. a press at 0 or 100 that changes nothing).
const REPLY_WAIT: Duration = Duration::from_millis(1500);
/// Minimum gap between "key pressed" updates while a key is held.
const PENDING_REPEAT: Duration = Duration::from_millis(250);
/// Unrequested reports right after switching to ARC (wake, input change)
/// update the value silently instead of popping up the overlay.
const QUIET_AFTER_SWITCH: Duration = Duration::from_secs(4);
/// Consecutive key presses without a soundbar report in the log before the
/// watcher also reads webOS's stored copy of the soundbar's volume.
const UNANSWERED_BEFORE_FALLBACK: u32 = 2;
/// Consecutive key presses that no source confirmed before the overlay stops
/// appearing for key presses, so it never keeps showing a stale number.
const SILENT_BEFORE_HIDING: u32 = 2;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Show {
    pub volume: u8,
    pub mute: bool,
    /// True when the soundbar has not yet confirmed the latest key press.
    pub pending: bool,
}

/// A key press still waiting for the soundbar's answer.
#[derive(Debug, Clone, Copy)]
struct Press {
    /// The level the press started from, if known.
    from: Option<u8>,
    /// True when `from` is a level the soundbar confirmed (not a prediction).
    from_confirmed: bool,
    dir: Option<Dir>,
    /// The level shown in advance, if the step was learned.
    predicted: Option<u8>,
}

#[derive(Default)]
pub struct Engine {
    arc: Option<bool>,
    volume: Option<u8>,
    mute: bool,
    awaiting: Option<Instant>,
    last_pending_show: Option<Instant>,
    quiet_until: Option<Instant>,
    /// Whether the TV's CEC log exists (None until known).
    log_present: Option<bool>,
    /// Consecutive key presses the soundbar did not answer in the CEC log.
    unanswered: u32,
    /// Consecutive key presses no source confirmed (no report, no polled change).
    silent: u32,
    /// Previous polled value; a poll counts as a change only against this,
    /// never against a CEC report or seed of a different age.
    last_polled: Option<u8>,
    /// Presses sent to the soundbar and not answered yet, oldest first.
    presses: Vec<Press>,
    /// Learned volume steps of the current soundbar.
    steps: Steps,
    soundbar: Option<Name>,
    steps_dirty: bool,
    /// Predicted level after the presses in flight; None when unknown.
    virtual_level: Option<u8>,
    /// A press went unanswered, so the soundbar may be at a different level
    /// than `volume`; nothing is learned until it reports again.
    volume_stale: bool,
}

impl Engine {
    pub fn on_arc(&self) -> bool {
        self.arc == Some(true)
    }

    /// Unknown output counts as ARC: the CEC messages themselves only
    /// flow while an audio system is in use.
    fn allowed(&self) -> bool {
        self.arc != Some(false)
    }

    fn quiet(&self, now: Instant) -> bool {
        self.quiet_until.is_some_and(|t| now < t)
    }

    /// True when the CEC log cannot supply the soundbar's reports, so the
    /// stored ARC volume must be polled instead. The TV's own master volume
    /// is never used as a number.
    pub fn wants_polling(&self) -> bool {
        self.allowed()
            && (self.log_present == Some(false) || self.unanswered >= UNANSWERED_BEFORE_FALLBACK)
    }

    /// True when key presses keep going unconfirmed by every source (e.g. a
    /// soundbar that never reports its volume). The overlay then stays hidden
    /// for key presses until a real value arrives again.
    pub fn starved(&self) -> bool {
        self.silent >= SILENT_BEFORE_HIDING
    }

    pub fn soundbar(&self) -> Option<Name> {
        self.soundbar
    }

    pub fn steps(&self) -> &Steps {
        &self.steps
    }

    /// True once after the learned steps changed, so the caller can save them.
    pub fn take_steps_dirty(&mut self) -> bool {
        std::mem::take(&mut self.steps_dirty)
    }

    /// Switches to another soundbar's learned steps. Steps learned before the
    /// soundbar was positively identified are kept where the new table has none.
    pub fn set_soundbar(&mut self, name: Name, mut loaded: Steps) {
        let weak = self.soundbar.is_none_or(|n| !n.strong);
        if weak {
            loaded.merge_missing(&self.steps);
            self.steps_dirty = !self.steps.is_empty();
        }
        self.soundbar = Some(name);
        self.steps = loaded;
        self.presses.clear();
        self.virtual_level = self.volume;
    }

    /// A volume or mute key was pressed: show the learned next level at once,
    /// or the last level dimmed while the soundbar's answer is on its way.
    fn press(&mut self, key: Option<Key>, now: Instant) -> Option<Show> {
        if !self.allowed() {
            return None;
        }
        self.awaiting = Some(now);
        if self.starved() {
            self.presses.clear();
            return None;
        }
        let dir = match key {
            Some(Key::VolumeUp) => Some(Dir::Up),
            Some(Key::VolumeDown) => Some(Dir::Down),
            _ => None,
        };
        let from = if self.presses.is_empty() { self.volume } else { self.virtual_level };
        let from_confirmed = self.presses.is_empty() && self.volume.is_some() && !self.volume_stale;
        let predicted = match (from, dir) {
            (Some(level), Some(d)) if !self.mute => self.steps.predict(level, d),
            _ => None,
        };
        if self.presses.len() < 16 {
            self.presses.push(Press { from, from_confirmed, dir, predicted });
        }
        self.virtual_level = predicted;
        if let Some(volume) = predicted {
            self.last_pending_show = None;
            return Some(Show { volume, mute: self.mute, pending: false });
        }
        let volume = from.or(self.volume)?;
        if self.last_pending_show.is_some_and(|t| now.duration_since(t) < PENDING_REPEAT) {
            return None;
        }
        self.last_pending_show = Some(now);
        Some(Show { volume, mute: self.mute, pending: true })
    }

    pub fn handle(&mut self, event: Event, now: Instant) -> Option<Show> {
        match event {
            Event::Key(key) => self.press(Some(key), now),
            Event::Report { volume, mute } => {
                self.unanswered = 0;
                self.silent = 0;
                self.last_polled = None;
                let stale = std::mem::take(&mut self.volume_stale);
                if stale {
                    // Learning resumes from this confirmed level, not this answer.
                    for p in &mut self.presses {
                        p.from_confirmed = false;
                    }
                }
                if let Some(true) = self.answer(volume, mute) {
                    // The level shown in advance was right; nothing to update.
                    if self.presses.is_empty() {
                        self.awaiting = None;
                    }
                    self.last_pending_show = None;
                    return None;
                }
                let changed = self.volume.is_some_and(|v| v != volume) || (self.volume.is_some() && self.mute != mute);
                self.volume = Some(volume);
                self.mute = mute;
                let asked = self.awaiting.take().is_some_and(|t| now.duration_since(t) < REPLY_WAIT);
                self.last_pending_show = None;
                if self.allowed() && (asked || (changed && !self.quiet(now))) {
                    Some(Show { volume, mute, pending: false })
                } else {
                    None
                }
            }
            Event::Polled(volume) => {
                if !self.wants_polling() {
                    self.last_polled = None;
                    return None;
                }
                let changed = self.last_polled.replace(volume).is_some_and(|v| v != volume);
                if self.volume.is_none() {
                    self.volume = Some(volume);
                }
                if !changed {
                    // A key press may still be on its way; tick() finalises it.
                    return None;
                }
                self.volume = Some(volume);
                self.silent = 0;
                let asked = self.awaiting.take().is_some_and(|t| now.duration_since(t) < REPLY_WAIT);
                self.last_pending_show = None;
                if self.quiet(now) && !asked {
                    return None;
                }
                Some(Show { volume, mute: self.mute, pending: false })
            }
            Event::Master { mute } => {
                // Only a trigger while polling; with CEC reports the
                // soundbar's own mute bit is authoritative.
                if !self.wants_polling() {
                    return None;
                }
                self.mute = mute;
                self.press(None, now)
            }
            Event::CecLog(present) => {
                self.log_present = Some(present);
                None
            }
            Event::Output(arc) => {
                let was = self.arc;
                self.arc = Some(arc);
                if arc && was != Some(true) {
                    if was == Some(false) {
                        // The soundbar may have changed while not in use.
                        self.volume = None;
                    }
                    self.quiet_until = Some(now + QUIET_AFTER_SWITCH);
                }
                if !arc {
                    self.awaiting = None;
                    self.last_pending_show = None;
                    self.presses.clear();
                }
                None
            }
            // Loading the soundbar's learned steps is done by the caller.
            Event::Soundbar(_) => None,
            Event::Seed(volume) => {
                if self.volume.is_none() {
                    self.volume = Some(volume);
                }
                None
            }
        }
    }

    /// Called when `next_deadline` passes without an event.
    pub fn tick(&mut self, now: Instant) -> Option<Show> {
        let since = self.awaiting?;
        if now.duration_since(since) < REPLY_WAIT {
            return None;
        }
        self.awaiting = None;
        self.last_pending_show = None;
        self.unanswered = self.unanswered.saturating_add(1);
        self.silent = self.silent.saturating_add(1);
        let predicted_shown = self.presses.iter().any(|p| p.predicted.is_some());
        if !self.presses.is_empty() {
            self.volume_stale = true;
        }
        self.presses.clear();
        self.virtual_level = self.volume;
        if !self.allowed() || self.starved() || predicted_shown {
            // A level shown in advance stays on screen as it was; no newer
            // confirmed value exists to replace it with.
            return None;
        }
        Some(Show { volume: self.volume?, mute: self.mute, pending: false })
    }

    /// Matches a soundbar report against the presses in flight and learns from it.
    /// Returns Some(true) when the report confirms what is already on screen,
    /// Some(false) when it answered presses but the screen needs the real value,
    /// and None when no press was waiting (e.g. the soundbar's own remote).
    fn answer(&mut self, volume: u8, mute: bool) -> Option<bool> {
        if self.presses.is_empty() {
            self.virtual_level = Some(volume);
            return None;
        }
        let single = self.presses.len() == 1;
        let first = self.presses[0];
        // Learn only from one isolated press that started at a confirmed level;
        // held keys may get one combined answer for several presses.
        if single && first.from_confirmed && !mute && !self.mute {
            if let (Some(from), Some(dir)) = (first.from, first.dir) {
                match self.steps.confirm(from, dir, volume) {
                    Outcome::Agreed { changed } => self.steps_dirty |= changed,
                    Outcome::Contradicted => {
                        self.steps_dirty = true;
                        crate::log(&format!(
                            "learned step {} {:?} was wrong (soundbar said {}); relearning",
                            from, dir, volume
                        ));
                    }
                }
            }
        }
        // A report matching one of the predictions answers every press up to it.
        if let Some(i) = self.presses.iter().position(|p| p.predicted == Some(volume)) {
            if mute == self.mute {
                self.presses.drain(..=i);
                if self.presses.is_empty() {
                    self.virtual_level = Some(volume);
                }
                self.volume = Some(volume);
                return Some(true);
            }
        }
        self.presses.clear();
        self.virtual_level = Some(volume);
        Some(false)
    }

    pub fn next_deadline(&self, now: Instant) -> Option<Duration> {
        self.awaiting.map(|t| (t + REPLY_WAIT).saturating_duration_since(now))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Key;

    fn ms(base: Instant, n: u64) -> Instant {
        base + Duration::from_millis(n)
    }

    fn ready(t0: Instant, volume: u8) -> Engine {
        let mut e = Engine::default();
        e.handle(Event::Output(true), t0);
        e.handle(Event::Seed(volume), t0);
        e
    }

    #[test]
    fn key_shows_dimmed_then_confirmed() {
        let t0 = Instant::now();
        let mut e = ready(t0, 16);
        let t = ms(t0, 5000);
        assert_eq!(e.handle(Event::Key(Key::VolumeDown), t), Some(Show { volume: 16, mute: false, pending: true }));
        assert_eq!(
            e.handle(Event::Report { volume: 14, mute: false }, ms(t, 600)),
            Some(Show { volume: 14, mute: false, pending: false })
        );
    }

    #[test]
    fn same_value_reply_to_key_is_still_shown() {
        let t0 = Instant::now();
        let mut e = ready(t0, 100);
        let t = ms(t0, 5000);
        e.handle(Event::Key(Key::VolumeUp), t);
        assert!(e.handle(Event::Report { volume: 100, mute: false }, ms(t, 500)).is_some());
    }

    #[test]
    fn soundbar_remote_change_is_shown_without_key() {
        let t0 = Instant::now();
        let mut e = ready(t0, 16);
        let r = e.handle(Event::Report { volume: 18, mute: false }, ms(t0, 9000));
        assert_eq!(r, Some(Show { volume: 18, mute: false, pending: false }));
        assert_eq!(e.handle(Event::Report { volume: 18, mute: false }, ms(t0, 9500)), None);
        assert!(e.handle(Event::Report { volume: 18, mute: true }, ms(t0, 9600)).is_some());
    }

    #[test]
    fn unanswered_key_is_finalised() {
        let t0 = Instant::now();
        let mut e = ready(t0, 0);
        let t = ms(t0, 5000);
        e.handle(Event::Key(Key::VolumeDown), t);
        assert_eq!(e.tick(ms(t, 1000)), None);
        assert_eq!(e.tick(ms(t, 1500)), Some(Show { volume: 0, mute: false, pending: false }));
        assert_eq!(e.next_deadline(ms(t, 1600)), None);
    }

    #[test]
    fn held_key_is_rate_limited() {
        let t0 = Instant::now();
        let mut e = ready(t0, 20);
        let t = ms(t0, 5000);
        assert!(e.handle(Event::Key(Key::VolumeUp), t).is_some());
        assert!(e.handle(Event::Key(Key::VolumeUp), ms(t, 100)).is_none());
        assert!(e.handle(Event::Key(Key::VolumeUp), ms(t, 300)).is_some());
    }

    #[test]
    fn nothing_when_not_on_arc_and_quiet_after_switch() {
        let t0 = Instant::now();
        let mut e = ready(t0, 20);
        e.handle(Event::Output(false), ms(t0, 1000));
        assert_eq!(e.handle(Event::Key(Key::VolumeUp), ms(t0, 2000)), None);
        assert_eq!(e.handle(Event::Report { volume: 30, mute: false }, ms(t0, 2100)), None);
        e.handle(Event::Output(true), ms(t0, 3000));
        // Value was cleared on the switch; first report only seeds it.
        assert_eq!(e.handle(Event::Report { volume: 25, mute: false }, ms(t0, 3500)), None);
        assert_eq!(e.handle(Event::Report { volume: 26, mute: false }, ms(t0, 4000)), None);
        assert!(e.handle(Event::Report { volume: 27, mute: false }, ms(t0, 8000)).is_some());
    }

    #[test]
    fn key_before_any_value_waits_for_report() {
        let t0 = Instant::now();
        let mut e = Engine::default();
        assert_eq!(e.handle(Event::Key(Key::VolumeUp), t0), None);
        assert!(e.handle(Event::Report { volume: 9, mute: false }, ms(t0, 600)).is_some());
    }

    #[test]
    fn missing_log_polls_stored_arc_volume() {
        let t0 = Instant::now();
        let mut e = ready(t0, 20);
        assert!(!e.wants_polling());
        e.handle(Event::CecLog(false), t0);
        assert!(e.wants_polling());
        // Master activity is only a trigger: last level, dimmed.
        let t = ms(t0, 5000);
        assert_eq!(e.handle(Event::Master { mute: false }, t), Some(Show { volume: 20, mute: false, pending: true }));
        assert_eq!(e.handle(Event::Polled(20), ms(t, 150)), None);
        assert_eq!(
            e.handle(Event::Polled(22), ms(t, 300)),
            Some(Show { volume: 22, mute: false, pending: false })
        );
        // Soundbar-remote changes picked up by the idle poll.
        assert!(e.handle(Event::Polled(25), ms(t, 9000)).is_some());
        e.handle(Event::CecLog(true), ms(t, 9500));
        assert!(!e.wants_polling());
        assert_eq!(e.handle(Event::Polled(30), ms(t, 9600)), None);
        assert_eq!(e.handle(Event::Master { mute: true }, ms(t, 9700)), None);
    }

    #[test]
    fn soundbar_that_never_answers_stops_showing_stale_numbers() {
        // LG C2 + Samsung HW-Q990B: one report at power-on, then key presses
        // that nothing ever answers.
        let t0 = Instant::now();
        let mut e = ready(t0, 22);
        e.handle(Event::CecLog(true), t0);
        e.handle(Event::Report { volume: 5, mute: false }, ms(t0, 100));
        for n in 0..2 {
            let t = ms(t0, 5000 + n * 3000);
            assert!(e.handle(Event::Key(Key::VolumeUp), t).is_some());
            e.tick(ms(t, 1500));
        }
        assert!(e.wants_polling(), "a power-on report must not disable the fallback");
        assert!(e.starved());
        let t = ms(t0, 20000);
        assert_eq!(e.handle(Event::Key(Key::VolumeUp), t), None, "no stale number");
        assert_eq!(e.tick(ms(t, 1500)), None);
        // The first poll is only a baseline, even if it differs from the report.
        assert_eq!(e.handle(Event::Polled(22), ms(t, 1800)), None);
        assert!(e.starved());
        // A real change brings the overlay back.
        assert!(e.handle(Event::Polled(9), ms(t, 2000)).is_some());
        assert!(!e.starved());
        assert!(e.handle(Event::Key(Key::VolumeUp), ms(t, 5000)).is_some());
    }

    #[test]
    fn single_missed_reply_does_not_hide_the_overlay() {
        let t0 = Instant::now();
        let mut e = ready(t0, 10);
        e.handle(Event::CecLog(true), t0);
        let t = ms(t0, 5000);
        e.handle(Event::Key(Key::VolumeUp), t);
        assert!(e.tick(ms(t, 1500)).is_some());
        assert!(!e.starved());
        e.handle(Event::Key(Key::VolumeUp), ms(t, 3000));
        assert!(e.handle(Event::Report { volume: 11, mute: false }, ms(t, 3600)).is_some());
        assert!(!e.wants_polling());
    }

    #[test]
    fn log_without_reports_falls_back_after_unanswered_keys() {
        let t0 = Instant::now();
        let mut e = ready(t0, 10);
        e.handle(Event::CecLog(true), t0);
        for n in 0..2 {
            let t = ms(t0, 5000 + n * 3000);
            e.handle(Event::Key(Key::VolumeUp), t);
            e.tick(ms(t, 1500));
        }
        assert!(e.wants_polling());
        // One report from the log switches back to the fast path for good.
        e.handle(Event::Report { volume: 12, mute: false }, ms(t0, 20000));
        assert!(!e.wants_polling());
    }

    #[test]
    fn answered_keys_never_poll() {
        let t0 = Instant::now();
        let mut e = ready(t0, 10);
        e.handle(Event::CecLog(true), t0);
        for n in 0..5u64 {
            let t = ms(t0, 10000 + n * 3000);
            e.handle(Event::Key(Key::VolumeUp), t);
            e.handle(Event::Report { volume: 11 + n as u8, mute: false }, ms(t, 600));
            assert_eq!(e.tick(ms(t, 1500)), None);
        }
        assert!(!e.wants_polling());
        assert!(!e.starved());
    }

    fn learned(t0: Instant) -> Engine {
        // C3 + Polk: from 14, volume up gives 16, seen twice.
        let mut e = ready(t0, 14);
        e.handle(Event::CecLog(true), t0);
        for n in 0..2u64 {
            let t = ms(t0, 5000 + n * 4000);
            assert_eq!(e.handle(Event::Key(Key::VolumeUp), t), Some(Show { volume: 14, mute: false, pending: true }));
            e.handle(Event::Report { volume: 16, mute: false }, ms(t, 600));
            e.handle(Event::Key(Key::VolumeDown), ms(t, 2000));
            e.handle(Event::Report { volume: 14, mute: false }, ms(t, 2600));
        }
        assert!(e.take_steps_dirty());
        e
    }

    #[test]
    fn learned_step_is_shown_at_once_and_confirmed_quietly() {
        let t0 = Instant::now();
        let mut e = learned(t0);
        let t = ms(t0, 20000);
        assert_eq!(e.handle(Event::Key(Key::VolumeUp), t), Some(Show { volume: 16, mute: false, pending: false }));
        assert_eq!(e.handle(Event::Report { volume: 16, mute: false }, ms(t, 600)), None, "already on screen");
        assert_eq!(e.next_deadline(ms(t, 700)), None);
        assert_eq!(e.handle(Event::Key(Key::VolumeDown), ms(t, 2000)), Some(Show { volume: 14, mute: false, pending: false }));
    }

    #[test]
    fn wrong_prediction_is_corrected_and_relearned() {
        let t0 = Instant::now();
        let mut e = learned(t0);
        let t = ms(t0, 20000);
        assert_eq!(e.handle(Event::Key(Key::VolumeUp), t).map(|s| s.volume), Some(16));
        // Soundbar firmware changed its steps: it now goes 14 -> 15.
        assert_eq!(
            e.handle(Event::Report { volume: 15, mute: false }, ms(t, 600)),
            Some(Show { volume: 15, mute: false, pending: false })
        );
        e.handle(Event::Key(Key::VolumeDown), ms(t, 2000));
        e.handle(Event::Report { volume: 14, mute: false }, ms(t, 2600));
        // The contradicted step is no longer predicted until confirmed again.
        assert_eq!(e.handle(Event::Key(Key::VolumeUp), ms(t, 5000)).map(|s| s.pending), Some(true));
        e.handle(Event::Report { volume: 15, mute: false }, ms(t, 5600));
        e.handle(Event::Key(Key::VolumeDown), ms(t, 7000));
        e.handle(Event::Report { volume: 14, mute: false }, ms(t, 7600));
        assert_eq!(e.handle(Event::Key(Key::VolumeUp), ms(t, 9000)).map(|s| (s.volume, s.pending)), Some((15, false)));
    }

    #[test]
    fn held_key_chains_predictions_without_learning() {
        let t0 = Instant::now();
        let mut e = learned(t0);
        // Also learn 16 -> 18.
        for n in 0..2u64 {
            let t = ms(t0, 20000 + n * 4000);
            e.handle(Event::Key(Key::VolumeUp), t);
            e.handle(Event::Report { volume: 16, mute: false }, ms(t, 600));
            e.handle(Event::Key(Key::VolumeUp), ms(t, 1500));
            e.handle(Event::Report { volume: 18, mute: false }, ms(t, 2100));
            e.handle(Event::Key(Key::VolumeDown), ms(t, 2500));
            e.handle(Event::Report { volume: 16, mute: false }, ms(t, 3000));
            e.handle(Event::Key(Key::VolumeDown), ms(t, 3300));
            e.handle(Event::Report { volume: 14, mute: false }, ms(t, 3800));
        }
        let t = ms(t0, 40000);
        assert_eq!(e.handle(Event::Key(Key::VolumeUp), t).map(|s| s.volume), Some(16));
        assert_eq!(e.handle(Event::Key(Key::VolumeUp), ms(t, 200)).map(|s| s.volume), Some(18));
        // From 18 nothing is learned: the third press shows 18 dimmed.
        assert_eq!(e.handle(Event::Key(Key::VolumeUp), ms(t, 400)), Some(Show { volume: 18, mute: false, pending: true }));
        e.take_steps_dirty();
        // One combined answer for all three presses: shown, and nothing is learned from it.
        assert_eq!(e.handle(Event::Report { volume: 20, mute: false }, ms(t, 900)).map(|s| s.volume), Some(20));
        assert!(!e.take_steps_dirty());
    }

    #[test]
    fn unanswered_prediction_stays_on_screen() {
        let t0 = Instant::now();
        let mut e = learned(t0);
        let t = ms(t0, 20000);
        assert_eq!(e.handle(Event::Key(Key::VolumeUp), t).map(|s| s.volume), Some(16));
        assert_eq!(e.tick(ms(t, 1500)), None, "do not jump back to the old level");
        // The next press starts again from the last confirmed level.
        assert_eq!(e.handle(Event::Key(Key::VolumeUp), ms(t, 3000)).map(|s| s.volume), Some(16));
    }

    #[test]
    fn no_prediction_while_muted_or_for_mute_key() {
        let t0 = Instant::now();
        let mut e = learned(t0);
        let t = ms(t0, 20000);
        assert_eq!(e.handle(Event::Key(Key::Mute), t).map(|s| s.pending), Some(true));
        assert_eq!(e.handle(Event::Report { volume: 14, mute: true }, ms(t, 600)), Some(Show { volume: 14, mute: true, pending: false }));
        assert_eq!(e.handle(Event::Key(Key::VolumeUp), ms(t, 2000)).map(|s| s.pending), Some(true));
    }

    #[test]
    fn steps_follow_the_identified_soundbar() {
        let t0 = Instant::now();
        let mut e = learned(t0);
        // Learned before identification; kept when the name becomes known.
        let polk = crate::learn::Name::new("Polk_S4", true).unwrap();
        e.set_soundbar(polk, crate::learn::Steps::default());
        assert_eq!(e.steps().predict(14, crate::learn::Dir::Up), Some(16));
        // A different soundbar brings its own (empty) table.
        let other = crate::learn::Name::new("HW-Q990B", true).unwrap();
        e.set_soundbar(other, crate::learn::Steps::default());
        assert_eq!(e.handle(Event::Key(Key::VolumeUp), ms(t0, 30000)).map(|s| s.pending), Some(true));
    }

    #[test]
    fn missed_reply_does_not_teach_a_double_step() {
        // Seen on the C3: a press from 8 got no reply (soundbar went to 6),
        // the next press from "8" got 4, and 8 down -> 4 was learned.
        let t0 = Instant::now();
        let mut e = ready(t0, 8);
        e.handle(Event::CecLog(true), t0);
        let t = ms(t0, 5000);
        e.handle(Event::Key(Key::VolumeDown), t);
        e.tick(ms(t, 1500));
        e.handle(Event::Key(Key::VolumeDown), ms(t, 3000));
        e.handle(Event::Report { volume: 4, mute: false }, ms(t, 3600));
        assert!(e.steps().is_empty(), "nothing learned across a missed reply");
        // After that confirmed report, learning works again.
        e.handle(Event::Key(Key::VolumeUp), ms(t, 6000));
        e.handle(Event::Report { volume: 6, mute: false }, ms(t, 6600));
        assert_eq!(e.steps().len(), 1);
    }
}

