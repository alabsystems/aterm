// SPDX-License-Identifier: MIT
// Copyright 2026 Andrew Yates

//! The host-facing predictive-echo API on [`WebTerminal`](crate::WebTerminal) — mosh-style
//! speculative typing, driven by the shared [`aterm_predict`] state machine
//! (the SAME predictor the native app runs — no forked heuristics).
//!
//! The host seams (the crate's own contract):
//! 1. [`predict_char`](crate::WebTerminal::predict_char) /
//!    [`predict_backspace`](crate::WebTerminal::predict_backspace) on the keydown
//!    whose bytes the host writes to the PTY, and
//!    [`predict_line_submit`](crate::WebTerminal::predict_line_submit) on a key that
//!    writes a line terminator (Enter in any spelling, Ctrl+J, Ctrl+M), and
//!    [`predict_unmodelled_input`](crate::WebTerminal::predict_unmodelled_input) on
//!    EVERY other key whose bytes reach the PTY and on a paste (the epoch
//!    boundaries — the password-prompt safety seam),
//! 2. [`predict_reconcile`](crate::WebTerminal::predict_reconcile) after `process()`
//!    applies a chunk of child output to the grid,
//! 3. [`predict_overlay`](crate::WebTerminal::predict_overlay) when composing a
//!    frame (the tentative ghost cells the host paints), and
//! 4. [`predict_reset`](crate::WebTerminal::predict_reset) when only the coordinate
//!    space changes (`resize` calls it automatically), and
//!    [`predict_session_reset`](crate::WebTerminal::predict_session_reset) when the
//!    host reuses this wrapper for a different pane/session.
//!
//! OFF by default: until `set_predictive_echo("adaptive" | "always")` every
//! entry point is inert and rendering is untouched — the same fail-safe
//! `PredictMode::parse` domain as the native `predictive_echo` config knob.
//!
//! Unlike the effects clock (a deterministic host-advanced `advance_effects`
//! stream), this module samples the REAL monotonic clock (`aterm_time::Instant`:
//! std on native, `performance.now()` on wasm32). The predictor's glitch-expiry
//! self-heal is a wall-clock window (an unechoed guess must vanish ~250 ms
//! after the real keypress) — the same live-clock seam the native event loop
//! wires. The state machine itself stays pure/clock-injected; deterministic
//! timing coverage lives in the `aterm-predict` crate tests.
//!
//! Shared with `aterm-gpu-web`, which mounts this file by `#[path]`;
//! [`WebTerminal`](crate::WebTerminal) is each crate's own terminal type.

use aterm_core::terminal::{RenderCell, Terminal};
use aterm_predict::{KeySample, PredictMode, Seen};
use aterm_time::Instant;

/// The predictor's resident row buffers on a web terminal: one extracted row
/// (cells, emoji clusters, combining marks) that the reconcile's observe reads
/// from, and the cursor row a run's first keypress hands the predictor.
#[derive(Default)]
pub(crate) struct PredRows {
    cells: Vec<RenderCell>,
    clusters: Vec<(usize, Box<str>)>,
    combining: Vec<(usize, Box<[char]>)>,
    /// Whether the row in `cells` is drawn in logical order at single width.
    logical: bool,
    key: Vec<Seen>,
}

impl PredRows {
    /// Refill the extracted row from visible row `row` of `term`: the frame's
    /// own extraction of it (`Terminal::logical_row_into`).
    fn read(&mut self, term: &Terminal, row: u16) {
        self.logical = term.logical_row_into(
            usize::from(row),
            &mut self.cells,
            &mut self.clusters,
            &mut self.combining,
        );
    }

    /// Column `col` of the extracted row as the predictor reads it: its glyph,
    /// OPAQUE on either half of a wide glyph, on a cluster or combining marks, and
    /// anywhere on a row drawn at DEC double size; a column past a trimmed row is
    /// the implicit blank. The press's row and the reconcile's observe both read
    /// through here, so the tail guard compares like with like (aterm-gui's
    /// `predict_seen` is the native twin).
    fn seen(&self, col: usize) -> Seen {
        let Some(cell) = self.cells.get(col) else {
            return Seen::Blank;
        };
        let marked = self.clusters.iter().any(|(at, _)| *at == col)
            || self.combining.iter().any(|(at, _)| *at == col);
        let opaque = !self.logical
            || cell.wide
            || self
                .cells
                .get(col.saturating_add(1))
                .is_some_and(|next| next.wide)
            || marked;
        Seen::of_cell(cell.ch, opaque)
    }
}

/// The JS-visible surface. Each crate expands it onto its own terminal
/// type, under its own export attribute, with `web_terminal_impl!`: the
/// attribute names the JS class after the impl's type, which this shared
/// file cannot spell.
macro_rules! bind {
    ($terminal:ident, $export:ident) => {
        #[cfg_attr(target_arch = "wasm32", $export)]
        impl $terminal {
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
            /// right margin. A run's first glyph also hands the predictor the cursor
            /// row, which its tail guard compares the run's echoes with. Returns
            /// whether a guess is now TRACKED — display is a separate gate (see
            /// [`predict_overlay`](Self::predict_overlay)).
            pub fn predict_char(&mut self, ch: char) -> bool {
                if self.term.is_alternate_screen() || self.term.kitty_suppresses_predictive_echo() {
                    // Not guessed, but sent: the app may leave the alternate screen
                    // before it reads the key and the shell echo it, so its echo is
                    // owed (the native no-echo gate does the same).
                    self.predict.note_unmodelled_input(Instant::now());
                    return false;
                }
                let cur = self.term.cursor();
                let cols = self.term.grid().cols();
                let coords = self.term.content_scroll_state().coordinate_generation();
                let rows = &mut self.pred_row_scratch;
                // The cursor row, read as the reconcile reads it, only where it is
                // one: `logical_row_into` reads the VIEW, so with the page scrolled
                // back it is a history row, which the tail guard would take for the
                // line the cursor is on (and a history row showing the same command
                // after the insert passed a mid-line echo for an append). With no
                // row the run proves only over a blank tail. Nor in `Off`, where
                // nothing is guessed and the row would be read for nothing.
                let has_row = self.predict.mode() != PredictMode::Off
                    && self.term.grid().display_offset() == 0
                    && self.predict.wants_key_row();
                if has_row {
                    rows.read(&self.term, cur.row);
                    let mut key = std::mem::take(&mut rows.key);
                    key.clear();
                    key.extend((0..usize::from(cols)).map(|c| rows.seen(c)));
                    rows.key = key;
                }
                // `rows = 0`: no row below to wrap into, so the margin declines
                // (the height-blind `predict_char`).
                let sample = KeySample {
                    cursor: (cur.row, cur.col),
                    coords,
                    grid: (cols, 0),
                    row: has_row.then_some(rows.key.as_slice()),
                };
                self.predict.predict_glyph(ch, &sample, Instant::now())
            }

            /// Register a Backspace: cancels our OWN trailing guess only (erasing
            /// already-committed real content is left to the program's echo, and ends
            /// the confirmation epoch). Returns whether a guess was cancelled.
            pub fn predict_backspace(&mut self) -> bool {
                if self.term.is_alternate_screen() || self.term.kitty_suppresses_predictive_echo() {
                    // Sent all the same, and owed (see `predict_char`).
                    self.predict.note_unmodelled_input(Instant::now());
                    return false;
                }
                self.predict.predict_backspace(Instant::now())
            }

            /// Register a line terminator going to the PTY: Enter (CR, or Shift+Enter's
            /// LF), Ctrl+J, Ctrl+M — not Alt+Enter, whose ESC prefix makes it any other
            /// key. Ends the confirmation epoch: nothing typed after it displays
            /// (`adaptive`) until an echo is confirmed again. LOAD-BEARING for password
            /// safety: a password typed ahead of the prompt that follows still sits on
            /// the proven row. The line's own unechoed tail keeps painting until its echo
            /// (the native `note_line_submit` seam). Cheap.
            pub fn predict_line_submit(&mut self) {
                self.predict.note_line_submit(Instant::now());
            }

            /// Register any OTHER key the host did not hand to
            /// [`predict_char`](Self::predict_char) /
            /// [`predict_backspace`](Self::predict_backspace) — Tab, an arrow, a control
            /// or Alt chord, an IME commit — and a paste: call when its bytes go to the
            /// PTY. Ends the confirmation epoch like
            /// [`predict_line_submit`](Self::predict_line_submit), and also retires the
            /// guesses in flight: the key may erase them (Ctrl+W, Ctrl+H), and a guess
            /// left over the erased cell is a ghost of the erased text. Which keys submit
            /// a line is the program's call, so this is the seam for every key that is
            /// not a terminator (the native `note_unmodelled_input`). Cheap.
            pub fn predict_unmodelled_input(&mut self) {
                // ICF barrier (see `predict_reset`): keep this export's body distinct.
                std::hint::black_box(2u8);
                self.predict.note_unmodelled_input(Instant::now());
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
                // `reconcile` probes once per retired guess plus the head, and the
                // predictor is a same-row model (its tail guard reads the rest of the
                // row) — so extracting the row per probe would rebuild the identical
                // row (fresh Vec + full colour/decoration resolve over every column) N
                // times per call. Memoize by row into the resident scratch, refilled
                // in place with the frame's own row extraction
                // (`Terminal::logical_row_into`), so observations are unchanged. This
                // is the fix aterm-gui already carries.
                let mut scratch = std::mem::take(&mut self.pred_row_scratch);
                let mut cached: Option<u16> = None;
                // Rows that moved under the guesses (output scrolled the screen) retire
                // them and their epoch inside the predictor.
                let coords = self.term.content_scroll_state().coordinate_generation();
                let (term, predict) = (&self.term, &mut self.predict);
                predict.reconcile_seen(Some((cur.row, cur.col)), coords, no_echo, now, |r, c| {
                    if cached != Some(r) {
                        scratch.read(term, r);
                        cached = Some(r);
                    }
                    // The native observe: the cell's glyph (a blank or space is
                    // `Blank`, and a typed space confirms by cursor advance
                    // instead), opaque where its glyph does not describe it.
                    scratch.seen(usize::from(c))
                });
                self.pred_row_scratch = scratch;
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
                // ICF barrier: `reset`, `note_line_submit` and `note_unmodelled_input` could
                // compile to identical bodies, and LLVM MergeFunctions folds the exported
                // shims — after which wasm-bindgen bound BOTH JS methods to the one surviving
                // export (observed: generated glue called predict_line_submit from
                // predict_reset). Harmless while semantics coincide; a silent misroute the
                // day they diverge. black_box keeps this shim's body distinct so the exports
                // stay separate.
                std::hint::black_box(0u8);
                self.predict.reset();
            }

            /// Reset for a DIFFERENT pane/session. In addition to coordinate-bound
            /// guesses, forget the learned echo RTT so a slow remote pane cannot make a
            /// newly selected local pane display speculation. Hosts that keep one
            /// `WebTerminal` per session never need this; pane-reusing hosts call it at
            /// the identity switch.
            pub fn predict_session_reset(&mut self) {
                // Keep this export distinct from the coordinate-only reset and line-submit
                // shims under wasm LLVM function merging.
                std::hint::black_box(1u8);
                self.predict.reset_session();
            }
        }
    };
}
web_terminal_impl!(bind);

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use crate::WebTerminal;

    /// A terminal on the system font, or COULD NOT RUN where the host has
    /// none (the same posture as the other binding tests).
    fn terminal() -> WebTerminal {
        crate::system_font_terminal(4, 20, 16.0)
    }

    #[test]
    fn off_by_default_and_unknown_modes_fail_safe() {
        let mut t = terminal();
        assert!(!t.predict_char('a'), "default off ⇒ inert");
        t.set_predictive_echo("nonsense");
        assert!(!t.predict_char('a'), "unknown mode parses to off");
        assert!(t.predict_next_deadline_ms().is_none(), "nothing armed");
    }

    #[test]
    fn always_tracks_paints_and_retires_through_the_export() {
        let mut t = terminal();
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
        let mut t = terminal();
        t.set_predictive_echo("adaptive");
        assert!(t.predict_char('a'), "tracked, awaiting the echo");
        assert!(
            t.predict_overlay().is_empty(),
            "no echo confirmed on this line yet ⇒ nothing painted (password safety)"
        );
    }

    #[test]
    fn adaptive_confirmed_echo_retires_the_prediction() {
        let mut t = terminal();
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
    fn codex_report_event_types_suppresses_arm_reconcile_and_display() {
        let mut t = terminal();
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
        let mut t = terminal();
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
        // 'b' is still in flight, so the live cursor stands on it: the keystroke
        // typed behind the submit has no cell until the terminator's echo decides
        // one, and is not even armed (`aterm-predict`'s one-guess-per-cell rule).
        assert!(
            !t.predict_char('s'),
            "the password keystroke behind a submitted line in flight is declined…"
        );
        assert!(
            t.predict_overlay().is_empty(),
            "…and never displayed on the unconfirmed new line (no leak)"
        );
    }

    #[test]
    fn only_a_line_terminator_carries_the_guesses_in_flight() {
        // The two boundary exports differ in what they do to the pixels already
        // painted: a terminator keeps the line's unechoed tail, any other key
        // (Ctrl+W, Ctrl+H) may have erased it and retires it.
        let mut t = terminal();
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
        let mut t = terminal();
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
        let mut t = terminal();
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
        let mut t = terminal();
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

    /// A run's first press hands the predictor the cursor row, the tail guard's
    /// reference: a right prompt the echo leaves in place proves the line. Read
    /// as nothing (no row), the right prompt would be text that appeared beside
    /// the echo, and the line would prove nothing.
    #[test]
    fn a_run_s_first_press_hands_the_predictor_its_row() {
        let mut t = terminal();
        t.set_predictive_echo("always");
        // `$ ` and a right prompt at columns 15..18, the cursor after the prompt.
        t.process(b"$ \x1b[16G<r>\x1b[1;3H");
        assert!(t.predict_char('a'));
        t.process(b"a");
        t.predict_reconcile();
        assert!(t.predict_overlay().is_empty(), "retired by its echo");
        let events = t.predict.events_for_test();
        assert_eq!(
            events.decorated_proofs, 1,
            "proved over the right prompt: {events:?}"
        );
        assert_eq!(events.tail_refusals, 0, "{events:?}");
    }

    /// A press with the page scrolled back hands no row: the view's row at the
    /// cursor's index is history, not the line the cursor is on. Here the history
    /// row is the same prompt line, which, taken for the cursor row, proved the
    /// echo over a right prompt it never compared; with no row the line proves
    /// only over a blank tail, and the right prompt beside the echo refuses it.
    #[test]
    fn a_press_while_scrolled_back_hands_no_row() {
        let mut t = terminal();
        for _ in 0..5 {
            t.process(b"$ \x1b[16G<r>\r\n");
        }
        t.process(b"$ \x1b[16G<r>\x1b[3G");
        t.set_predictive_echo("always");
        t.scroll_lines(1);
        assert!(t.predict_char('a'));
        t.scroll_to_bottom();
        t.process(b"a");
        t.predict_reconcile();
        assert!(t.predict_overlay().is_empty(), "retired by its echo");
        let events = t.predict.events_for_test();
        assert_eq!(
            events.decorated_proofs, 0,
            "no history row compared: {events:?}"
        );
        assert_eq!(events.tail_refusals, 1, "{events:?}");
    }

    #[test]
    fn resize_drops_inflight_guesses() {
        let mut t = terminal();
        t.set_predictive_echo("always");
        t.predict_char('a');
        t.resize(10, 40);
        assert!(t.predict_overlay().is_empty(), "stale-coords guess dropped");
        assert!(t.predict_next_deadline_ms().is_none());
    }
}
