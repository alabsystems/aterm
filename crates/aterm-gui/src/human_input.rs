// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE PERSON'S STAMP: when a PERSON last gave a session input, per session
//! — `status human_ms=<ms|->` and `text --json`'s `"human_ms"`.
//!
//! **Why it exists** (the critique of 2026-09-25, R1, on the harness that
//! answers a question dialog with its recommended option): the supervisor
//! presses keys into a dialog a person may be navigating at that moment, and
//! nothing the screen shows says so. A person's `↓` into Claude Code's
//! question dialog echoes nothing (the dialog hides the cursor), so the
//! cursor effects' typing momentum — the only presence signal the harness had
//! (`await momentum`) — never latched for it, and it was kept per WINDOW, not
//! per session. The OS-wide HID idle (`user_input_recent`, the updater's; its
//! APIs fenced by `grep_guard` B9) answers "is anyone at this Mac", not "did
//! anyone key THIS session". So the server stamps, per session, the instant a
//! [`crate::input::Source::Human`] gesture addressed to it passed the App
//! input seam ([`crate::App::input_to_session`]) — before any filter that
//! classifies a key as navigation, so an arrow, a digit, Enter, Tab, Esc and
//! a paste all count — and a client asks how long ago that was.
//!
//! **What counts** ([`is_person_gesture`]): a key press or repeat, committed
//! text, a `[key_sequences]` chord, a paste, a mouse button, a drag, a wheel
//! notch, and a scroll of the view — typing, and the pointer a person reads
//! back or selects with, which a supervisor's keys would undo (they snap the
//! view and clear the selection). NOT a key release, a hover (buttonless
//! motion is not intent), a focus change or a resize. An IME composition
//! stamps too (`App::on_ime_preedit`): it sends no byte and moves no screen
//! generation, yet it is a person typing.
//!
//! **The burst** ([`HumanInputStamp::note`]): the first gesture ever, and the
//! first after [`crate::session_timeline::HUMAN_BURST_GAP_MS`] without one,
//! opens a burst — the one `human` timeline row and `EVENT <local> human`
//! push per burst (`crate::app_input::note_person`), so a person at work is
//! one row however long they stay. A controller's input
//! ([`crate::input::Source::Controller`] — every control verb, the session
//! owner's included) is never stamped: that is the whole point.
//!
//! **What it never does**: gate a byte. The seam records `src` for audit and
//! never branches its egress on it (`bytes_human_eq_controller`); this stamp
//! is such a record, read by the `status` and `text --json` verbs — and by
//! the Claude Code lights (`crate::claude_lights`), which type nothing more
//! into a composer a person has touched since aterm's own last write
//! ([`HumanInputStamp::last`]).
//!
//! Lock-free: one monotonic clock read ([`crate::metrics::now_us`]) and one
//! relaxed atomic store per person gesture, one load per read. The stamp
//! lives in this process: a session adopted across a seamless update reads
//! "never" until a person keys it again.

use std::sync::atomic::{AtomicU64, Ordering};

use crate::input::InputEvent;

/// When a person last gave this session input: [`crate::metrics::now_us`]
/// plus one (so `0` stays "never"), or `0` when no person ever has since the
/// session started in this process. Lives on [`crate::SessionCtx`].
#[derive(Debug, Default)]
pub(crate) struct HumanInputStamp(AtomicU64);

impl HumanInputStamp {
    /// A person's gesture reached the session at `now_us`. `true` when it
    /// opens a burst: the first ever, or the first after
    /// [`crate::session_timeline::HUMAN_BURST_GAP_MS`] without one.
    pub(crate) fn note(&self, now_us: u64) -> bool {
        let before = self.0.swap(now_us.saturating_add(1), Ordering::Relaxed);
        before.checked_sub(1).is_none_or(|at| {
            now_us.saturating_sub(at) / 1000 >= crate::session_timeline::HUMAN_BURST_GAP_MS
        })
    }

    /// The raw stamp: equal to an earlier reading exactly when no person's
    /// gesture has reached the session since that reading.
    pub(crate) fn last(&self) -> u64 {
        self.0.load(Ordering::Relaxed)
    }

    /// Milliseconds from the last person's gesture to `now_us`; `None` when
    /// no person has given the session input. A stamp later than `now_us`
    /// (two clock reads racing) reads as `0`.
    pub(crate) fn ms_since(&self, now_us: u64) -> Option<u64> {
        let at = self.0.load(Ordering::Relaxed).checked_sub(1)?;
        Some(now_us.saturating_sub(at) / 1000)
    }

    /// The wire value: the milliseconds, or `-` for never
    /// (`status human_ms=`).
    pub(crate) fn wire(&self, now_us: u64) -> String {
        self.ms_since(now_us)
            .map_or_else(|| "-".to_string(), |ms| ms.to_string())
    }
}

/// Whether `ev` is a person's deliberate gesture toward a session (module
/// header, "What counts"): everything but a key release, bare pointer motion,
/// a focus change and a geometry change.
pub(crate) fn is_person_gesture(ev: &InputEvent) -> bool {
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
        // A drag (a button held: `buttons` 3 is none) selects; a hover reads.
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
            event_type,
        }
    }

    /// Never until a gesture, then the time since it, whole milliseconds;
    /// a clock read that raced behind the stamp reads 0, never wraps.
    #[test]
    fn the_stamp_reads_never_then_the_time_since() {
        let s = HumanInputStamp::default();
        assert_eq!(s.ms_since(5_000_000), None);
        assert_eq!(s.wire(5_000_000), "-");
        s.note(5_000_000);
        assert_eq!(s.ms_since(5_000_000), Some(0));
        assert_eq!(s.ms_since(17_345_678), Some(12_345));
        assert_eq!(s.wire(17_345_678), "12345");
        assert_eq!(s.ms_since(15_000_000), Some(10_000));
        assert_eq!(s.ms_since(4_000_000), Some(0), "a racing read never wraps");
        // A stamp at the clock's origin is still a stamp, not "never".
        s.note(0);
        assert_eq!(s.ms_since(2_000), Some(2));
    }

    /// The first gesture and the first after
    /// [`crate::session_timeline::HUMAN_BURST_GAP_MS`] of quiet open a burst
    /// (the `human` event), gestures inside one do not — however long it
    /// lasts; an unstamped session reads `-`, never `0`.
    #[test]
    fn a_persons_burst_opens_after_the_gap_without_input() {
        let gap_us = crate::session_timeline::HUMAN_BURST_GAP_MS * 1000;
        let s = HumanInputStamp::default();
        assert_eq!(s.wire(5_000_000), "-");
        assert!(s.note(5_000_000), "the first gesture opens a burst");
        assert!(!s.note(5_400_000), "inside the burst");
        assert!(
            !s.note(5_400_000 + gap_us - 1_000),
            "a gesture just inside the gap after the last is inside it"
        );
        // A slow typist — a key every few seconds for ten minutes — is still
        // the one burst: one row, not one a keystroke (the ring it shares).
        let mut t = 5_400_000 + gap_us - 1_000;
        for _ in 0..200 {
            t += 3_000_000;
            assert!(!s.note(t), "at {t}");
        }
        assert!(
            s.note(t + gap_us),
            "a full gap without input opens the next"
        );
        assert_eq!(s.ms_since(t + gap_us + 101_000), Some(101));
        // The clock's first microsecond is still a stamp, not "never".
        let early = HumanInputStamp::default();
        assert!(early.note(0));
        assert!(!early.note(3_000));
    }

    /// THE SEAM STAMPS A PERSON AND NEVER A CONTROLLER: the same arrow from a
    /// control verb ([`Source::Controller`]) leaves session 0 unstamped, and
    /// so do a person's key release and a focus change; the person's arrow
    /// stamps it. Driven through the real App input seam
    /// ([`crate::App::input`]), on the headless App's session 0.
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

    /// An arrow, a digit, Enter and a paste are a person's gesture — the
    /// keys that navigate a dialog and echo nothing are exactly the ones the
    /// momentum signal missed — and a release, a hover, a focus change and a
    /// resize are not.
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
}
