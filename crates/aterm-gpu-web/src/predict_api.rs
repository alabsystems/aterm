// SPDX-License-Identifier: MIT
// Copyright 2026 Andrew Yates

//! The host-facing predictive-echo API on [`AtermGpuTerminal`]: mosh-style
//! speculative typing driven by the shared [`aterm_predict`] state machine
//! (the same predictor the native app runs). Mirrors aterm-wasm's
//! `predict_api`, producing the same `[row, col, codepoint]` overlay triples.
//!
//! Host seams:
//! 1. [`predict_char`](AtermGpuTerminal::predict_char) /
//!    [`predict_backspace`](AtermGpuTerminal::predict_backspace) on the keydown
//!    whose bytes go to the PTY, and
//!    [`predict_line_submit`](AtermGpuTerminal::predict_line_submit) on a key
//!    that writes a line terminator (Enter in any spelling, Ctrl+J, Ctrl+M), and
//!    [`predict_unmodelled_input`](AtermGpuTerminal::predict_unmodelled_input)
//!    on every other key whose bytes go to the PTY and on a paste (the
//!    password-prompt safety boundaries),
//! 2. [`predict_reconcile`](AtermGpuTerminal::predict_reconcile) after `process()`
//!    applies child output,
//! 3. [`predict_overlay`](AtermGpuTerminal::predict_overlay) when composing a
//!    frame (the ghost cells to paint), and
//! 4. [`predict_reset`](AtermGpuTerminal::predict_reset) when the coordinate
//!    space changes (`resize` calls it), and
//!    [`predict_session_reset`](AtermGpuTerminal::predict_session_reset) when the
//!    wrapper is reused for a different pane/session.
//!
//! Off by default: until `set_predictive_echo("adaptive" | "always")` every
//! entry point is inert (`PredictMode::parse`, as the native knob).
//!
//! This module samples the real monotonic clock (`aterm_time::Instant`),
//! because an unechoed guess must expire ~250 ms after the keypress. The state
//! machine itself is clock-injected; timing tests live in `aterm-predict`.

#[cfg(target_arch = "wasm32")]
use wasm_bindgen::prelude::*;

use crate::AtermGpuTerminal;
use aterm_predict::PredictMode;
use aterm_time::Instant;

#[cfg_attr(target_arch = "wasm32", wasm_bindgen)]
impl AtermGpuTerminal {
    /// Set the predictive-echo mode: `"off"` (default) | `"adaptive"` (show
    /// once the current line confirms echo and measured RTT is high enough)
    /// | `"always"`. Case-insensitive; unknown strings mean `off`.
    pub fn set_predictive_echo(&mut self, mode: &str) {
        let mode = PredictMode::parse(mode);
        // A real mode change flushes in-flight guesses; switching to Off also
        // forgets the confirmation epoch so no armed deadline outlives a
        // disable.
        self.predict.set_mode(mode);
        if mode == PredictMode::Off {
            self.predict.reset();
        }
    }

    /// Register a printable character just written to the PTY (call beside
    /// `encode_key`). The guess anchors at the live cursor, extends pending
    /// type-ahead and never crosses the right margin. Returns whether a guess
    /// is tracked; display is gated separately (see
    /// [`predict_overlay`](Self::predict_overlay)).
    pub fn predict_char(&mut self, ch: char) -> bool {
        if self.term.is_alternate_screen() || self.term.kitty_suppresses_predictive_echo() {
            self.predict.reset();
            return false;
        }
        let cur = self.term.cursor();
        let cols = self.term.grid().cols();
        let coords = self.term.content_scroll_state().coordinate_generation();
        self.predict
            .predict_char(ch, (cur.row, cur.col), coords, cols, Instant::now())
    }

    /// Register a Backspace: cancels only our own trailing guess (erasing real
    /// content is left to the program's echo, and ends the confirmation epoch).
    /// Returns whether a guess was cancelled.
    pub fn predict_backspace(&mut self) -> bool {
        if self.term.is_alternate_screen() || self.term.kitty_suppresses_predictive_echo() {
            self.predict.reset();
            return false;
        }
        self.predict.predict_backspace(Instant::now())
    }

    /// Register a line terminator going to the PTY: Enter (CR, or Shift+Enter's
    /// LF), Ctrl+J, Ctrl+M — not Alt+Enter, whose ESC prefix makes it any other
    /// key. Ends the confirmation epoch, so nothing typed after it displays
    /// (`adaptive`) until echo is confirmed again. Required for password safety:
    /// a password typed ahead of the next prompt still sits on the proven row.
    /// The line's own unechoed tail keeps painting until its echo. Cheap.
    pub fn predict_line_submit(&mut self) {
        self.predict.note_line_submit();
    }

    /// Register any OTHER key not handed to `predict_char` / `predict_backspace`
    /// (Tab, an arrow, a control or Alt chord, an IME commit) or a paste, when
    /// its bytes go to the PTY. Ends the confirmation epoch like
    /// `predict_line_submit` and retires the guesses in flight: the key may erase
    /// them (Ctrl+W, Ctrl+H), and a guess left over the erased cell is a ghost of
    /// the erased text. Which keys submit a line is the program's call, so this
    /// is the seam for every key that is not a terminator. Cheap.
    pub fn predict_unmodelled_input(&mut self) {
        // ICF barrier (see `predict_reset`): keep this export's body distinct.
        std::hint::black_box(2u8);
        self.predict.note_unmodelled_input();
    }

    /// Reconcile pending guesses against the grid; call after `process()`.
    /// Confirmed leading guesses retire (arming display), a divergence flushes
    /// the set, and a no-echo context refuses prediction: the alternate screen
    /// or an app-owned Kitty composer (REPORT_EVENT_TYPES /
    /// REPORT_ALL_KEYS_AS_ESC). While scrolled into history only expiry runs,
    /// since guesses are in active-grid coords.
    pub fn predict_reconcile(&mut self) {
        let now = Instant::now();
        if self.term.grid().display_offset() != 0 {
            // Expiry only: a guess in flight when the user scrolled up must
            // still expire, or `predict_next_deadline_ms` stays in the past.
            let _ = self.predict.overlay(now);
            return;
        }
        let cur = self.term.cursor();
        // The native no-echo gate: alt screen OR app-owned Kitty composer.
        // Read-only display projection — never a byte producer.
        let no_echo =
            self.term.is_alternate_screen() || self.term.kitty_suppresses_predictive_echo();
        // `reconcile` probes the same row once per retired guess plus the
        // head; memoize by row into the resident scratch, refilled in place by
        // `render_row_into` (the same code path as `render_row`), instead of
        // re-resolving the whole row each time.
        let mut scratch = std::mem::take(&mut self.pred_row_scratch);
        let mut cached: Option<u16> = None;
        // Rows that moved under the guesses retire them and their epoch.
        let coords = self.term.content_scroll_state().coordinate_generation();
        let (term, predict) = (&self.term, &mut self.predict);
        predict.reconcile(Some((cur.row, cur.col)), coords, no_echo, now, |r, c| {
            if cached != Some(r) {
                term.render_row_into(r as usize, &mut scratch);
                cached = Some(r);
            }
            // The native observe: the cell's glyph, with blank/space mapped to
            // None (a typed space confirms by cursor advance instead).
            scratch
                .get(c as usize)
                .map(|cell| cell.ch)
                .filter(|ch| *ch != ' ')
        });
        self.pred_row_scratch = scratch;
    }

    /// The ghost cells to paint this frame, as flat `[row, col, codepoint]`
    /// triples (`Uint32Array`). The host paints them tentatively (dim or
    /// underlined) and may draw its cursor past the last one. Runs expiry,
    /// then the display gate: `always` shows all pending; `adaptive` shows them
    /// once echo is confirmed on this line and RTT is high enough. Empty in
    /// app-owned Kitty composers and while scrolled into history.
    pub fn predict_overlay(&mut self) -> Vec<u32> {
        if self.term.is_alternate_screen() || self.term.kitty_suppresses_predictive_echo() {
            self.predict.reset();
            return Vec::new();
        }
        let shown = self.predict.overlay(Instant::now());
        if self.term.grid().display_offset() != 0 {
            return Vec::new();
        }
        let mut out = Vec::with_capacity(shown.len() * 3);
        for p in shown {
            out.extend([u32::from(p.row), u32::from(p.col), u32::from(p.ch)]);
        }
        out
    }

    /// Milliseconds until the oldest pending guess expires, or `undefined`
    /// when none is pending. Arm one timer for it and call
    /// [`predict_overlay`](Self::predict_overlay) + repaint there.
    pub fn predict_next_deadline_ms(&self) -> Option<f64> {
        self.predict
            .next_deadline()
            .map(|d| d.saturating_duration_since(Instant::now()).as_secs_f64() * 1000.0)
    }

    /// Drop in-flight guesses because this terminal's coordinate space changed
    /// (`resize` calls this). Forgets the confirmation epoch, keeps the learned
    /// link RTT.
    pub fn predict_reset(&mut self) {
        // ICF barrier — see aterm-wasm/src/predict_api.rs: identical-body shims get
        // merged by LLVM and wasm-bindgen then cross-binds the JS methods.
        std::hint::black_box(0u8);
        self.predict.reset();
    }

    /// Reset for a different pane/session: also forgets the learned echo RTT,
    /// so a slow remote pane cannot make a local one display speculation.
    /// Only pane-reusing hosts need this.
    pub fn predict_session_reset(&mut self) {
        // Keep this export distinct from the coordinate-only reset and line-submit
        // shims under wasm LLVM function merging.
        std::hint::black_box(1u8);
        self.predict.reset_session();
    }
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use super::*;

    /// Build a terminal, or skip when the environment has no system font (the
    /// same posture as the other binding tests).
    fn terminal() -> Option<AtermGpuTerminal> {
        AtermGpuTerminal::new_from_system(4, 20, 16.0)
    }

    #[test]
    fn off_by_default_and_unknown_modes_fail_safe() {
        let Some(mut t) = terminal() else {
            eprintln!("no system font; skipping export smoke");
            return;
        };
        assert!(!t.predict_char('a'), "default off ⇒ inert");
        t.set_predictive_echo("nonsense");
        assert!(!t.predict_char('a'), "unknown mode parses to off");
        assert!(t.predict_next_deadline_ms().is_none(), "nothing armed");
    }

    #[test]
    fn always_tracks_paints_and_retires_through_the_export() {
        let Some(mut t) = terminal() else {
            return;
        };
        t.set_predictive_echo("always");
        assert!(t.predict_char('a'));
        assert_eq!(t.predict_overlay(), vec![0, 0, u32::from('a')]);
        assert!(t.predict_next_deadline_ms().is_some(), "glitch timer armed");
        // The real echo lands: the guess retires, the overlay clears, and the
        // self-heal deadline disarms.
        t.process(b"a");
        t.predict_reconcile();
        assert!(t.predict_overlay().is_empty());
        assert!(t.predict_next_deadline_ms().is_none());
    }

    #[test]
    fn adaptive_tracks_but_never_shows_an_unconfirmed_line() {
        let Some(mut t) = terminal() else {
            return;
        };
        t.set_predictive_echo("adaptive");
        assert!(t.predict_char('a'), "tracked, awaiting the echo");
        assert!(
            t.predict_overlay().is_empty(),
            "no echo confirmed on this line yet ⇒ nothing painted (password safety)"
        );
    }

    #[test]
    fn adaptive_confirmed_echo_retires_the_prediction() {
        let Some(mut t) = terminal() else {
            return;
        };
        t.set_predictive_echo("adaptive");
        assert!(t.predict_char('a'));
        assert!(t.predict_next_deadline_ms().is_some());
        t.process(b"a"); // the echo lands ⇒ epoch confirmed
        t.predict_reconcile();
        assert!(
            t.predict_next_deadline_ms().is_none(),
            "confirmed echo retires the tracked prediction"
        );
    }

    #[test]
    fn session_reset_forgets_rtt_while_coordinate_reset_preserves_it() {
        let Some(mut t) = terminal() else {
            return;
        };
        t.set_predictive_echo("adaptive");
        assert!(t.predict_char('a'));
        std::thread::sleep(std::time::Duration::from_millis(20));
        t.process(b"a");
        t.predict_reconcile();
        assert!(t.predict_char('b'));
        assert!(
            t.predict_overlay().is_empty(),
            "one slow confirmation cannot open the Adaptive display latch"
        );
        std::thread::sleep(std::time::Duration::from_millis(20));
        t.process(b"b");
        t.predict_reconcile();
        assert!(t.predict_char('c'));
        assert!(
            !t.predict_overlay().is_empty(),
            "two independent slow confirmations display the next prediction"
        );

        // A coordinate reset keeps the link estimate, so a slow confirmation
        // in the new epoch makes the next prediction visible.
        t.predict_reset();
        assert!(t.predict_char('x'));
        std::thread::sleep(std::time::Duration::from_millis(20));
        t.process(b"x");
        t.predict_reconcile();
        assert!(t.predict_char('y'));
        assert!(
            !t.predict_overlay().is_empty(),
            "coordinate-only reset preserves the same session's RTT"
        );

        // A pane identity switch must start untrained. Its immediate local echo
        // closes the Adaptive gate instead of inheriting the remote RTT.
        t.predict_session_reset();
        assert!(t.predict_char('m'));
        t.process(b"m");
        t.predict_reconcile();
        assert!(t.predict_char('n'));
        assert!(t.predict_overlay().is_empty());
    }

    #[test]
    fn codex_report_event_types_suppresses_arm_reconcile_and_display() {
        let Some(mut t) = terminal() else {
            return;
        };
        t.set_predictive_echo("always");

        // Reconciliation gate: a guess armed before Codex negotiates its observed
        // 1|2|4 flags is flushed even though REPORT_ALL_KEYS_AS_ESC is absent.
        assert!(t.predict_char('a'));
        t.process(b"\x1b[>7u");
        t.predict_reconcile();
        assert!(t.predict_overlay().is_empty());
        assert!(t.predict_next_deadline_ms().is_none());

        // Display gate: a mode flip between arm and paint cannot expose the ghost.
        t.process(b"\x1b[=0u");
        assert!(t.predict_char('b'));
        t.process(b"\x1b[>7u");
        assert!(t.predict_overlay().is_empty());
        assert!(t.predict_next_deadline_ms().is_none());

        // Arm gate: while Codex owns the composer there is no speculative state and
        // therefore no 250 ms erase deadline.
        assert!(!t.predict_char('c'));
        assert!(!t.predict_backspace());
        assert!(t.predict_next_deadline_ms().is_none());
    }

    #[test]
    fn line_submit_ends_the_epoch_no_same_row_leak() {
        // After Enter, a non-echoing prompt on the same physical row must not
        // inherit the previous command's confirmation.
        let Some(mut t) = terminal() else {
            return;
        };
        t.set_predictive_echo("adaptive");
        assert!(t.predict_char('a'));
        t.process(b"a");
        t.predict_reconcile();
        assert!(t.predict_char('b'));
        assert!(
            t.predict_next_deadline_ms().is_some(),
            "the next guess is tracked before submit"
        );
        t.predict_line_submit();
        assert!(
            t.predict_overlay().is_empty(),
            "a fast link paints nothing, before or after the submit"
        );
        assert!(t.predict_char('s'), "the password keystroke is tracked…");
        assert!(
            t.predict_overlay().is_empty(),
            "…but never displayed on the unconfirmed new line (no leak)"
        );
    }

    #[test]
    fn only_a_line_terminator_carries_the_guesses_in_flight() {
        // The two boundary exports differ in what they do to the pixels already
        // painted: a terminator keeps the line's unechoed tail, any other key
        // (Ctrl+W, Ctrl+H) may have erased it and retires it.
        let Some(mut t) = terminal() else {
            return;
        };
        t.set_predictive_echo("always");
        assert!(t.predict_char('a'));
        t.predict_line_submit();
        assert_eq!(
            t.predict_overlay(),
            vec![0, 0, u32::from('a')],
            "Enter keeps the tail on glass"
        );
        t.predict_unmodelled_input();
        assert!(
            t.predict_overlay().is_empty(),
            "a key that may have erased the tail retires it"
        );
        assert!(t.predict_next_deadline_ms().is_none(), "nothing left armed");
    }

    #[test]
    fn divergence_and_alt_screen_flush() {
        let Some(mut t) = terminal() else {
            return;
        };
        t.set_predictive_echo("always");
        t.predict_char('a');
        t.process(b"*"); // the program echoed a DIFFERENT glyph (masked input)
        t.predict_reconcile();
        assert!(t.predict_overlay().is_empty(), "divergence flushes the set");
        t.predict_char('x');
        t.process(b"\x1b[?1049h"); // vim/less: the alternate screen
        t.predict_reconcile();
        assert!(t.predict_overlay().is_empty(), "alt screen refuses guesses");
        assert!(t.predict_next_deadline_ms().is_none());
    }

    #[test]
    fn backspace_cancels_and_disable_is_fully_inert() {
        let Some(mut t) = terminal() else {
            return;
        };
        t.set_predictive_echo("always");
        t.predict_char('a');
        t.predict_char('b');
        assert!(t.predict_backspace());
        assert_eq!(t.predict_overlay(), vec![0, 0, u32::from('a')]);
        // Disabled with a guess in flight: the overlay empties and no deadline
        // stays armed.
        t.set_predictive_echo("off");
        assert!(t.predict_overlay().is_empty());
        assert!(t.predict_next_deadline_ms().is_none());
    }

    #[test]
    fn scrolled_back_viewport_paints_no_ghosts() {
        let Some(mut t) = terminal() else {
            return;
        };
        for i in 0..12 {
            t.process(format!("line {i}\r\n").as_bytes());
        }
        t.set_predictive_echo("always");
        assert!(t.predict_char('a'));
        t.scroll_lines(2);
        assert!(
            t.predict_overlay().is_empty(),
            "guesses are active-grid coords — never painted over scrollback"
        );
        t.scroll_to_bottom();
        assert_eq!(
            t.predict_overlay().len(),
            3,
            "back at bottom, ghost returns"
        );
    }

    #[test]
    fn resize_drops_inflight_guesses() {
        let Some(mut t) = terminal() else {
            return;
        };
        t.set_predictive_echo("always");
        t.predict_char('a');
        t.resize(10, 40);
        assert!(t.predict_overlay().is_empty(), "stale-coords guess dropped");
        assert!(t.predict_next_deadline_ms().is_none());
    }
}
