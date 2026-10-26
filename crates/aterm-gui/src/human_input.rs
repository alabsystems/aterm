// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tracks when a person last gave a session input (`human_ms=` and `human_seq=`).

use std::sync::atomic::{AtomicI64, AtomicU64, Ordering};

use crate::input::InputEvent;

/// When a person last gave this session input ([`crate::metrics::now_us`] plus one, so `0` is "never" in this process) and how many gestures that has been ([`Self::seq`]).
#[derive(Debug)]
pub(crate) struct HumanInputStamp {
    at: AtomicU64,
    count: AtomicU64,
    /// THE NUMBER OF THE LAST GESTURE THAT REACHED THE PROGRAM,
    /// `0` for none: a key that encodes to
    /// bytes, text, a paste, a wheel the program is sent (a report or
    /// alternate scroll), a mouse press under tracking, a drag the tracking
    /// mode reports ([`reaches_program`]).
    reached: AtomicU64,
    /// THE NUMBER OF THE LAST GESTURE THAT OPENED A BURST,
    /// `0` for none: published after the count and
    /// before [`Self::reached`], so a reading that holds a reaching
    /// gesture's number holds whether it opened a burst.
    burst: AtomicU64,
    /// The same moment on the clock that counts through sleep
    /// (`aterm_uds::process::continuous_ms`) plus one, `0` for never or no
    /// such clock: what the survey's `human_ms=` is taken on
    /// ([`Self::age_ms`]).
    at_continuous: AtomicU64,
    /// The number of the last gesture that was [`Gesture::PlainEnter`]; `0`
    /// for none.
    last_plain_enter: AtomicU64,
    /// THE OUTGOING PROCESS'S AGE OF THE LAST GESTURE, in milliseconds plus
    /// one (so `0` stays "none carried"), as of [`Self::carried_at`]
    /// ([`Self::carry`]): read only while no gesture of this process is
    /// stamped (`at` is `0`), every one of which is later.
    carried_ms: AtomicU64,
    /// When the carried age was taken here ([`crate::metrics::now_us`]).
    carried_at: AtomicU64,
    /// WHEN THE HOST BEGAN SERVING THE SESSION, on this process's clock
    /// ([`crate::metrics::now_us`], microseconds): its start here, or the
    /// Commit a seamless successor adopted it at — earlier than this
    /// process's own start (negative) where the outgoing process said how
    /// long it served the shell with no person's gesture ([`Self::serve_from`]).
    served_from_us: AtomicI64,
}

impl Default for HumanInputStamp {
    fn default() -> Self {
        Self {
            at: AtomicU64::new(0),
            count: AtomicU64::new(0),
            reached: AtomicU64::new(0),
            burst: AtomicU64::new(0),
            at_continuous: AtomicU64::new(0),
            last_plain_enter: AtomicU64::new(0),
            carried_ms: AtomicU64::new(0),
            carried_at: AtomicU64::new(0),
            served_from_us: AtomicI64::new(
                i64::try_from(crate::metrics::now_us()).unwrap_or(i64::MAX),
            ),
        }
    }
}

/// What kind of gesture [`HumanInputStamp::note_reaching`] counts, for the plain-Enter marker.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Gesture {
    /// A press of the named Enter key with no modifier held: not a repeat, not keypad Enter, not committed text, a paste or a `[key_sequences]` chord that carries a CR.
    PlainEnter,
    /// Every other gesture.
    Other,
}

impl Gesture {
    /// The class of `ev`, a person's gesture ([`is_person_gesture`]).
    pub(crate) fn of(ev: &InputEvent) -> Self {
        use aterm_types::keyboard::{Key, KeyEventType, Modifiers, NamedKey};
        match ev {
            InputEvent::Key {
                key: Key::Named(NamedKey::Enter),
                mods,
                event_type: KeyEventType::Press,
                ..
            } if mods
                .difference(Modifiers::CAPS_LOCK | Modifiers::NUM_LOCK)
                .is_empty() =>
            {
                Self::PlainEnter
            }
            _ => Self::Other,
        }
    }
}

/// What [`HumanInputStamp::note_reaching`] counted: whether the gesture opens a burst, and its number (the [`HumanInputStamp::seq`] it made).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Noted {
    /// It opens a burst ([`HumanInputStamp::note_reaching`]).
    pub(crate) burst: bool,
    /// Its number.
    pub(crate) seq: u64,
}

/// The person's count and its last program-bound gesture, read together ([`HumanInputStamp::counts`]).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct PersonCounts {
    /// How many gestures are counted ([`HumanInputStamp::seq`]), never below `reached`.
    pub(crate) seq: u64,
    /// The number of the last gesture that reached the program; `0` for none.
    pub(crate) reached: u64,
    /// The number of the last gesture that opened a burst; `0` for none.
    pub(crate) burst: u64,
}

/// The person count with what its last gesture was ([`HumanInputStamp::probe`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct PersonProbe {
    /// [`HumanInputStamp::seq`].
    pub(crate) seq: u64,
    /// Gesture `seq` was a [`Gesture::PlainEnter`] (never for `seq` 0).
    pub(crate) plain_enter: bool,
}

impl HumanInputStamp {
    /// [`Self::note_reaching`] for a gesture that reached the program: whether it opens a burst.
    #[cfg(test)]
    pub(crate) fn note(&self, now_us: u64, gesture: Gesture) -> bool {
        self.note_reaching(now_us, gesture, true).burst
    }

    /// A person's gesture of kind `gesture` reached the session at `now_us`, named the last that reached the program ([`Self::counts`]) too when it `reaches` it ([`reaches_program`]): its number, and whether it opens a burst (the first ever, or the first after [`crate::session_timeline::HUMAN_BURST_GAP_MS`] without one).
    pub(crate) fn note_reaching(&self, now_us: u64, gesture: Gesture, reaches: bool) -> Noted {
        self.note_reaching_split(now_us, gesture, reaches, || ())
    }

    /// [`Self::note_reaching`], running `between` once the gesture is counted and before its class is published: the one point another thread can read the two apart, where a test puts a reconciling entry (the production call runs nothing there).
    pub(crate) fn note_reaching_split(
        &self,
        now_us: u64,
        gesture: Gesture,
        reaches: bool,
        between: impl FnOnce(),
    ) -> Noted {
        let before = self.at.swap(now_us.saturating_add(1), Ordering::Relaxed);
        if let Some(ms) = aterm_uds::process::continuous_ms() {
            self.at_continuous
                .store(ms.saturating_add(1), Ordering::Relaxed);
        }
        let n = self.count.fetch_add(1, Ordering::Release).wrapping_add(1);
        let burst = before.checked_sub(1).is_none_or(|at| {
            now_us.saturating_sub(at) / 1000 >= crate::session_timeline::HUMAN_BURST_GAP_MS
        });
        between();
        if burst {
            self.burst.fetch_max(n, Ordering::Release);
        }
        if reaches {
            self.reached.fetch_max(n, Ordering::Release);
        }
        if gesture == Gesture::PlainEnter {
            self.last_plain_enter.store(n, Ordering::Release);
        }
        Noted { burst, seq: n }
    }

    /// THE PERSON'S COUNTS AS ONE READING (rulings T2, T39): how many gestures are counted and the number of the last that reached the program, the count never below that number.
    pub(crate) fn counts(&self) -> PersonCounts {
        let reached = self.reached.load(Ordering::Acquire);
        let burst = self.burst.load(Ordering::Acquire);
        let seq = self.count.load(Ordering::Acquire);
        PersonCounts {
            seq: seq.max(reached).max(burst),
            reached,
            burst,
        }
    }

    /// The raw stamp; the tests pair it with [`Self::seq`].
    #[cfg(test)]
    pub(crate) fn last(&self) -> u64 {
        self.at.load(Ordering::Relaxed)
    }

    /// How many person gestures have reached the session in this process (`human_seq=`): equal to an earlier reading exactly when none has since — what a write fenced `if-human=<n>` is checked against.
    pub(crate) fn seq(&self) -> u64 {
        self.count.load(Ordering::Acquire)
    }

    /// [`Self::seq`] and whether its last gesture was a plain Enter: the count first, then the marker, both acquires.
    pub(crate) fn probe(&self) -> PersonProbe {
        let seq = self.count.load(Ordering::Acquire);
        let enter = self.last_plain_enter.load(Ordering::Acquire);
        PersonProbe {
            seq,
            plain_enter: seq > 0 && enter == seq,
        }
    }

    /// Milliseconds from the last person's gesture to `now_us`; `None` when no person has given the session input — in this process or, carried ([`Self::carry`]), in the one a seamless update adopted it from.
    pub(crate) fn ms_since(&self, now_us: u64) -> Option<u64> {
        if let Some(at) = self.at.load(Ordering::Relaxed).checked_sub(1) {
            return Some(now_us.saturating_sub(at) / 1000);
        }
        let age = self.carried_ms.load(Ordering::Acquire).checked_sub(1)?;
        let at = self.carried_at.load(Ordering::Relaxed);
        Some(age.saturating_add(now_us.saturating_sub(at) / 1000))
    }

    /// THE OUTGOING PROCESS'S STAMP, CARRIED: a person last gave the session input `age_ms` before
    /// `now_us`, in the instance a seamless update adopted it from.
    #[cfg(any(unix, test))]
    pub(crate) fn carry(&self, age_ms: u64, now_us: u64) {
        self.carried_at.store(now_us, Ordering::Relaxed);
        self.carried_ms
            .store(age_ms.saturating_add(1), Ordering::Release);
    }

    /// THE HOST HAS SERVED THE SESSION WITH NO PERSON'S GESTURE: `age_ms` before `now_us` it began,
    /// here or, carried, in the instance a seamless update adopted it from.
    #[cfg(any(unix, test))]
    pub(crate) fn serve_from(&self, age_ms: u64, now_us: u64) {
        let now = i64::try_from(now_us).unwrap_or(i64::MAX);
        let age = i64::try_from(age_ms.saturating_mul(1000)).unwrap_or(i64::MAX);
        self.served_from_us
            .store(now.saturating_sub(age), Ordering::Relaxed);
    }

    /// How long, in milliseconds to `now_us`, the host has served a session no person ever gave input ([`Self::serve_from`]); `None` once one has ([`Self::ms_since`] is `Some`).
    pub(crate) fn unkeyed_ms(&self, now_us: u64) -> Option<u64> {
        if self.ms_since(now_us).is_some() {
            return None;
        }
        let now = i64::try_from(now_us).unwrap_or(i64::MAX);
        let from = self.served_from_us.load(Ordering::Relaxed);
        Some(u64::try_from(now.saturating_sub(from)).unwrap_or(0) / 1000)
    }

    /// [`Self::ms_since`] counted THROUGH SLEEP, as the survey reports it: a laptop that slept since the person's last gesture reads the time that passed.
    pub(crate) fn age_ms(&self, now_us: u64) -> Option<u64> {
        let since = self.ms_since(now_us)?;
        let at = self.at_continuous.load(Ordering::Relaxed).checked_sub(1);
        Some(match (at, aterm_uds::process::continuous_ms()) {
            (Some(at), Some(now)) => now.saturating_sub(at),
            _ => since,
        })
    }

    /// The wire value: the milliseconds, or `-` for never (`status human_ms=`).
    pub(crate) fn wire(&self, now_us: u64) -> String {
        self.ms_since(now_us)
            .map_or_else(|| "-".to_string(), |ms| ms.to_string())
    }
}

#[cfg(unix)]
impl crate::App {
    /// THE OUTGOING PROCESS'S STAMPS, WRITTEN RIGHT BEFORE ITS COMMIT'S ONE WRITE: each carried
    /// session's age of its last person's gesture, in the attempt `nonce`'s person-stamps sidecar
    /// ([`crate::seamless::person_stamps_path`]), after the drain gate's last dispatch of input — a
    /// key the overlap passed through to a shell is in it.
    pub(crate) fn write_person_stamps_at_commit(&self, nonce: &str) -> Option<std::path::PathBuf> {
        let path =
            crate::seamless::person_stamps_path(&crate::seamless::outgoing_manifest_path(nonce)?);
        let live: Vec<u64> = self
            .pending_update_handoff
            .as_ref()?
            .live
            .iter()
            .map(|&(id, _, _)| id)
            .collect();
        let written = self.write_person_stamps_of(&live, path);
        if written.is_some() {
            // The keep seam's second copy (inert in a shipped build), of what the stamps added:
            // after the grant, inside the Commit window, before the Commit that lets the
            // successor read the stamps.
            crate::seamless::keep_attempt(nonce);
        }
        written
    }

    fn write_person_stamps_of(
        &self,
        ids: &[u64],
        path: std::path::PathBuf,
    ) -> Option<std::path::PathBuf> {
        use crate::seamless::PersonStamp;
        let now_us = crate::metrics::now_us();
        let stamps: Vec<(u64, PersonStamp)> = ids
            .iter()
            .filter_map(|&id| {
                let stamp = &self.pool.get(id)?.ctx.human_input;
                let carried = match stamp.ms_since(now_us) {
                    Some(age) => PersonStamp::Keyed(age),
                    None => PersonStamp::Unkeyed(stamp.unkeyed_ms(now_us)?),
                };
                Some((id, carried))
            })
            .collect();
        if stamps.is_empty() {
            return None;
        }
        if crate::seamless::write_person_stamps(&path, &stamps) {
            Some(path)
        } else {
            aterm_log::warn!(
                "update apply: the person's stamps could not be written for the successor; it \
                 reads the {} handed session(s) as never keyed since its Commit",
                stamps.len()
            );
            None
        }
    }

    /// THE SUCCESSOR TAKES THE OUTGOING PROCESS'S STAMPS AT ITS COMMIT: every adopted shell starts
    /// serving here, at the Commit — the outgoing process served it until now, and only its own row
    /// says how — then each one the sidecar names by its outgoing id carries its keyed age
    /// ([`HumanInputStamp::carry`]) or how long it was served unkeyed
    /// ([`HumanInputStamp::serve_from`]).
    pub(crate) fn take_carried_person_stamps(&mut self) {
        let now_us = crate::metrics::now_us();
        for session in self.pool.iter() {
            if session.handoff_local_id.is_some() {
                session.ctx.human_input.serve_from(0, now_us);
            }
        }
        let Some(path) = self.handoff_person_stamps.take() else {
            return;
        };
        let Some(dir) = crate::control_auth::socket_dir() else {
            return;
        };
        let stamps = crate::seamless::take_person_stamps(&path, &dir);
        let mut carried = 0_usize;
        for (named, stamp) in stamps {
            if let Some(session) = self
                .pool
                .iter()
                .find(|session| session.handoff_local_id == Some(named))
            {
                match stamp {
                    crate::seamless::PersonStamp::Keyed(age_ms) => {
                        session.ctx.human_input.carry(age_ms, now_us);
                    }
                    crate::seamless::PersonStamp::Unkeyed(age_ms) => {
                        session.ctx.human_input.serve_from(age_ms, now_us);
                    }
                }
                carried += 1;
            }
        }
        if carried > 0 {
            aterm_log::info!(
                "update apply: carried the person's stamps of {carried} adopted session(s)"
            );
        }
    }
}

/// Whether a person's gesture `ev` ([`is_person_gesture`]) reaches the program, as the session's lock-free mode mirror says before the seam routes it: a key press or repeat that encodes to bytes (under the mirror's keyboard mode), text that encodes to bytes (the seam's own [`crate::keymap::encode_committed_text`]: under kitty's report-all-keys mode a control character encodes to nothing), a key sequence, a paste that writes bytes (framed by its own answer or, for one that reads the live mode, the mirror's DEC 2004, then [`aterm_core::terminal::Terminal::paste_sends_bytes`], the formatter's own rule: bracketed always, unbracketed when a character survives the sanitizer); a wheel the seam sends the program bytes for (its route, [`crate::input::wheel_route_mirrored`], then the seam's own rule on that route: a report under a tracking mode that reports wheels, [`aterm_core::terminal::ModeMirror::reports_wheel`], so not X10; or alternate scroll's arrows, [`crate::input::alt_scroll_arrow`], so only a vertical wheel, whose arrow encodes to bytes, on a platform that scrolls a detent some lines, [`crate::input::alt_scroll_sends`] on `platform`, the wheel rule the seam scrolls by: [`crate::input::WheelPlatform`]), a mouse press under mouse tracking, and a drag the tracking mode reports (button-event or any-event tracking, [`aterm_core::terminal::ModeMirror::reports_motion`], the encoder's own rule: X10 and normal tracking report no motion).
pub(crate) fn reaches_program(
    ev: &InputEvent,
    mirror: &aterm_core::terminal::ModeMirror,
    platform: crate::input::WheelPlatform,
) -> bool {
    match ev {
        InputEvent::Key {
            key,
            mods,
            base_layout,
            layout_text,
            event_type,
        } => {
            !matches!(event_type, aterm_types::keyboard::KeyEventType::Release)
                && !aterm_types::keyboard::encode_key_with_text(
                    key,
                    *mods,
                    mirror.keyboard_mode(),
                    *event_type,
                    *base_layout,
                    *layout_text,
                )
                .is_empty()
        }
        InputEvent::Text(t) => {
            !crate::keymap::encode_committed_text(t, mirror.keyboard_mode()).is_empty()
        }
        InputEvent::KeySequence(b) => !b.is_empty(),
        InputEvent::Paste(text, framing) => aterm_core::terminal::Terminal::paste_sends_bytes(
            text,
            framing.bracketed(mirror.bracketed_paste()),
        ),
        InputEvent::Wheel {
            dir,
            lines,
            detent,
            mods,
            ..
        } => match crate::input::wheel_route_mirrored(mirror, *mods) {
            crate::input::WheelRoute::Report => mirror.reports_wheel(),
            crate::input::WheelRoute::AltScroll => {
                crate::input::alt_scroll_arrow(*dir, mirror.keyboard_mode())
                    .is_some_and(|arrow| !arrow.is_empty())
                    && crate::input::alt_scroll_sends(platform, *lines, *detent)
            }
            crate::input::WheelRoute::Viewport => false,
        },
        InputEvent::MouseButton { pressed, .. } => *pressed && mirror.mouse_tracking_enabled(),
        InputEvent::MouseMove { buttons, .. } => *buttons != 3 && mirror.reports_motion(*buttons),
        InputEvent::ScrollView(_)
        | InputEvent::Resize { .. }
        | InputEvent::ResizeWindowPx { .. }
        | InputEvent::Focus(_) => false,
    }
}

/// Whether `ev` is a person's deliberate gesture toward a session: everything but a key or button
/// release, bare pointer motion, a focus change and a geometry change.
pub(crate) const fn is_person_gesture(ev: &InputEvent) -> bool {
    match ev {
        InputEvent::Key { event_type, .. } => {
            !matches!(event_type, aterm_types::keyboard::KeyEventType::Release)
        }
        InputEvent::Text(_)
        | InputEvent::KeySequence(_)
        | InputEvent::Paste(..)
        | InputEvent::Wheel { .. }
        | InputEvent::ScrollView(_) => true,
        InputEvent::MouseButton { pressed, .. } => *pressed,
        InputEvent::MouseMove { buttons, .. } => *buttons != 3,
        InputEvent::Resize { .. } | InputEvent::ResizeWindowPx { .. } | InputEvent::Focus(_) => {
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aterm_types::keyboard::{Key, KeyEventType, Modifiers, NamedKey};

    fn key(event_type: KeyEventType) -> InputEvent {
        InputEvent::Key {
            key: Key::Named(NamedKey::ArrowDown),
            mods: Modifiers::empty(),
            base_layout: None,
            layout_text: None,
            event_type,
        }
    }

    #[test]
    fn the_stamp_reads_never_then_the_time_since() {
        let s = HumanInputStamp::default();
        assert_eq!(s.ms_since(5_000_000), None);
        assert_eq!(s.wire(5_000_000), "-");
        s.note(5_000_000, Gesture::Other);
        assert_eq!(s.ms_since(5_000_000), Some(0));
        assert_eq!(s.ms_since(17_345_678), Some(12_345));
        assert_eq!(s.wire(17_345_678), "12345");
        assert_eq!(s.ms_since(15_000_000), Some(10_000));
        assert_eq!(s.ms_since(4_000_000), Some(0), "a racing read never wraps");
        s.note(0, Gesture::Other);
        assert_eq!(s.ms_since(2_000), Some(2));
    }

    #[test]
    fn a_carried_stamp_reads_its_age_on_until_a_gesture_here() {
        let s = HumanInputStamp::default();
        s.carry(90_000, 5_000_000);
        assert_eq!(s.ms_since(5_000_000), Some(90_000));
        assert_eq!(s.wire(15_000_000), "100000");
        assert_eq!(
            s.ms_since(4_000_000),
            Some(90_000),
            "a racing read never wraps"
        );
        assert_eq!(s.seq(), 0, "the count is this process's");
        s.note(20_000_000, Gesture::Other);
        assert_eq!(s.ms_since(21_000_000), Some(1_000), "the gesture here");
        s.carry(600_000, 21_000_000);
        s.serve_from(600_000, 21_000_000);
        assert_eq!(s.seq(), 1, "the local gesture stays counted");
        assert_eq!(s.ms_since(21_000_000), Some(1_000), "the local stamp wins");
        assert_eq!(s.unkeyed_ms(21_000_000), None, "keyed here");
        assert_eq!(HumanInputStamp::default().ms_since(5_000_000), None);
    }

    #[test]
    fn a_read_that_sees_a_gesture_counted_sees_its_stamp() {
        use std::sync::Arc;
        use std::sync::atomic::AtomicBool;
        const GESTURES: u64 = 200_000;
        let stamp = Arc::new(HumanInputStamp::default());
        let done = Arc::new(AtomicBool::new(false));
        let writer = {
            let stamp = Arc::clone(&stamp);
            let done = Arc::clone(&done);
            std::thread::spawn(move || {
                for k in 1..=GESTURES {
                    stamp.note(k * 1000, Gesture::Other);
                }
                done.store(true, Ordering::Release);
            })
        };
        let (mut reads, mut torn) = (0_u64, 0_u64);
        while !done.load(Ordering::Acquire) {
            let n = stamp.seq();
            let at = stamp.last();
            if n > 0 && at < n * 1000 + 1 {
                torn += 1;
            }
            reads += 1;
        }
        writer.join().expect("the writer");
        assert_eq!(
            torn, 0,
            "{torn} of {reads} reads paired a counted gesture with an older stamp"
        );
        assert_eq!(stamp.seq(), GESTURES);
        assert_eq!(stamp.last(), GESTURES * 1000 + 1);
    }

    #[test]
    fn a_persons_burst_opens_after_the_gap_without_input() {
        let gap_us = crate::session_timeline::HUMAN_BURST_GAP_MS * 1000;
        let s = HumanInputStamp::default();
        assert_eq!(s.wire(5_000_000), "-");
        assert!(
            s.note(5_000_000, Gesture::Other),
            "the first gesture opens a burst"
        );
        assert!(!s.note(5_400_000, Gesture::Other), "inside the burst");
        assert!(
            !s.note(5_400_000 + gap_us - 1_000, Gesture::Other),
            "a gesture just inside the gap after the last is inside it"
        );
        let mut t = 5_400_000 + gap_us - 1_000;
        for _ in 0..200 {
            t += 3_000_000;
            assert!(!s.note(t, Gesture::Other), "at {t}");
        }
        assert!(
            s.note(t + gap_us, Gesture::Other),
            "a full gap without input opens the next"
        );
        assert_eq!(s.ms_since(t + gap_us + 101_000), Some(101));
        let early = HumanInputStamp::default();
        assert!(early.note(0, Gesture::Other));
        assert!(!early.note(3_000, Gesture::Other));
    }

    #[test]
    fn the_seam_stamps_a_persons_arrow_and_never_a_controllers() {
        use crate::input::Source;
        use crate::{App, WindowId};
        let mut app = App::headless_for_test();
        let stamp = |app: &App| {
            app.pool
                .get(0)
                .expect("session 0")
                .ctx
                .human_input
                .ms_since(crate::metrics::now_us())
        };
        assert_eq!(stamp(&app), None, "a fresh session: never");
        let _ = app.input(WindowId(0), key(KeyEventType::Press), Source::Controller);
        assert_eq!(stamp(&app), None, "a control verb's key is no person's");
        let _ = app.input(WindowId(0), key(KeyEventType::Release), Source::Human);
        let _ = app.input(WindowId(0), InputEvent::Focus(true), Source::Human);
        assert_eq!(
            stamp(&app),
            None,
            "a release and a focus change are no gesture"
        );
        let _ = app.input(WindowId(0), key(KeyEventType::Press), Source::Human);
        assert!(
            stamp(&app).is_some_and(|ms| ms < 60_000),
            "the person's arrow stamped the session: {:?}",
            stamp(&app)
        );
    }

    /// THE PERSON'S STAMPS CROSS A SEAMLESS UPDATE (LC-4 of ):
    /// the outgoing process writes, for the sessions it hands over, the age
    /// of each one's last person's gesture — a stamp it carried itself from
    /// an earlier update included — and for a session nobody keyed, how long
    /// it served it so (`-<age>`, the review's KR-2); the successor's Commit
    /// carries each onto the shell adopted under the id it names and
    /// consumes the sidecar.
    #[cfg(unix)]
    #[test]
    fn the_persons_stamps_cross_a_seamless_update_at_its_commit() {
        use crate::App;
        let dir = crate::control_auth::socket_dir().expect("a private control dir");
        let path = dir.join(format!(
            "seamless-{}-{}.person",
            std::process::id(),
            aterm_uds::rand::hex_token::<8>().expect("a token")
        ));
        let mut outgoing = App::headless_for_test();
        let keyed = *outgoing.pool.sessions.keys().max().expect("session 0");
        let _ = outgoing.insert_logical_window(crate::stub_session(1), 24, 80);
        let untouched = *outgoing
            .pool
            .sessions
            .keys()
            .max()
            .expect("a second session");
        assert_ne!(keyed, untouched);
        let now = crate::metrics::now_us();
        outgoing
            .pool
            .get(keyed)
            .expect("live")
            .ctx
            .human_input
            .carry(90_000, now);
        outgoing
            .pool
            .get(untouched)
            .expect("live")
            .ctx
            .human_input
            .serve_from(600_000, now);
        let written = outgoing
            .write_person_stamps_of(&[keyed, untouched], path.clone())
            .expect("written");
        assert_eq!(written, path);
        let body = std::fs::read_to_string(&path).expect("the sidecar");
        let rows: Vec<&str> = body.lines().collect();
        assert!(
            rows.len() == 2
                && rows[0].starts_with(&format!("{keyed} 9"))
                && rows[1].starts_with(&format!("{untouched} -6")),
            "the keyed age and the unkeyed one: {body:?}"
        );

        let read = |carried: Option<std::path::PathBuf>| {
            let mut successor = App::headless_for_test();
            let _ = successor.insert_logical_window(crate::stub_session(1), 24, 80);
            let ids: Vec<u64> = successor.pool.sessions.keys().copied().collect();
            for (&id, named) in ids.iter().zip([keyed, untouched]) {
                let session = successor.pool.sessions.get_mut(&id).expect("live");
                session.session.handoff_local_id = Some(named);
                session
                    .session
                    .ctx
                    .human_input
                    .serve_from(3_600_000, crate::metrics::now_us());
            }
            successor.handoff_person_stamps = carried;
            successor.take_carried_person_stamps();
            let now = crate::metrics::now_us();
            ids.iter()
                .map(|&id| {
                    let stamp = &successor.pool.get(id).expect("live").ctx.human_input;
                    (stamp.ms_since(now), stamp.unkeyed_ms(now))
                })
                .collect::<Vec<_>>()
        };
        let carried = read(Some(path.clone()));
        assert!(
            matches!(
                carried[..],
                [(Some(keyed), None), (None, Some(served))] if keyed >= 90_000 && served >= 600_000
            ),
            "(keyed, untouched): {carried:?}"
        );
        assert!(!path.exists(), "consumed");
        let none = read(Some(path.clone()));
        assert!(
            matches!(
                none[..],
                [(None, Some(a)), (None, Some(b))] if a < 60_000 && b < 60_000
            ),
            "NEGATIVE CONTROL: no sidecar, never keyed and served since the Commit: {none:?}"
        );
    }

    #[test]
    fn a_never_keyed_session_reads_how_long_it_was_served() {
        let s = HumanInputStamp::default();
        let now = crate::metrics::now_us();
        assert!(
            s.unkeyed_ms(now).is_some_and(|ms| ms < 60_000),
            "since its start"
        );
        s.serve_from(600_000, 5_000_000);
        assert_eq!(s.unkeyed_ms(5_000_000), Some(600_000));
        assert_eq!(s.unkeyed_ms(15_000_000), Some(610_000));
        s.serve_from(0, 5_000_000);
        assert_eq!(
            s.unkeyed_ms(4_000_000),
            Some(0),
            "a racing read never wraps"
        );
        let carried = HumanInputStamp::default();
        carried.carry(90_000, 5_000_000);
        assert_eq!(carried.unkeyed_ms(5_000_000), None, "keyed, carried");
        s.note(6_000_000, Gesture::Other);
        assert_eq!(s.unkeyed_ms(7_000_000), None, "keyed here");
    }

    fn enter(mods: Modifiers, event_type: KeyEventType) -> InputEvent {
        InputEvent::Key {
            key: Key::Named(NamedKey::Enter),
            mods,
            base_layout: None,
            layout_text: None,
            event_type,
        }
    }

    #[test]
    fn only_a_persons_plain_enter_press_marks_the_count() {
        use crate::input::{PasteFraming, Source};
        use crate::{App, WindowId};
        let mut app = App::headless_for_test();
        let probe = |app: &App| app.pool.get(0).expect("session 0").ctx.human_input.probe();
        let at = |seq, plain_enter| PersonProbe { seq, plain_enter };
        let plain = enter(Modifiers::empty(), KeyEventType::Press);
        let letter = InputEvent::Key {
            key: Key::Character('a'),
            mods: Modifiers::empty(),
            base_layout: None,
            layout_text: None,
            event_type: KeyEventType::Press,
        };
        assert_eq!(probe(&app), at(0, false), "a fresh session");
        let _ = app.input(WindowId(0), plain.clone(), Source::Controller);
        assert_eq!(probe(&app), at(0, false), "a control verb's Enter");
        let _ = app.input(WindowId(0), plain.clone(), Source::Human);
        assert_eq!(probe(&app), at(1, true), "the person's Enter");
        let mut seq = 1;
        for (what, lock) in [
            ("Caps Lock", Modifiers::CAPS_LOCK),
            ("Num Lock", Modifiers::NUM_LOCK),
        ] {
            let _ = app.input(WindowId(0), letter.clone(), Source::Human);
            let _ = app.input(WindowId(0), enter(lock, KeyEventType::Press), Source::Human);
            seq += 2;
            assert_eq!(probe(&app), at(seq, true), "Enter with {what} on");
        }
        let others = [
            ("Shift+Enter", enter(Modifiers::SHIFT, KeyEventType::Press)),
            (
                "Shift+Enter with Caps Lock on",
                enter(Modifiers::SHIFT | Modifiers::CAPS_LOCK, KeyEventType::Press),
            ),
            ("a repeat", enter(Modifiers::empty(), KeyEventType::Repeat)),
            (
                "keypad Enter",
                InputEvent::Key {
                    key: Key::Named(NamedKey::NumpadEnter),
                    mods: Modifiers::empty(),
                    base_layout: None,
                    layout_text: None,
                    event_type: KeyEventType::Press,
                },
            ),
            ("text", InputEvent::Text("\r".into())),
            (
                "a paste",
                InputEvent::Paste("claude\r".into(), PasteFraming::AtDrain),
            ),
            ("a chord", InputEvent::KeySequence(b"\r".to_vec())),
            ("a letter", letter),
        ];
        for (what, ev) in others {
            let _ = app.input(WindowId(0), plain.clone(), Source::Human);
            assert_eq!(probe(&app), at(seq + 1, true), "the Enter before {what}");
            let _ = app.input(WindowId(0), ev, Source::Human);
            seq += 2;
            assert_eq!(probe(&app), at(seq, false), "{what}");
        }
    }

    #[test]
    fn navigation_counts_and_releases_hovers_focus_and_geometry_do_not() {
        assert!(is_person_gesture(&key(KeyEventType::Press)));
        assert!(is_person_gesture(&key(KeyEventType::Repeat)));
        assert!(!is_person_gesture(&key(KeyEventType::Release)));
        assert!(is_person_gesture(&InputEvent::Text("1".into())));
        assert!(is_person_gesture(&InputEvent::KeySequence(
            b"\x1b[B".to_vec()
        )));
        assert!(is_person_gesture(&InputEvent::Paste(
            "x".into(),
            crate::input::PasteFraming::AtDrain
        )));
        assert!(!is_person_gesture(&InputEvent::Focus(true)));
        assert!(!is_person_gesture(&InputEvent::Resize {
            rows: 24,
            cols: 80,
            echo_to_window: false
        }));
        assert!(!is_person_gesture(&InputEvent::ResizeWindowPx {
            width: 800,
            height: 600
        }));
    }

    #[test]
    fn a_reading_that_sees_a_gesture_reach_the_program_counts_it() {
        let s = HumanInputStamp::default();
        s.note_reaching(1_000, Gesture::Other, false);
        let mut inside = PersonCounts::default();
        let noted = s.note_reaching_split(2_000, Gesture::Other, true, || inside = s.counts());
        assert_eq!(noted.seq, 2);
        let counts = |seq, reached, burst| PersonCounts {
            seq,
            reached,
            burst,
        };
        assert_eq!(inside, counts(2, 0, 1), "counted first");
        assert_eq!(s.counts(), counts(2, 2, 1));
        s.reached.fetch_max(3, Ordering::Release);
        assert_eq!(s.counts(), counts(3, 3, 1));
        let t = HumanInputStamp::default();
        t.note_reaching(1_000, Gesture::Other, false);
        let late = 1_000 + crate::session_timeline::HUMAN_BURST_GAP_MS * 1000;
        let noted = t.note_reaching_split(late, Gesture::Other, true, || inside = t.counts());
        assert!(noted.burst, "{noted:?}");
        assert_eq!(inside, counts(2, 0, 1), "counted, neither published");
        assert_eq!(t.counts(), counts(2, 2, 2));
    }

    #[test]
    fn a_drag_reaches_the_program_only_where_its_mode_reports_motion() {
        use crate::input::PixelOffset;
        use aterm_core::selection::SelectionSide;
        let drag = InputEvent::MouseMove {
            buttons: 0,
            row: 1,
            col: 4,
            mods: 0,
            side: SelectionSide::Left,
            px_off: PixelOffset::CELL_ORIGIN,
        };
        let press = InputEvent::MouseButton {
            button: aterm_types::mouse::MouseButton::Left,
            pressed: true,
            row: 1,
            col: 4,
            mods: 0,
            click_count: 1,
            side: SelectionSide::Left,
            block: false,
            suppress_copy_on_select: false,
            px_off: PixelOffset::CELL_ORIGIN,
        };
        for (set, motion) in [
            (&b""[..], false),
            (b"\x1b[?9h", false),
            (b"\x1b[?1000h", false),
            (b"\x1b[?1002h", true),
            (b"\x1b[?1003h", true),
        ] {
            let mut term = aterm_core::terminal::Terminal::new(24, 80);
            term.process(set);
            let mirror = term.mode_mirror();
            let mode = String::from_utf8_lossy(set);
            assert_eq!(
                reaches_program(&drag, mirror, crate::input::WheelPlatform::Host),
                motion,
                "a drag under {mode:?}"
            );
            assert_eq!(
                term.encode_mouse_motion(0, 4, 1, 0).is_some(),
                motion,
                "the encoder under {mode:?}"
            );
            assert_eq!(
                reaches_program(&press, mirror, crate::input::WheelPlatform::Host),
                !set.is_empty(),
                "a press under {mode:?}"
            );
        }
    }
}
