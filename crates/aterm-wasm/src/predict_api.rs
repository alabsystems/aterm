// SPDX-License-Identifier: MIT
// Copyright 2026 Andrew Yates

//! Predictive echo on [`WebTerminal`](crate::WebTerminal): mosh-style speculative typing
//! driven by the [`aterm_predict`] state machine the native app runs.
//!
//! Host seams:
//! 1. [`predict_char`](crate::WebTerminal::predict_char) /
//!    [`predict_edit_key`](crate::WebTerminal::predict_edit_key) (a bare `←`, `→`, or
//!    Backspace, also [`predict_backspace`](crate::WebTerminal::predict_backspace))
//!    on the keydown whose bytes the host writes to the PTY, and
//!    [`predict_line_submit`](crate::WebTerminal::predict_line_submit) on a key that
//!    writes a line terminator (Enter in any spelling, Ctrl+J, Ctrl+M), and
//!    [`predict_unmodelled_input`](crate::WebTerminal::predict_unmodelled_input) on
//!    EVERY other key whose bytes reach the PTY and on a paste (the epoch
//!    boundaries — the password-prompt safety seam),
//! 2. [`predict_reconcile`](crate::WebTerminal::predict_reconcile) after `process()`
//!    applies a chunk of child output to the grid,
//! 3. [`predict_overlay`](crate::WebTerminal::predict_overlay) when composing a
//!    frame (the tentative ghost cells the host paints), with
//!    [`predict_overlay_cursor`](crate::WebTerminal::predict_overlay_cursor) (an
//!    edit's cursor, and the sign to paint its cells all or none), and
//! 4. [`predict_reset`](crate::WebTerminal::predict_reset) when only the coordinate
//!    space changes (`resize` calls it automatically), and
//!    [`predict_session_reset`](crate::WebTerminal::predict_session_reset) when the
//!    wrapper is reused for another pane.
//!
//! Off by default: until `set_predictive_echo("adaptive" | "always")` every entry
//! point is inert. Unlike the deterministic effects clock, this module samples the
//! real monotonic clock (`aterm_time::Instant`, through [`clock`]): an unechoed guess
//! must vanish within the predictor's RTT-relative glitch window. A test pins that
//! clock ([`pin_clock`]), so no stall of its thread ages a guess past the window;
//! deterministic timing coverage lives in `aterm-predict`.
//!
//! `aterm-gpu-web` mounts this file by `#[path]`; `WebTerminal` is each crate's own
//! terminal type.

use aterm_core::terminal::{RenderCell, Terminal};
use aterm_predict::{EditKey, EditOutcome, KeySample, PredictMode, Seen};
use aterm_time::Instant;

/// The predictor's one clock read, for every export below: the real monotonic
/// clock, or the instant a test pinned ([`pin_clock`]).
#[inline]
fn clock() -> Instant {
    #[cfg(test)]
    if let Some(at) = PINNED_CLOCK.with(std::cell::Cell::get) {
        return at;
    }
    Instant::now()
}

#[cfg(test)]
thread_local! {
    /// The pinned clock ([`clock`]); `None` reads the real one.
    static PINNED_CLOCK: std::cell::Cell<Option<Instant>> = const { std::cell::Cell::new(None) };
}

/// Pin (or, with `None`, release) this thread's predictor clock ([`clock`]).
#[cfg(test)]
fn pin_clock(at: Option<Instant>) {
    PINNED_CLOCK.with(|clock| clock.set(at));
}

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
    #[inline]
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
    #[inline]
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

/// The JS-visible surface, which each crate's `web_terminal_impl!` expands onto its
/// own terminal type under its own export attribute: the attribute names the JS class
/// after the impl's type, which this shared file cannot spell.
macro_rules! bind {
    ($terminal:ident, $export:ident) => {
        #[cfg_attr(target_arch = "wasm32", $export)]
        impl $terminal {
            /// Set the predictive-echo mode: `"off"` (default), `"adaptive"` (show once the
            /// line confirms echo and measured RTT is high enough to benefit) or `"always"`.
            /// Case-insensitive; unknown strings fail safe to `off`.
            pub fn set_predictive_echo(&mut self, mode: &str) {
                let mode = PredictMode::parse(mode);
                // A mode change flushes in-flight guesses; Off also forgets the epoch so no
                // armed deadline outlives a disable.
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
                    self.predict.note_unmodelled_input(clock());
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
                    // The web host reads no signed prompt marks: no buffer start, so
                    // no line is ever witnessed (Home, End, Ctrl-A, Ctrl-E and Delete
                    // stay input the predictor does not model), and one editor context.
                    input_start: None,
                    // The engine's own deferred-wrap flag (a cursor on a filled
                    // last column is parked only after the terminal wrote it).
                    wrap_pending: Some(self.term.grid().pending_wrap()),
                    ..KeySample::default()
                };
                self.predict.predict_glyph(ch, &sample, clock())
            }

            /// Register a Backspace: it cancels our OWN trailing guess, or, on a line
            /// that has proven it echoes, is predicted as an erase at the end of the
            /// line (see [`predict_edit_key`](Self::predict_edit_key), code 3); erasing
            /// anything else is left to the program's echo, and ends the confirmation
            /// epoch. Returns whether a guess was cancelled.
            pub fn predict_backspace(&mut self) -> bool {
                self.predict_edit_key(3) == 2
            }

            /// Register a line-editing key the host just wrote to the PTY: `code` 1
            /// is a bare `←`, 2 a bare `→`, 3 Backspace (any other code is ignored and
            /// answers 0). Hand it HERE, not to
            /// [`predict_unmodelled_input`](Self::predict_unmodelled_input): every
            /// outcome accounts for the key. On a line that has watched its own
            /// echoes and proved them, the key is armed as an edit of the line the
            /// predictor knows: in `adaptive` a Backspace shows on that proof, an
            /// arrow once a motion of the line has also echoed exactly, and in
            /// `always` both show at once; one it cannot place is declined, an
            /// arrow with the guesses in flight kept. Answers 0 (declined), 1 (armed) or 2 (a Backspace that cancelled
            /// our own trailing guess). The web host has no signed prompt marks, so
            /// Home, End, Ctrl-A, Ctrl-E and Delete, which the predictor places only at
            /// a prompt whose buffer start the shell authenticated, stay unmodelled
            /// input (`predict_unmodelled_input`), as they always were.
            pub fn predict_edit_key(&mut self, code: u32) -> u32 {
                // ICF barrier (see `predict_reset`): keep this export's body distinct.
                std::hint::black_box(3u8);
                let key = match code {
                    1 => EditKey::Left,
                    2 => EditKey::Right,
                    3 => EditKey::Backspace,
                    _ => return 0,
                };
                if self.term.is_alternate_screen() || self.term.kitty_suppresses_predictive_echo() {
                    // Sent all the same, and owed (see `predict_char`).
                    self.predict.note_unmodelled_input(clock());
                    return 0;
                }
                let cur = self.term.cursor();
                let cols = self.term.grid().cols();
                let coords = self.term.content_scroll_state().coordinate_generation();
                let rows = &mut self.pred_row_scratch;
                // An edit key is placed only on a row the predictor can read, as
                // `predict_char` reads it (never a scrolled-back history row).
                let has_row = self.predict.mode() != PredictMode::Off
                    && self.term.grid().display_offset() == 0;
                if has_row {
                    rows.read(&self.term, cur.row);
                    let mut key = std::mem::take(&mut rows.key);
                    key.clear();
                    key.extend((0..usize::from(cols)).map(|c| rows.seen(c)));
                    rows.key = key;
                }
                let sample = KeySample {
                    cursor: (cur.row, cur.col),
                    coords,
                    grid: (cols, 0),
                    row: has_row.then_some(rows.key.as_slice()),
                    // The web host reads no signed prompt marks: no buffer start, so
                    // no line is ever witnessed (Home, End, Ctrl-A, Ctrl-E and Delete
                    // stay input the predictor does not model), and one editor context.
                    input_start: None,
                    // The engine's own deferred-wrap flag (a cursor on a filled
                    // last column is parked only after the terminal wrote it).
                    wrap_pending: Some(self.term.grid().pending_wrap()),
                    ..KeySample::default()
                };
                match self.predict.predict_edit(key, &sample, clock()) {
                    EditOutcome::Declined => 0,
                    EditOutcome::Armed => 1,
                    EditOutcome::Cancelled => 2,
                }
            }

            /// Register a line terminator going to the PTY: Enter (CR, or Shift+Enter's LF),
            /// Ctrl+J, Ctrl+M; not Alt+Enter, whose ESC prefix makes it any other key. Ends the
            /// confirmation epoch, so nothing typed after it displays (`adaptive`) until an
            /// echo is confirmed again: a password typed ahead of the next prompt never
            /// paints. The line's own unechoed tail keeps painting until its echo.
            pub fn predict_line_submit(&mut self) {
                self.predict.note_line_submit(clock());
            }

            /// Register any other key whose bytes go to the PTY (Tab, an arrow, a control or
            /// Alt chord, an IME commit) and a paste. Ends the confirmation epoch like
            /// [`predict_line_submit`](Self::predict_line_submit) and also retires the guesses
            /// in flight, since the key may erase them (Ctrl+W, Ctrl+H).
            pub fn predict_unmodelled_input(&mut self) {
                // ICF barrier (see `predict_reset`): keep this export's body distinct.
                std::hint::black_box(2u8);
                self.predict.note_unmodelled_input(clock());
            }

            /// Reconcile pending guesses against the grid after `process()`. Confirmed leading
            /// guesses retire (arming the display gate), a divergence flushes the set, and a
            /// no-echo context (alternate screen, or a Kitty composer with REPORT_EVENT_TYPES
            /// or REPORT_ALL_KEYS_AS_ESC) refuses prediction. While scrolled into history only
            /// the expiry self-heal runs: guesses are in active-grid coordinates.
            pub fn predict_reconcile(&mut self) {
                let now = clock();
                if self.term.grid().display_offset() != 0 {
                    // Expiry flush only, so a guess in flight when the user scrolled up
                    // still self-heals and `predict_next_deadline_ms` does not stay pinned.
                    let _ = self.predict.overlay(now);
                    return;
                }
                let cur = self.term.cursor();
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
                // Output that scrolled the screen retires the guesses inside the predictor.
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

            /// The ghost cells to paint this frame as flat `[row, col, codepoint]` triples (a
            /// `Uint32Array` in JS), drawn tentatively; the host may advance its drawn cursor
            /// past the last one. Runs the expiry self-heal, then the display gate (`always`:
            /// all pending; `adaptive`: once this line's echo is confirmed and RTT is high
            /// enough). Empty in a no-echo context and while scrolled into history.
            ///
            /// While [`predict_overlay_cursor`](Self::predict_overlay_cursor) is
            /// non-empty these are the cells of an edit's displayed state (a typed
            /// glyph; a glyph a SHIFT moved, typed inside the line, which the native
            /// host paints as the cell it came from and the web paints as any
            /// ghost, its text exact; or U+0020 where an erase vacated a cell: paint
            /// it as the implicit blank): paint ALL of them or none, and draw the
            /// cursor THERE, not past the last cell. A part of a state is a frame no
            /// program draws.
            pub fn predict_overlay(&mut self) -> Vec<u32> {
                if self.term.is_alternate_screen() || self.term.kitty_suppresses_predictive_echo() {
                    self.predict.reset();
                    return Vec::new();
                }
                let shown = self.predict.overlay(clock());
                if self.term.grid().display_offset() != 0 {
                    return Vec::new();
                }
                let mut out = Vec::with_capacity(shown.len() * 3);
                for p in shown {
                    out.extend([u32::from(p.row), u32::from(p.col), u32::from(p.ch)]);
                }
                out
            }

            /// The cursor of the frame [`predict_overlay`](Self::predict_overlay) last
            /// composed when it shows an edit (`[row, col]`): the cursor its displayed
            /// state leaves, which a motion moves with no cell to paint. Empty for
            /// typed-ahead glyphs (draw the cursor past the last of them), when
            /// nothing is shown, in app-owned Kitty composers and while scrolled into
            /// history. Call after `predict_overlay`; draw it only when every cell of
            /// that overlay was painted.
            pub fn predict_overlay_cursor(&self) -> Vec<u32> {
                // ICF barrier (see `predict_reset`): keep this export's body distinct.
                std::hint::black_box(4u8);
                let shown = !self.term.is_alternate_screen()
                    && !self.term.kitty_suppresses_predictive_echo()
                    && self.term.grid().display_offset() == 0;
                self.predict
                    .overlay_cursor()
                    .filter(|_| shown)
                    .map_or_else(Vec::new, |(row, col)| vec![u32::from(row), u32::from(col)])
            }

            /// Milliseconds until the oldest pending guess self-expires, or `undefined` when
            /// none is pending. Arm one timer for it and call
            /// [`predict_overlay`](Self::predict_overlay) + repaint there, so a stale ghost is
            /// erased even when no input or output arrives.
            pub fn predict_next_deadline_ms(&self) -> Option<f64> {
                self.predict
                    .next_deadline()
                    .map(|d| d.saturating_duration_since(clock()).as_secs_f64() * 1000.0)
            }

            /// Drop in-flight guesses because this terminal's coordinate space changed
            /// (`resize` calls it). Forgets the confirmation epoch, keeps the learned RTT.
            pub fn predict_reset(&mut self) {
                // ICF barrier: these export shims can compile to identical bodies, and LLVM
                // MergeFunctions then folds them, binding several JS methods to one export.
                // black_box keeps each body distinct.
                std::hint::black_box(0u8);
                self.predict.reset();
            }

            /// Reset for a different pane/session: also forget the learned echo RTT, so a
            /// slow remote pane cannot make a local one display speculation. Call at the
            /// identity switch when one `WebTerminal` serves several panes.
            pub fn predict_session_reset(&mut self) {
                // ICF barrier (see `predict_reset`).
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
    /// none (the same posture as the other binding tests). The predictor's
    /// clock is pinned for the test: every export reads the same instant,
    /// however long the thread stalls between calls, so no guess expires
    /// behind the test's back.
    fn terminal() -> WebTerminal {
        super::pin_clock(Some(aterm_time::Instant::now()));
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
        // The real echo retires the guess and disarms the deadline.
        t.process(b"a");
        t.predict_reconcile();
        assert!(t.predict_overlay().is_empty());
        assert!(t.predict_next_deadline_ms().is_none());
    }

    /// An edit key goes through `predict_edit_key`: on a line whose glyphs have
    /// echoed, `←` is armed as an edit and paints no cell, its cursor handed over
    /// by `predict_overlay_cursor` until its echo retires it; a Backspace at the
    /// end of the line is an erase, its vacated cell a blank ghost; an unknown
    /// code is ignored.
    #[test]
    fn an_edit_key_is_armed_and_its_cursor_handed_over() {
        let mut t = terminal();
        t.set_predictive_echo("always");
        for ch in ['a', 'b'] {
            assert!(t.predict_char(ch));
            t.process(ch.to_string().as_bytes());
            t.predict_reconcile();
        }
        assert_eq!(t.predict_edit_key(0), 0, "an unknown code is ignored");
        assert_eq!(t.predict_edit_key(1), 1, "`←` armed on the proven line");
        assert!(t.predict_overlay().is_empty(), "a motion paints no cell");
        assert_eq!(t.predict_overlay_cursor(), vec![0, 1]);
        t.process(b"\x08");
        t.predict_reconcile();
        assert!(t.predict_overlay().is_empty());
        assert!(t.predict_overlay_cursor().is_empty(), "its echo retired it");
        assert_eq!(t.predict_edit_key(2), 1, "`→` back to the end");
        t.process(b"\x1b[C");
        t.predict_reconcile();
        assert_eq!(t.predict_edit_key(3), 1, "an erase at the end of the line");
        assert_eq!(t.predict_overlay(), vec![0, 1, u32::from(' ')]);
        assert_eq!(t.predict_overlay_cursor(), vec![0, 1]);
        assert!(
            !t.predict_backspace(),
            "a second erase arms; it cancels nothing"
        );
        // Off is inert at once: no cursor for the flushed chain even before the
        // next overlay, and an edit key arms nothing.
        t.set_predictive_echo("off");
        assert!(t.predict_overlay_cursor().is_empty());
        assert_eq!(t.predict_edit_key(1), 0);
        assert!(t.predict_next_deadline_ms().is_none());
    }

    /// A glyph typed inside a line whose glyphs have echoed is a SHIFT: the
    /// overlay is the state it leaves, the typed glyph and the text it moved,
    /// each a `[row, col, codepoint]` triple, with the state's cursor; the echo
    /// that writes the glyph, the shifted tail and steps back retires it.
    #[test]
    fn a_shift_s_moved_text_comes_out_as_triples() {
        let mut t = terminal();
        t.set_predictive_echo("always");
        for ch in ['a', 'b', 'c'] {
            assert!(t.predict_char(ch));
            t.process(ch.to_string().as_bytes());
            t.predict_reconcile();
        }
        for _ in 0..2 {
            assert_eq!(t.predict_edit_key(1), 1, "`←` armed");
            t.process(b"\x08");
            t.predict_reconcile();
        }
        assert!(t.predict_char('x'), "a shift, armed");
        assert_eq!(
            t.predict_overlay(),
            vec![
                0,
                1,
                u32::from('x'),
                0,
                2,
                u32::from('b'),
                0,
                3,
                u32::from('c')
            ]
        );
        assert_eq!(t.predict_overlay_cursor(), vec![0, 2]);
        t.process(b"xbc\x08\x08");
        t.predict_reconcile();
        assert!(t.predict_overlay().is_empty(), "its echo retired it");
        assert!(t.predict_overlay_cursor().is_empty());
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

        // Arm gate: while Codex owns the composer nothing is tracked or armed.
        assert!(!t.predict_char('c'));
        assert!(!t.predict_backspace());
        assert!(t.predict_next_deadline_ms().is_none());
    }

    #[test]
    fn line_submit_ends_the_epoch_no_same_row_leak() {
        // After Enter, a non-echoing prompt on the same physical row must not
        // inherit the previous command's confirmation.
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
        // A terminator keeps the line's unechoed tail; any other key may have
        // erased it (Ctrl+W, Ctrl+H) and retires it.
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
        // Disabling with a guess in flight leaves no deadline armed.
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
        t.resize(10, 40).expect("admitted geometry");
        assert!(t.predict_overlay().is_empty(), "stale-coords guess dropped");
        assert!(t.predict_next_deadline_ms().is_none());
    }
}
