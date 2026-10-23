// SPDX-License-Identifier: MIT
// Copyright 2026 Andrew Yates

//! Predictive echo on [`WebTerminal`](crate::WebTerminal): mosh-style speculative typing driven by the [`aterm_predict`] state machine the native app runs.

use aterm_core::terminal::{RenderCell, Terminal};
use aterm_predict::{EditKey, EditOutcome, KeySample, PredictMode, Seen};
use aterm_time::Instant;

/// The predictor's one clock read, for every export below: the real monotonic
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
    /// The pinned clock ([`clock`]); `None` reads the real one.
    static PINNED_CLOCK: std::cell::Cell<Option<Instant>> = const { std::cell::Cell::new(None) };
}

/// Pin (or, with `None`, release) this thread's predictor clock ([`clock`]).
#[cfg(test)]
fn pin_clock(at: Option<Instant>) {
    PINNED_CLOCK.with(|clock| clock.set(at));
}

/// The predictor's resident row buffers on a web terminal: one extracted row (cells, emoji clusters, combining marks) that the reconcile's observe reads from, and the cursor row a run's first keypress hands the predictor.
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

/// The JS-visible surface, which each crate's `web_terminal_impl!` expands onto its own terminal type under its own export attribute: the attribute names the JS class after the impl's type, which this shared file cannot spell.
macro_rules! bind {
    ($terminal:ident, $export:ident) => {
        #[cfg_attr(target_arch = "wasm32", $export)]
        impl $terminal {
            /// Set the predictive-echo mode: `"off"` (default), `"adaptive"` (show once the line confirms echo and measured RTT is high enough to benefit) or `"always"`.
            pub fn set_predictive_echo(&mut self, mode: &str) {
                let mode = PredictMode::parse(mode);
                self.predict.set_mode(mode);
                if mode == PredictMode::Off {
                    self.predict.reset();
                }
            }

            /// Register a printable character the host just wrote to the PTY (the keydown seam — call beside `encode_key`).
            pub fn predict_char(&mut self, ch: char) -> bool {
                if self.term.is_alternate_screen() || self.term.kitty_suppresses_predictive_echo() {
                    self.predict.note_unmodelled_input(clock());
                    return false;
                }
                let cur = self.term.cursor();
                let cols = self.term.grid().cols();
                let coords = self.term.content_scroll_state().coordinate_generation();
                let rows = &mut self.pred_row_scratch;
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
                let sample = KeySample {
                    cursor: (cur.row, cur.col),
                    coords,
                    grid: (cols, 0),
                    row: has_row.then_some(rows.key.as_slice()),
                    input_start: None,
                    wrap_pending: Some(self.term.grid().pending_wrap()),
                    ..KeySample::default()
                };
                self.predict.predict_glyph(ch, &sample, clock())
            }

            /// Register a Backspace: it cancels our OWN trailing guess, or, on a line that has proven it echoes, is predicted as an erase at the end of the line (see [`predict_edit_key`](Self::predict_edit_key), code 3); erasing anything else is left to the program's echo, and ends the confirmation epoch.
            pub fn predict_backspace(&mut self) -> bool {
                self.predict_edit_key(3) == 2
            }

            /// Register a line-editing key the host just wrote to the PTY: `code` 1 is a bare `←`, 2 a bare `→`, 3 Backspace (any other code is ignored and answers 0).
            pub fn predict_edit_key(&mut self, code: u32) -> u32 {
                std::hint::black_box(3u8);
                let key = match code {
                    1 => EditKey::Left,
                    2 => EditKey::Right,
                    3 => EditKey::Backspace,
                    _ => return 0,
                };
                if self.term.is_alternate_screen() || self.term.kitty_suppresses_predictive_echo() {
                    self.predict.note_unmodelled_input(clock());
                    return 0;
                }
                let cur = self.term.cursor();
                let cols = self.term.grid().cols();
                let coords = self.term.content_scroll_state().coordinate_generation();
                let rows = &mut self.pred_row_scratch;
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
                    input_start: None,
                    wrap_pending: Some(self.term.grid().pending_wrap()),
                    ..KeySample::default()
                };
                match self.predict.predict_edit(key, &sample, clock()) {
                    EditOutcome::Declined => 0,
                    EditOutcome::Armed => 1,
                    EditOutcome::Cancelled => 2,
                }
            }

            /// Register a line terminator going to the PTY: Enter (CR, or Shift+Enter's LF), Ctrl+J, Ctrl+M; not Alt+Enter, whose ESC prefix makes it any other key.
            pub fn predict_line_submit(&mut self) {
                self.predict.note_line_submit(clock());
            }

            /// Register any other key whose bytes go to the PTY (Tab, an arrow, a control or Alt chord, an IME commit) and a paste.
            pub fn predict_unmodelled_input(&mut self) {
                std::hint::black_box(2u8);
                self.predict.note_unmodelled_input(clock());
            }

            /// Reconcile pending guesses against the grid after `process()`.
            pub fn predict_reconcile(&mut self) {
                let now = clock();
                if self.term.grid().display_offset() != 0 {
                    let _ = self.predict.overlay(now);
                    return;
                }
                let cur = self.term.cursor();
                let no_echo =
                    self.term.is_alternate_screen() || self.term.kitty_suppresses_predictive_echo();
                let mut scratch = std::mem::take(&mut self.pred_row_scratch);
                let mut cached: Option<u16> = None;
                let coords = self.term.content_scroll_state().coordinate_generation();
                let (term, predict) = (&self.term, &mut self.predict);
                predict.reconcile_seen(Some((cur.row, cur.col)), coords, no_echo, now, |r, c| {
                    if cached != Some(r) {
                        scratch.read(term, r);
                        cached = Some(r);
                    }
                    scratch.seen(usize::from(c))
                });
                self.pred_row_scratch = scratch;
            }

            /// The ghost cells to paint this frame as flat `[row, col, codepoint]` triples (a `Uint32Array` in JS), drawn tentatively; the host may advance its drawn cursor past the last one.
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

            /// The cursor of the frame [`predict_overlay`](Self::predict_overlay) last composed when it shows an edit (`[row, col]`): the cursor its displayed state leaves, which a motion moves with no cell to paint.
            pub fn predict_overlay_cursor(&self) -> Vec<u32> {
                std::hint::black_box(4u8);
                let shown = !self.term.is_alternate_screen()
                    && !self.term.kitty_suppresses_predictive_echo()
                    && self.term.grid().display_offset() == 0;
                self.predict
                    .overlay_cursor()
                    .filter(|_| shown)
                    .map_or_else(Vec::new, |(row, col)| vec![u32::from(row), u32::from(col)])
            }

            /// Milliseconds until the oldest pending guess self-expires, or `undefined` when none is pending.
            pub fn predict_next_deadline_ms(&self) -> Option<f64> {
                self.predict
                    .next_deadline()
                    .map(|d| d.saturating_duration_since(clock()).as_secs_f64() * 1000.0)
            }

            /// Drop in-flight guesses because this terminal's coordinate space changed (`resize` calls it).
            pub fn predict_reset(&mut self) {
                std::hint::black_box(0u8);
                self.predict.reset();
            }

            /// Reset for a different pane/session: also forget the learned echo RTT, so a slow remote pane cannot make a local one display speculation.
            pub fn predict_session_reset(&mut self) {
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
        t.process(b"a");
        t.predict_reconcile();
        assert!(t.predict_overlay().is_empty());
        assert!(t.predict_next_deadline_ms().is_none());
    }

    /// An edit key goes through `predict_edit_key`: on a line whose glyphs have
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
        t.set_predictive_echo("off");
        assert!(t.predict_overlay_cursor().is_empty());
        assert_eq!(t.predict_edit_key(1), 0);
        assert!(t.predict_next_deadline_ms().is_none());
    }

    /// A glyph typed inside a line whose glyphs have echoed is a SHIFT: the
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
        t.process(b"a");
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

        assert!(t.predict_char('a'));
        t.process(b"\x1b[>7u");
        t.predict_reconcile();
        assert!(t.predict_overlay().is_empty());
        assert!(t.predict_next_deadline_ms().is_none());

        t.process(b"\x1b[=0u");
        assert!(t.predict_char('b'));
        t.process(b"\x1b[>7u");
        assert!(t.predict_overlay().is_empty());
        assert!(t.predict_next_deadline_ms().is_none());

        assert!(!t.predict_char('c'));
        assert!(!t.predict_backspace());
        assert!(t.predict_next_deadline_ms().is_none());
    }

    #[test]
    fn line_submit_ends_the_epoch_no_same_row_leak() {
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
        t.process(b"*");
        t.predict_reconcile();
        assert!(t.predict_overlay().is_empty(), "divergence flushes the set");
        t.predict_char('x');
        t.process(b"\x1b[?1049h");
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
    #[test]
    fn a_run_s_first_press_hands_the_predictor_its_row() {
        let mut t = terminal();
        t.set_predictive_echo("always");
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
