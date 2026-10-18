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
//! APIs fenced by the aterm-census guard `input-probe`) answers "is anyone at this Mac", not "did
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
//! owner's included, and the shift+tabs the Claude Code lights press on their
//! own after a person's click) is never stamped: that is the whole point.
//!
//! **What it never does**: gate a PERSON's byte. The seam records `src` for
//! audit and never branches its egress on it (`seam_egress` takes no source);
//! this stamp is such a record, read by the `status` and `text --json` verbs
//! and by a CONTROLLER's own fence: `key|send
//! if-human=<n>` (`crate::control_input::InputFence`) asks, under the terminal
//! lock its write is made under, that ITS OWN write be refused (`OK skipped
//! reason=person`) when the count moved since the read that returned `n`
//! ([`HumanInputStamp::seq`]). The only byte the stamp ever holds back is the
//! controller's that asked for it.
//!
//! **The count** ([`HumanInputStamp::seq`], `status human_seq=`, `text
//! --json`'s `"human_seq"`): how many person gestures have reached the
//! session. A write fenced on it (`key|send if-human=<n>`, checked under the
//! terminal lock with the other fences, `crate::control_input::InputFence`)
//! is refused `OK skipped reason=person` when a person has keyed since the
//! read that returned `n` — the round trip between a supervisor's read and
//! its key that the age alone cannot close (R3b of the question answer's
//! critique, 2026-09-25; built 2026-09-27).
//!
//! **What that fence is not** (the residual, said plainly): atomic with the
//! person's own write. A person's gesture is counted here, on the main thread,
//! BEFORE its bytes are written, but that write does not take the terminal
//! lock (the key encoder reads the lock-free `ModeMirror`; a paste or an
//! ordered write goes through the FIFO writer). So a person's gesture counted
//! after the fence's check and written before the controller's write — inside
//! that one lock hold, microseconds wide — still lands ahead of it. What the
//! fence does close is the round trip it replaces: a controller that read the
//! count on one request and wrote on the next.
//!
//! **The plain-Enter marker** ([`HumanInputStamp::probe`], 2026-10-16): the
//! number of the last gesture that was a plain Enter key press
//! ([`Gesture::PlainEnter`]). Equal to the count, the LAST gesture counted was
//! that Enter — what the launch witness (`crate::launch_witness`) asks at the
//! shell's command-start mark: the key that accepted the command line, with
//! nothing typed after it.
//!
//! Lock-free: one monotonic clock read ([`crate::metrics::now_us`]), one
//! relaxed atomic store and one release add per person gesture — the stamp
//! before the count, so a read of the count (an acquire) and then the age
//! never pairs a counted gesture with an age from before it — one load per
//! read, and for a plain Enter one release store more. The stamp lives in
//! this process: a session adopted across a seamless update reads "never"
//! until a person keys it again.

use std::sync::atomic::{AtomicU64, Ordering};

use crate::input::InputEvent;

/// When a person last gave this session input — [`crate::metrics::now_us`]
/// plus one (so `0` stays "never"), or `0` when no person ever has since the
/// session started in this process — and how many gestures that has been
/// ([`Self::seq`]). Lives on [`crate::SessionCtx`].
#[derive(Debug, Default)]
pub(crate) struct HumanInputStamp {
    at: AtomicU64,
    count: AtomicU64,
    /// The number of the last gesture that was [`Gesture::PlainEnter`]; `0`
    /// for none.
    last_plain_enter: AtomicU64,
}

/// What kind of gesture [`HumanInputStamp::note`] counts, for the
/// plain-Enter marker. Passed down from the seam that saw the event
/// (`crate::app_input::note_person`), never guessed from bytes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Gesture {
    /// A press of the named Enter key with no modifier held: not a repeat,
    /// not keypad Enter, not committed text, a paste or a `[key_sequences]`
    /// chord that carries a CR. A lock key's state (Caps Lock, Num Lock),
    /// which a windowed key press carries in its modifiers, is no modifier
    /// held: it does not change which key accepted the line (as
    /// `crate::app_input`'s plain Enter reads it).
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

/// The person count with what its last gesture was ([`HumanInputStamp::probe`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct PersonProbe {
    /// [`HumanInputStamp::seq`].
    pub(crate) seq: u64,
    /// Gesture `seq` was a [`Gesture::PlainEnter`] (never for `seq` 0).
    pub(crate) plain_enter: bool,
}

impl HumanInputStamp {
    /// A person's gesture of kind `gesture` reached the session at `now_us`.
    /// `true` when it opens a burst: the first ever, or the first after
    /// [`crate::session_timeline::HUMAN_BURST_GAP_MS`] without one.
    pub(crate) fn note(&self, now_us: u64, gesture: Gesture) -> bool {
        // THE STAMP BEFORE THE COUNT, the count's add a Release and
        // [`Self::seq`]'s load an Acquire: a reader that sees this gesture
        // counted sees its stamp too. Counted first (as it was until
        // 2026-09-29), a read landing between the two stores paired the new
        // count with the OLD age — "quiet" — and a key fenced on that count
        // passed the fence the gesture should have moved.
        let before = self.at.swap(now_us.saturating_add(1), Ordering::Relaxed);
        let n = self.count.fetch_add(1, Ordering::Release).wrapping_add(1);
        // The marker takes the number THIS add returned, after it: equal to
        // the count only when gesture `n` itself was a plain Enter, however
        // two notes interleave. A probe between the add and this store sees
        // the older marker, and says "not a plain Enter".
        if gesture == Gesture::PlainEnter {
            self.last_plain_enter.store(n, Ordering::Release);
        }
        before.checked_sub(1).is_none_or(|at| {
            now_us.saturating_sub(at) / 1000 >= crate::session_timeline::HUMAN_BURST_GAP_MS
        })
    }

    /// The raw stamp: equal to an earlier reading exactly when no person's
    /// gesture has reached the session since that reading — what the tests
    /// pair with [`Self::seq`].
    #[cfg(test)]
    pub(crate) fn last(&self) -> u64 {
        self.at.load(Ordering::Relaxed)
    }

    /// How many person gestures have reached the session in this process
    /// (`human_seq=`): equal to an earlier reading exactly when none has
    /// since — what a write fenced `if-human=<n>` is checked against. Read
    /// it BEFORE the age ([`Self::ms_since`], [`Self::wire`]) whenever the two
    /// go out together: the Acquire here makes every stamp up to the count
    /// read visible to that later load, so the pair never says "quiet" about
    /// a gesture the count already holds ([`Self::note`]).
    pub(crate) fn seq(&self) -> u64 {
        self.count.load(Ordering::Acquire)
    }

    /// [`Self::seq`] and whether its last gesture was a plain Enter: the
    /// count first, then the marker, both acquires. Equal, gesture `seq` was
    /// a [`Gesture::PlainEnter`] (the marker only ever holds the number of a
    /// plain Enter); a probe racing a note reads them unequal, never a false
    /// "plain Enter".
    pub(crate) fn probe(&self) -> PersonProbe {
        let seq = self.count.load(Ordering::Acquire);
        let enter = self.last_plain_enter.load(Ordering::Acquire);
        PersonProbe {
            seq,
            plain_enter: seq > 0 && enter == seq,
        }
    }

    /// Milliseconds from the last person's gesture to `now_us`; `None` when
    /// no person has given the session input. A stamp later than `now_us`
    /// (two clock reads racing) reads as `0`.
    pub(crate) fn ms_since(&self, now_us: u64) -> Option<u64> {
        let at = self.at.load(Ordering::Relaxed).checked_sub(1)?;
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
            layout_text: None,
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
        s.note(5_000_000, Gesture::Other);
        assert_eq!(s.ms_since(5_000_000), Some(0));
        assert_eq!(s.ms_since(17_345_678), Some(12_345));
        assert_eq!(s.wire(17_345_678), "12345");
        assert_eq!(s.ms_since(15_000_000), Some(10_000));
        assert_eq!(s.ms_since(4_000_000), Some(0), "a racing read never wraps");
        // A stamp at the clock's origin is still a stamp, not "never".
        s.note(0, Gesture::Other);
        assert_eq!(s.ms_since(2_000), Some(2));
    }

    /// A COUNT NEVER OUTRUNS ITS STAMP: a reader that loads the count and
    /// then the stamp — the order `status` and `text --json` read them in —
    /// while a person's gestures land never sees gesture `n` counted beside
    /// the stamp of an earlier one. A torn pair is a `human_seq` that holds
    /// the gesture beside a `human_ms` that says the person is quiet: the
    /// supervisor keys on it, fenced `if-human=` on that very count, and the
    /// fence passes. Gesture `k` lands at `k` ms, so gesture `n`'s stamp is
    /// `n * 1000 + 1`, and a later one only larger. (Counted before it was
    /// stamped, as until 2026-09-29, the pair tore on a plain interleaving,
    /// whatever the memory model: this reader, spinning beside the writer,
    /// is how it shows.)
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

    /// The first gesture and the first after
    /// [`crate::session_timeline::HUMAN_BURST_GAP_MS`] of quiet open a burst
    /// (the `human` event), gestures inside one do not — however long it
    /// lasts; an unstamped session reads `-`, never `0`.
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
        // A slow typist — a key every few seconds for ten minutes — is still
        // the one burst: one row, not one a keystroke (the ring it shares).
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
        // The clock's first microsecond is still a stamp, not "never".
        let early = HumanInputStamp::default();
        assert!(early.note(0, Gesture::Other));
        assert!(!early.note(3_000, Gesture::Other));
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

    fn enter(mods: Modifiers, event_type: KeyEventType) -> InputEvent {
        InputEvent::Key {
            key: Key::Named(NamedKey::Enter),
            mods,
            base_layout: None,
            layout_text: None,
            event_type,
        }
    }

    /// THE PLAIN-ENTER MARKER, through the real App input seam
    /// ([`crate::App::input`], the headless App's session 0): a person's
    /// Enter press marks the count's last gesture as a plain Enter. Each
    /// gesture that may carry a CR another way — Shift+Enter, a held Enter's
    /// repeat, keypad Enter, committed text, a paste, a `[key_sequences]`
    /// chord — and a letter is counted after a plain Enter and leaves the
    /// probe saying "not a plain Enter"; a controller's Enter neither counts
    /// nor marks. A lock key's state (Caps Lock, Num Lock: a windowed press
    /// carries it in its modifiers) leaves an Enter plain, and Shift+Enter
    /// not.
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
