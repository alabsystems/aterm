// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Lock-free mirror of the terminal's input-encoding modes.
//!
//! Publishes as atomics the fold behind
//! [`Terminal::keyboard_mode`](super::Terminal::keyboard_mode),
//! [`Terminal::mouse_mode`](super::Terminal::mouse_mode) and
//! [`Terminal::focus_reporting_enabled`](super::Terminal::focus_reporting_enabled) and
//! [`Terminal::focus_arms`](super::Terminal::focus_arms), plus the
//! alternate-screen flag, DEC 1007 (alternate scroll) and DEC 2004 (bracketed paste). The input
//! seam reads it on every key
//! press without the terminal mutex; so do the DEC 1004 gate at the window's tab/pane
//! re-stabilization point, which may not take the engine mutex, and the wheel route
//! (`input::wheel_route_mirrored`, twin of the locked `wheel_route_for`).
//!
//! Refreshed under the lock at the end of every `process()` batch and by every host mutator that
//! moves an input. A debug assertion at the start of each batch checks the mirror still equals
//! the fold, so a mutator that forgets to refresh fails on the next output. Shared as an `Arc`
//! because the seam holds only `&Mutex<Terminal>`; `Release` on publish, `Acquire` on read.

use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU32, Ordering};

use aterm_types::keyboard::KeyboardMode;
use aterm_types::mouse::MouseMode;

/// The lock-free input-mode word. See the module docs.
#[derive(Debug, Default)]
pub struct ModeMirror {
    /// [`KeyboardMode`] bits (a `u32` bitflags word).
    keyboard: AtomicU32,
    /// [`MouseMode`] discriminant (`repr(u8)`, `None = 0`).
    mouse: AtomicU8,
    /// DEC 1004 (focus reporting) — `modes.focus_reporting`.
    focus: AtomicBool,
    /// The program's DEC 1004 enables — `Terminal::focus_arms()`. Stored after `focus`,
    /// so a reader that sees a new count sees the mode that enable left on.
    focus_arms: AtomicU32,
    /// The alternate screen is active — `Terminal::is_alternate_screen()`.
    alternate_screen: AtomicBool,
    /// DEC 1007 (alternate scroll) — `modes.alternate_scroll`: on the alternate
    /// screen, a wheel nobody tracks becomes arrow keys.
    alternate_scroll: AtomicBool,
    /// DEC 2004 (bracketed paste) — `modes.bracketed_paste`: a paste framed
    /// by the live mode is wrapped in its markers, so it writes the program
    /// bytes whatever its text.
    bracketed_paste: AtomicBool,
}

impl ModeMirror {
    /// The keyboard encoding mode the terminal last published — the same value
    /// `Terminal::keyboard_mode()` returns under the lock, without the lock.
    #[inline]
    #[must_use]
    pub fn keyboard_mode(&self) -> KeyboardMode {
        KeyboardMode::from_bits_truncate(self.keyboard.load(Ordering::Acquire))
    }

    /// The mouse tracking mode the terminal last published.
    #[inline]
    #[must_use]
    pub fn mouse_mode(&self) -> MouseMode {
        mouse_mode_from_u8(self.mouse.load(Ordering::Acquire))
    }

    /// Lock-free twin of `Terminal::mouse_tracking_enabled()`.
    #[inline]
    #[must_use]
    pub fn mouse_tracking_enabled(&self) -> bool {
        self.mouse.load(Ordering::Acquire) != 0
    }

    /// Whether the published tracking mode reports a pointer motion with
    /// `button` held (`3`: none): the rule `Terminal::encode_mouse_motion`
    /// encodes by (`super::mouse::motion_reported`), without the lock.
    #[inline]
    #[must_use]
    pub fn reports_motion(&self, button: u8) -> bool {
        super::mouse::motion_reported(self.mouse_mode(), button)
    }

    /// Whether the published tracking mode reports a wheel notch: the rule
    /// `Terminal::encode_mouse_wheel` encodes by (`super::mouse::wheel_reported`:
    /// not X10, which is press-only), without the lock.
    #[inline]
    #[must_use]
    pub fn reports_wheel(&self) -> bool {
        super::mouse::wheel_reported(self.mouse_mode())
    }

    /// Lock-free twin of `Terminal::focus_reporting_enabled()` (DEC 1004), for the focus report
    /// written on the winit thread, which must never take an engine mutex.
    #[inline]
    #[must_use]
    pub fn focus_reporting_enabled(&self) -> bool {
        self.focus.load(Ordering::Acquire)
    }

    /// Lock-free twin of `Terminal::focus_arms()`: the host compares it with the count it
    /// last answered to learn that the program turned DEC 1004 on again. Read it before
    /// [`Self::focus_reporting_enabled`]: the count is published second, so the mode read
    /// after it is at least as new as that enable.
    #[inline]
    #[must_use]
    pub fn focus_arms(&self) -> u32 {
        self.focus_arms.load(Ordering::Acquire)
    }

    /// Lock-free twin of `Terminal::is_alternate_screen()`.
    #[inline]
    #[must_use]
    pub fn alternate_screen(&self) -> bool {
        self.alternate_screen.load(Ordering::Acquire)
    }

    /// Lock-free twin of `Terminal::modes().alternate_scroll` (DEC 1007).
    #[inline]
    #[must_use]
    pub fn alternate_scroll(&self) -> bool {
        self.alternate_scroll.load(Ordering::Acquire)
    }

    /// Lock-free twin of `Terminal::modes().bracketed_paste` (DEC 2004): the
    /// framing a paste that reads the live mode gets
    /// ([`super::Terminal::format_paste`]).
    #[inline]
    #[must_use]
    pub fn bracketed_paste(&self) -> bool {
        self.bracketed_paste.load(Ordering::Acquire)
    }

    /// Publish a fresh fold of `term`. Called only by the terminal, under its
    /// mutex ([`super::Terminal::refresh_mode_mirror`]).
    #[inline]
    fn publish(&self, term: &super::Terminal) {
        self.keyboard
            .store(term.keyboard_mode().bits(), Ordering::Release);
        self.mouse.store(term.mouse_mode() as u8, Ordering::Release);
        self.focus
            .store(term.focus_reporting_enabled(), Ordering::Release);
        self.focus_arms.store(term.focus_arms(), Ordering::Release);
        self.alternate_screen
            .store(term.is_alternate_screen(), Ordering::Release);
        self.alternate_scroll
            .store(term.modes.alternate_scroll, Ordering::Release);
        self.bracketed_paste
            .store(term.modes.bracketed_paste, Ordering::Release);
    }
}

/// Inverse of `MouseMode as u8`. Unknown discriminants decode as `None`
/// (tracking off) — the fail-safe direction for an encoder gate.
#[inline]
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
    #[inline]
    #[must_use]
    pub const fn mode_mirror(&self) -> &std::sync::Arc<ModeMirror> {
        &self.mode_mirror
    }

    /// Re-publish the fold. Called by every site outside `process()` that can move one of its
    /// inputs, and once per batch by `process_at` after the parser runs.
    #[inline]
    pub(crate) fn refresh_mode_mirror(&self) {
        self.mode_mirror.publish(self);
    }

    /// Asserts the published word equals the live fold. Run at the start of every `process()`
    /// batch in debug builds, so a missed mutation site fails on the next output byte.
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
        debug_assert_eq!(
            self.mode_mirror.focus_reporting_enabled(),
            self.focus_reporting_enabled(),
            "focus-report mirror out of sync: a mutation site forgot refresh_mode_mirror()"
        );
        debug_assert_eq!(
            self.mode_mirror.focus_arms(),
            self.focus_arms(),
            "focus-arm mirror out of sync: a mutation site forgot refresh_mode_mirror()"
        );
        debug_assert_eq!(
            self.mode_mirror.alternate_screen(),
            self.is_alternate_screen(),
            "alternate-screen mirror out of sync: a mutation site forgot refresh_mode_mirror()"
        );
        debug_assert_eq!(
            self.mode_mirror.alternate_scroll(),
            self.modes.alternate_scroll,
            "alternate-scroll mirror out of sync: a mutation site forgot refresh_mode_mirror()"
        );
        debug_assert_eq!(
            self.mode_mirror.bracketed_paste(),
            self.modes.bracketed_paste,
            "bracketed-paste mirror out of sync: a mutation site forgot refresh_mode_mirror()"
        );
    }
}

#[cfg(test)]
mod tests {
    //! Every way the fold's inputs move, through `process()` and every host mutator, leaves the
    //! mirror equal to the fold.
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
        assert_eq!(
            m.focus_reporting_enabled(),
            term.focus_reporting_enabled(),
            "focus reporting after {what}"
        );
        assert_eq!(m.focus_arms(), term.focus_arms(), "focus arms after {what}");
        assert_eq!(
            m.alternate_screen(),
            term.is_alternate_screen(),
            "alternate screen after {what}"
        );
        assert_eq!(
            m.alternate_scroll(),
            term.modes().alternate_scroll,
            "alternate scroll after {what}"
        );
        assert_eq!(
            m.bracketed_paste(),
            term.modes().bracketed_paste,
            "bracketed paste after {what}"
        );
    }

    #[test]
    fn a_fresh_terminal_publishes_its_fold_not_an_empty_word() {
        let term = Terminal::new(24, 80);
        // The constructor publishes the fresh fold rather than relying on `Default` to match it.
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
            // Both edges: a mirror that only latched true would pass a set-only walk.
            ("focus reporting on", b"\x1b[?1004h"),
            ("focus reporting off", b"\x1b[?1004l"),
            ("focus reporting on again", b"\x1b[?1004h"),
            (
                "enter alt screen with own kitty state",
                b"\x1b[?1049h\x1b[>2u",
            ),
            ("DECSTR on alt", b"\x1b[!p"),
            ("leave alt screen", b"\x1b[?1049l"),
            // DEC 1007 and the screen (the wheel route's inputs), both edges, every door.
            ("alternate scroll on", b"\x1b[?1007h"),
            ("enter alt screen 1049", b"\x1b[?1049h"),
            ("alternate scroll off on alt", b"\x1b[?1007l"),
            ("leave alt screen 1049", b"\x1b[?1049l"),
            ("enter alt screen 47", b"\x1b[?47h"),
            ("leave alt screen 47", b"\x1b[?47l"),
            ("enter alt screen 1047", b"\x1b[?1047h"),
            ("leave alt screen 1047", b"\x1b[?1047l"),
            // DEC 2004 (a paste's live framing), both edges, and DECSTR's reset of it.
            ("bracketed paste on", b"\x1b[?2004h"),
            ("bracketed paste off", b"\x1b[?2004l"),
            ("bracketed paste on again", b"\x1b[?2004h"),
            ("DECSTR clears bracketed paste", b"\x1b[!p"),
            ("VT52 enter", b"\x1b[?2l"),
            // In VT52 neither CSI nor `ESC c` parse: leave it before RIS.
            ("VT52 exit", b"\x1b<"),
            ("RIS", b"\x1bc"),
        ];
        for (what, bytes) in steps {
            term.process(bytes);
            assert_in_sync(&term, what);
        }
        // The walk carried a non-trivial word at least once.
        term.process(b"\x1b[>1u");
        assert!(
            term.mode_mirror()
                .keyboard_mode()
                .contains(KeyboardMode::DISAMBIGUATE_ESC_CODES)
        );
        term.process(b"\x1b[?1000h");
        assert_eq!(term.mode_mirror().mouse_mode(), MouseMode::Normal);
        term.process(b"\x1b[?1004h");
        assert!(term.mode_mirror().focus_reporting_enabled());
        term.process(b"\x1b[?1004l");
        assert!(!term.mode_mirror().focus_reporting_enabled());
        // ... and the wheel's screen facts: a pager on the alt screen with 1007.
        term.process(b"\x1b[?1007h\x1b[?1049h");
        assert!(term.mode_mirror().alternate_screen() && term.mode_mirror().alternate_scroll());
        term.process(b"\x1b[?1049l\x1b[?1007l");
        assert!(!term.mode_mirror().alternate_screen() && !term.mode_mirror().alternate_scroll());
        term.process(b"\x1b[?2004h");
        assert!(term.mode_mirror().bracketed_paste());
        term.process(b"\x1b[?2004l");
        assert!(!term.mode_mirror().bracketed_paste());
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
        term.set_host_alternate_scroll(true);
        assert_in_sync(&term, "set_host_alternate_scroll(true)");
        assert!(term.mode_mirror().alternate_scroll());
        term.set_host_alternate_scroll(false);
        assert_in_sync(&term, "set_host_alternate_scroll(false)");
        term.process(b"\x1b[?1049h");
        assert!(term.mode_mirror().alternate_screen());
        assert!(term.leave_alternate_screen());
        assert_in_sync(&term, "leave_alternate_screen()");
        // `apply_config` arms DEC 1004 outside `process()`, so it must republish itself.
        let mut config = crate::config::TerminalConfig::default();
        config.focus_reporting = true;
        let _ = term.apply_config(&config);
        assert!(term.focus_reporting_enabled(), "the host armed DEC 1004");
        assert_in_sync(&term, "apply_config(focus_reporting = true)");
        // ... and seeds DEC 2004 from the host's setting, both edges.
        config.bracketed_paste = true;
        let _ = term.apply_config(&config);
        assert!(term.modes().bracketed_paste, "the host seeded DEC 2004");
        assert_in_sync(&term, "apply_config(bracketed_paste = true)");
        config.bracketed_paste = false;
        let _ = term.apply_config(&config);
        assert_in_sync(&term, "apply_config(bracketed_paste = false)");
        term.process(b"x");
        assert_in_sync(&term, "one output byte after apply_config");
        term.process(b"\x1b[?1004h");
        assert_in_sync(&term, "focus reporting armed");
        term.reset();
        assert_in_sync(&term, "reset()");
        assert_eq!(term.mode_mirror().mouse_mode(), MouseMode::None);
        assert!(
            !term.mode_mirror().focus_reporting_enabled(),
            "a reset disarms DEC 1004 in the lock-free word too"
        );

        // Checkpoint hydration, both paths.
        let mut source = Terminal::new(24, 80);
        source.process(b"\x1b[>1u\x1b[?1002h\x1b[?1h\x1b[?1004h");
        let cp = source.checkpoint();
        let fresh = Terminal::from_checkpoint(&cp);
        assert_in_sync(&fresh, "from_checkpoint");
        assert_eq!(fresh.mode_mirror().mouse_mode(), MouseMode::ButtonEvent);
        assert!(
            fresh.mode_mirror().focus_reporting_enabled(),
            "a hydrated terminal carries DEC 1004 into the lock-free word"
        );
        let mut live = Terminal::new(24, 80);
        live.restore_checkpoint(&cp);
        assert_in_sync(&live, "restore_checkpoint");
        assert!(
            live.mode_mirror()
                .keyboard_mode()
                .contains(KeyboardMode::APP_CURSOR)
        );
    }

    /// The arm count moves once per off->on edge of the program's DEC 1004 (DECSET or
    /// XTRESTORE), never for a re-sent `?1004h`, a disable, the host's own arming or a
    /// reset, and the lock-free word carries every value.
    #[test]
    fn the_focus_arm_count_moves_on_each_program_enable_edge_only() {
        let mut term = Terminal::new(24, 80);
        let arms = |term: &Terminal| term.mode_mirror().focus_arms();
        assert_eq!(arms(&term), 0, "a fresh terminal has armed nothing");
        let steps: &[(&str, &[u8], u32)] = &[
            ("first enable", b"\x1b[?1004h", 1),
            ("a re-sent enable", b"\x1b[?1004h\x1b[?1004h", 1),
            ("a disable", b"\x1b[?1004l", 1),
            ("enable again", b"\x1b[?1004h", 2),
            ("off and on in one batch", b"\x1b[?1004l\x1b[?1004h", 3),
            (
                "XTSAVE, off, XTRESTORE",
                b"\x1b[?1004s\x1b[?1004l\x1b[?1004r",
                4,
            ),
            ("DECSTR turns it off", b"\x1b[!p", 4),
            ("RIS turns it off", b"\x1b[?1004h\x1bc", 5),
        ];
        for (what, bytes, want) in steps {
            term.process(bytes);
            assert_eq!(arms(&term), *want, "{what}");
            assert_in_sync(&term, what);
        }
        assert!(!term.focus_reporting_enabled(), "the reset left it off");
        let mut config = crate::config::TerminalConfig::default();
        config.focus_reporting = true;
        let _ = term.apply_config(&config);
        assert!(term.focus_reporting_enabled(), "the host armed DEC 1004");
        assert_eq!(
            arms(&term),
            5,
            "the host's arming is not the program's enable"
        );
        assert_in_sync(&term, "apply_config(focus_reporting = true)");
    }
}
