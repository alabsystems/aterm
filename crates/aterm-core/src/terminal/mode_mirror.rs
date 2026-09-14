// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Lock-free mirror of the terminal's INPUT-ENCODING modes.
//!
//! [`Terminal::keyboard_mode`](super::Terminal::keyboard_mode) and
//! [`Terminal::mouse_mode`](super::Terminal::mouse_mode) are pure folds of state
//! that only `process()` (on the PTY reader thread) and a handful of host
//! mutators change. The input seam used to take the terminal mutex ON EVERY KEY
//! PRESS solely to read that fold — a second queue position behind the reader's
//! `process()` slice for a value the reader had just finished publishing.
//!
//! The mirror is that fold, published as atomics the seam reads WITHOUT the
//! mutex. It is refreshed under the lock at the end of every `process()` batch
//! and by every host mutator that can move one of the fold's inputs (the
//! constructor, `set_kitty_keyboard_enabled`, `reset`, and both checkpoint
//! hydration paths). A debug obligation at the START of every `process()`
//! batch asserts the mirror still equals the fold, so a mutation site that
//! forgets to refresh fails loudly on the next byte of output in any debug/test
//! build instead of silently encoding against a stale mode.
//!
//! The mirror is shared out as an `Arc` because the seam only ever holds
//! `&Mutex<Terminal>` — nothing inside a `std::sync::Mutex` can be read without
//! locking it — so the atomics must live OUTSIDE the mutex, with the terminal
//! holding one clone (to write through under the lock) and the session's input
//! path holding another (to read). Ordering: `Release` on publish, `Acquire`
//! on read, so a reader that observes the new word also observes everything
//! the batch that produced it wrote before it.

use std::sync::atomic::{AtomicU8, AtomicU16, Ordering};

use aterm_types::keyboard::KeyboardMode;
use aterm_types::mouse::MouseMode;

/// The lock-free input-mode word. See the module docs.
#[derive(Debug, Default)]
pub struct ModeMirror {
    /// [`KeyboardMode`] bits (a `u16` bitflags word).
    keyboard: AtomicU16,
    /// [`MouseMode`] discriminant (`repr(u8)`, `None = 0`).
    mouse: AtomicU8,
}

impl ModeMirror {
    /// The keyboard encoding mode the terminal last published — the same value
    /// `Terminal::keyboard_mode()` returns under the lock, without the lock.
    #[must_use]
    pub fn keyboard_mode(&self) -> KeyboardMode {
        KeyboardMode::from_bits_truncate(self.keyboard.load(Ordering::Acquire))
    }

    /// The mouse tracking mode the terminal last published.
    #[must_use]
    pub fn mouse_mode(&self) -> MouseMode {
        mouse_mode_from_u8(self.mouse.load(Ordering::Acquire))
    }

    /// Lock-free twin of `Terminal::mouse_tracking_enabled()`.
    #[must_use]
    pub fn mouse_tracking_enabled(&self) -> bool {
        self.mouse.load(Ordering::Acquire) != 0
    }

    /// Publish a fresh fold. Called only by the terminal, under its mutex.
    pub(crate) fn publish(&self, keyboard: KeyboardMode, mouse: MouseMode) {
        self.keyboard.store(keyboard.bits(), Ordering::Release);
        self.mouse.store(mouse as u8, Ordering::Release);
    }
}

/// Inverse of `MouseMode as u8`. Unknown discriminants decode as `None`
/// (tracking off) — the fail-safe direction for an encoder gate.
const fn mouse_mode_from_u8(v: u8) -> MouseMode {
    match v {
        1 => MouseMode::Normal,
        2 => MouseMode::ButtonEvent,
        3 => MouseMode::AnyEvent,
        4 => MouseMode::X10,
        _ => MouseMode::None,
    }
}

impl super::Terminal {
    /// The shared lock-free mirror of this terminal's input-encoding modes.
    /// Clone the `Arc` once at session construction; read it on every key
    /// press instead of taking the terminal mutex.
    #[must_use]
    pub fn mode_mirror(&self) -> &std::sync::Arc<ModeMirror> {
        &self.mode_mirror
    }

    /// Re-publish the fold. Every mutation site that can move an input of
    /// `keyboard_mode()` / `mouse_mode()` outside `process()` calls this;
    /// `process_at` calls it once per batch after the parser has run.
    pub(crate) fn refresh_mode_mirror(&self) {
        self.mode_mirror
            .publish(self.keyboard_mode(), self.mouse_mode());
    }

    /// The obligation: the published word equals the live fold. Asserted at the
    /// start of every `process()` batch in debug builds so a missed mutation
    /// site fails on the next output byte instead of shipping a stale encoder
    /// mode. Free in release.
    #[cfg(debug_assertions)]
    pub(crate) fn debug_assert_mode_mirror_in_sync(&self) {
        debug_assert_eq!(
            self.mode_mirror.keyboard_mode(),
            self.keyboard_mode(),
            "keyboard-mode mirror out of sync: a mutation site forgot refresh_mode_mirror()"
        );
        debug_assert_eq!(
            self.mode_mirror.mouse_mode(),
            self.mouse_mode(),
            "mouse-mode mirror out of sync: a mutation site forgot refresh_mode_mirror()"
        );
    }
}

#[cfg(test)]
mod tests {
    //! The obligation, walked: every way the fold's inputs can move — through
    //! `process()` and through every host mutator — leaves the mirror equal to
    //! the fold, read lock-free.
    use super::super::Terminal;
    use aterm_types::keyboard::KeyboardMode;
    use aterm_types::mouse::MouseMode;

    fn assert_in_sync(term: &Terminal, what: &str) {
        let m = term.mode_mirror();
        assert_eq!(
            m.keyboard_mode(),
            term.keyboard_mode(),
            "keyboard after {what}"
        );
        assert_eq!(m.mouse_mode(), term.mouse_mode(), "mouse after {what}");
        assert_eq!(
            m.mouse_tracking_enabled(),
            term.mouse_tracking_enabled(),
            "tracking after {what}"
        );
    }

    #[test]
    fn a_fresh_terminal_publishes_its_fold_not_an_empty_word() {
        let term = Terminal::new(24, 80);
        // The fresh fold is NOT `KeyboardMode::empty()` (negative flags such as
        // ALT_NO_ESC / NO_SPECIAL_MODIFIERS may be set by default), so a mirror
        // left at `Default` would already be wrong here.
        assert_in_sync(&term, "construction");
    }

    #[test]
    fn every_process_driven_mutation_refreshes_the_mirror() {
        let mut term = Terminal::new(24, 80);
        let steps: &[(&str, &[u8])] = &[
            ("kitty push disambiguate", b"\x1b[>1u"),
            ("kitty set report-all", b"\x1b[=8;1u"),
            ("kitty pop", b"\x1b[<u"),
            ("DECCKM set", b"\x1b[?1h"),
            ("DECKPAM", b"\x1b="),
            ("DECBKM set", b"\x1b[?67h"),
            ("altSendsEscape reset", b"\x1b[?1039l"),
            ("metaSendsEscape set", b"\x1b[?1036h"),
            ("numLock reset", b"\x1b[?1035l"),
            ("modifyOtherKeys 2", b"\x1b[>4;2m"),
            ("formatOtherKeys 1", b"\x1b[>4;1m"),
            ("mouse 1000", b"\x1b[?1000h"),
            ("mouse 1002", b"\x1b[?1002h"),
            ("mouse 1003", b"\x1b[?1003h"),
            ("mouse 9 (x10)", b"\x1b[?9h"),
            ("mouse off", b"\x1b[?1003l\x1b[?9l"),
            (
                "enter alt screen with own kitty state",
                b"\x1b[?1049h\x1b[>2u",
            ),
            ("DECSTR on alt", b"\x1b[!p"),
            ("leave alt screen", b"\x1b[?1049l"),
            ("VT52 enter", b"\x1b[?2l"),
            // `ESC <` leaves VT52 (ANSI mode); in VT52 neither CSI nor `ESC c`
            // parse, so the walk must exit before the RIS step below.
            ("VT52 exit", b"\x1b<"),
            ("RIS", b"\x1bc"),
        ];
        for (what, bytes) in steps {
            term.process(bytes);
            assert_in_sync(&term, what);
        }
        // And the walk actually exercised a non-trivial word at least once:
        // re-push and confirm the lock-free read carries the kitty bit.
        term.process(b"\x1b[>1u");
        assert!(
            term.mode_mirror()
                .keyboard_mode()
                .contains(KeyboardMode::DISAMBIGUATE_ESC_CODES)
        );
        term.process(b"\x1b[?1000h");
        assert_eq!(term.mode_mirror().mouse_mode(), MouseMode::Normal);
    }

    #[test]
    fn every_host_mutator_refreshes_the_mirror() {
        let mut term = Terminal::new(24, 80);
        term.process(b"\x1b[>1u\x1b[?1000h");
        term.set_kitty_keyboard_enabled(false);
        assert_in_sync(&term, "set_kitty_keyboard_enabled(false)");
        assert!(
            !term
                .mode_mirror()
                .keyboard_mode()
                .contains(KeyboardMode::DISAMBIGUATE_ESC_CODES),
            "capability off must drop the kitty bit from the lock-free word too"
        );
        term.set_kitty_keyboard_enabled(true);
        assert_in_sync(&term, "set_kitty_keyboard_enabled(true)");
        term.reset();
        assert_in_sync(&term, "reset()");
        assert_eq!(term.mode_mirror().mouse_mode(), MouseMode::None);

        // Checkpoint hydration, both paths.
        let mut source = Terminal::new(24, 80);
        source.process(b"\x1b[>1u\x1b[?1002h\x1b[?1h");
        let cp = source.checkpoint();
        let fresh = Terminal::from_checkpoint(&cp, super::super::HostBindings::default());
        assert_in_sync(&fresh, "from_checkpoint");
        assert_eq!(fresh.mode_mirror().mouse_mode(), MouseMode::ButtonEvent);
        let mut live = Terminal::new(24, 80);
        live.restore_checkpoint(&cp);
        assert_in_sync(&live, "restore_checkpoint");
        assert!(
            live.mode_mirror()
                .keyboard_mode()
                .contains(KeyboardMode::APP_CURSOR)
        );
    }
}
