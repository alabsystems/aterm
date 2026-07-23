// SPDX-License-Identifier: MIT
// Copyright 2026 Andrew Yates

//! The host-facing predictive-echo API on [`AtermGpuTerminal`] — mosh-style
//! speculative typing, driven by the shared [`aterm_predict`] state machine
//! (the SAME predictor the native app runs — no forked heuristics). Mirrors
//! aterm-wasm's `predict_api` so the GPU and CPU bindings expose ONE contract:
//! the same shared state machine surfaces the same `[row, col, codepoint]`
//! overlay triples on both bundles.
//!
//! The host seams (the crate's own contract):
//! 1. [`predict_char`](AtermGpuTerminal::predict_char) /
//!    [`predict_backspace`](AtermGpuTerminal::predict_backspace) on the keydown
//!    whose bytes the host writes to the PTY, and
//!    [`predict_line_submit`](AtermGpuTerminal::predict_line_submit) on Enter
//!    (the submit boundary — the password-prompt safety seam),
//! 2. [`predict_reconcile`](AtermGpuTerminal::predict_reconcile) after `process()`
//!    applies a chunk of child output to the grid,
//! 3. [`predict_overlay`](AtermGpuTerminal::predict_overlay) when composing a
//!    frame (the tentative ghost cells the host paints), and
//! 4. [`predict_reset`](AtermGpuTerminal::predict_reset) when only the coordinate
//!    space changes (`resize` calls it automatically), and
//!    [`predict_session_reset`](AtermGpuTerminal::predict_session_reset) when the
//!    host reuses this wrapper for a different pane/session.
//!
//! OFF by default: until `set_predictive_echo("adaptive" | "always")` every
//! entry point is inert and rendering is untouched — the same fail-safe
//! `PredictMode::parse` domain as the native `predictive_echo` config knob.
//!
//! Unlike the effects clock (a deterministic host-advanced `advance_effects`
//! stream), this module samples the REAL monotonic clock (`web_time::Instant`:
//! std on native, `performance.now()` on wasm32). The predictor's glitch-expiry
//! self-heal is a wall-clock window (an unechoed guess must vanish ~250 ms
//! after the real keypress) — the same live-clock seam the native event loop
//! wires. The state machine itself stays pure/clock-injected; deterministic
//! timing coverage lives in the `aterm-predict` crate tests.

#[cfg(target_arch = "wasm32")]
use wasm_bindgen::prelude::*;

use crate::AtermGpuTerminal;
use aterm_predict::PredictMode;
use web_time::Instant;

#[cfg_attr(target_arch = "wasm32", wasm_bindgen)]
impl AtermGpuTerminal {
    /// Set the predictive-echo display mode: `"off"` (the default) |
    /// `"adaptive"` (show after the current line confirms echo and its measured
    /// RTT is high enough to benefit) | `"always"` (power users / demos). Case-
    /// insensitive; unknown strings fail safe to `off` — the native
    /// `predictive_echo` domain.
    pub fn set_predictive_echo(&mut self, mode: &str) {
        let mode = PredictMode::parse(mode);
        // A real mode change flushes in-flight guesses (set_mode). Flipping to
        // Off additionally forgets the confirmation epoch so the toggle is
        // FULLY inert: no armed deadline may outlive a disable (the native
        // stranded-deadline 100%-CPU lesson).
        self.predict.set_mode(mode);
        if mode == PredictMode::Off {
            self.predict.reset();
        }
    }

    /// Register a printable character the host just wrote to the PTY (the
    /// keydown seam — call beside `encode_key`). The guess anchors at the
    /// engine's live cursor, extends pending type-ahead, and never crosses the
    /// right margin. Returns whether a guess is now TRACKED — display is a
    /// separate gate (see [`predict_overlay`](Self::predict_overlay)).
    pub fn predict_char(&mut self, ch: char) -> bool {
        if self.term.is_alternate_screen() || self.term.kitty_suppresses_predictive_echo() {
            self.predict.reset();
            return false;
        }
        let cur = self.term.cursor();
        let cols = self.term.grid().cols();
        self.predict
            .predict_char(ch, (cur.row, cur.col), cols, Instant::now())
    }

    /// Register a Backspace: cancels our OWN trailing guess only (erasing
    /// already-committed real content is left to the program's echo). Returns
    /// whether state changed.
    pub fn predict_backspace(&mut self) -> bool {
        if self.term.is_alternate_screen() || self.term.kitty_suppresses_predictive_echo() {
            self.predict.reset();
            return false;
        }
        self.predict.predict_backspace(Instant::now())
    }

    /// Register a plain Enter (the SUBMIT boundary — call when the host writes
    /// the line terminator to the PTY). Ends the confirmation epoch: the NEXT
    /// line must re-confirm an echo before `adaptive` displays anything.
    /// LOAD-BEARING for password safety on a terminal scrolled to the bottom,
    /// where the cursor REUSES one physical row across logical lines: without
    /// it, a non-echoing password prompt landing on the same row as a just-
    /// confirmed command would inherit that confirmation and flash the secret
    /// (the native `note_line_submit` seam). Cheap no-op when nothing pends.
    pub fn predict_line_submit(&mut self) {
        self.predict.note_line_submit();
    }

    /// Reconcile pending guesses against the grid — call after `process()`
    /// applies a PTY chunk. Confirmed leading guesses retire (arming the
    /// epoch's display gate), any divergence flushes the set, and a no-echo
    /// context refuses prediction outright — the alternate screen (vim/less/
    /// htop) OR an app-owned Kitty composer (REPORT_EVENT_TYPES /
    /// REPORT_ALL_KEYS_AS_ESC). While scrolled into history only the expiry
    /// self-heal runs: guesses live in ACTIVE-grid coords, so the scrollback
    /// view is never reconciled against them (the native discipline).
    pub fn predict_reconcile(&mut self) {
        let now = Instant::now();
        if self.term.grid().display_offset() != 0 {
            // Expiry flush only — a guess in flight when the user scrolled up
            // must still self-heal, or `predict_next_deadline_ms` stays pinned
            // at a past instant forever.
            let _ = self.predict.overlay(now);
            return;
        }
        let cur = self.term.cursor();
        // The native no-echo gate: alt screen OR app-owned Kitty composer.
        // Read-only display projection — never a byte producer.
        let no_echo =
            self.term.is_alternate_screen() || self.term.kitty_suppresses_predictive_echo();
        let (term, predict) = (&self.term, &mut self.predict);
        predict.reconcile(Some((cur.row, cur.col)), no_echo, now, |r, c| {
            // The native observe: the cell's glyph, with blank/space mapped to
            // None (a typed space confirms by cursor advance instead).
            term.render_row(r as usize)
                .get(c as usize)
                .map(|cell| cell.ch)
                .filter(|ch| *ch != ' ')
        });
    }

    /// The ghost cells to paint THIS frame, as flat `[row, col, codepoint]`
    /// triples (a `Uint32Array` in JS). The host renders them tentatively
    /// (dim/underline) and may advance its DRAWN cursor past the last one,
    /// mosh-style. Runs the expiry self-heal first, then the display gate:
    /// `always` ⇒ all pending; `adaptive` ⇒ all pending after an echo is confirmed
    /// on this line and measured RTT is high enough to help. Empty in app-owned
    /// Kitty composers and while scrolled into history.
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

    /// Milliseconds until the oldest pending guess self-expires (the glitch
    /// flush), or `undefined` when none is pending. Arm ONE timer for this and
    /// call [`predict_overlay`](Self::predict_overlay) + repaint there, so a
    /// stale ghost is erased even when no further input or output arrives.
    pub fn predict_next_deadline_ms(&self) -> Option<f64> {
        self.predict
            .next_deadline()
            .map(|d| d.saturating_duration_since(Instant::now()).as_secs_f64() * 1000.0)
    }

    /// Drop all in-flight guesses because this SAME terminal's coordinate space
    /// changed (`resize` calls this automatically). The confirmation epoch is
    /// forgotten, while this session's learned link RTT remains useful.
    pub fn predict_reset(&mut self) {
        // ICF barrier — see aterm-wasm/src/predict_api.rs: identical-body shims get
        // merged by LLVM and wasm-bindgen then cross-binds the JS methods.
        std::hint::black_box(0u8);
        self.predict.reset();
    }

    /// Reset for a DIFFERENT pane/session. In addition to coordinate-bound
    /// guesses, forget the learned echo RTT so a slow remote pane cannot make a
    /// newly selected local pane display speculation. Hosts that keep one
    /// `AtermGpuTerminal` per session never need this; pane-reusing hosts call it
    /// at the identity switch.
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

        // Resize/coordinate reset retains this terminal's link estimate and
        // stable slow-link classification. A slow confirmation in the new
        // coordinate epoch therefore makes its next prediction visible.
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
        // The native note_line_submit seam through the export: after Enter, a
        // non-echoing prompt on the SAME physical row (a terminal scrolled to
        // the bottom reuses its last row across logical lines) must not
        // inherit the previous command's confirmation and flash a secret.
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
            "submit flushes pending guesses"
        );
        assert!(t.predict_char('s'), "the password keystroke is tracked…");
        assert!(
            t.predict_overlay().is_empty(),
            "…but never displayed on the unconfirmed new line (no leak)"
        );
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
        // Disabled with a guess still in flight: overlay empties AND no
        // deadline stays armed (the stranded-timer regression).
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
