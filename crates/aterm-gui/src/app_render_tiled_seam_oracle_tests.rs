// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use std::collections::BTreeSet;
use std::time::{Duration, Instant};

use aterm_core::terminal::RenderCell;

use super::{
    COMPOSE_RETAIN_ARMS, COMPOSE_RETAIN_OFF, COMPOSE_RETAIN_UNSOUND, COMPOSE_SEAM_ARMS,
    COMPOSE_SEAM_UNTILED, divider_cell,
};
use crate::{App, WindowId, pane, term_lock};

/// Every per-row channel of a composed frame the retention lane can skip
/// rebuilding: clusters, combining marks, inline-image placements, DEC line
/// size and both span lanes, besides the cells. The fill rebuilds all seven
/// on a rebuilt row and touches none on a retained one, so the differential
/// compares all seven. Images are recorded by column: `ImageRef` is
/// `Arc`-backed and the two arms never share one.
#[derive(Clone, PartialEq, Debug)]
struct Composed {
    cells: Vec<Vec<RenderCell>>,
    clusters: Vec<Vec<(usize, Box<str>)>>,
    combining: Vec<Vec<(usize, Box<[char]>)>>,
    line_sizes: Vec<aterm_core::grid::LineSize>,
    line_size_spans: Vec<Vec<aterm_core::render::LineSizeSpan>>,
    default_bg_spans: Vec<Vec<aterm_core::render::DefaultBgSpan>>,
    image_columns: Vec<Vec<usize>>,
}

impl Composed {
    fn of(input: &aterm_render::RenderInput) -> Self {
        Self {
            cells: input.cells.clone(),
            clusters: input.clusters.clone(),
            combining: input.combining.clone(),
            line_sizes: input.line_sizes.clone(),
            line_size_spans: input.line_size_spans.clone(),
            default_bg_spans: input.default_bg_spans.clone(),
            image_columns: input
                .images
                .iter()
                .map(|row| row.iter().map(|(c, _)| *c).collect())
                .collect(),
        }
    }
}

const FRAME: Duration = Duration::from_millis(16);
/// The four glyphs the active-pane mark can put in a divider gap; no
/// terminal in this corpus ever prints one.
const MARK_GLYPHS: [char; 4] = ['\u{2581}', '\u{2594}', '\u{2590}', '\u{258c}'];

/// The stamp the coverage oracle floods the composed scratch with between
/// frames. Nothing in the corpus prints a snowman and no theme resolves
/// `[1, 2, 3]` on `[4, 5, 6]`, so a surviving one is unambiguous.
fn poison_cell() -> RenderCell {
    RenderCell {
        ch: '\u{2603}',
        fg: [1, 2, 3],
        bg: [4, 5, 6],
        ..RenderCell::default()
    }
}

/// What the corpus armed, counted while it ran.
#[derive(Default)]
struct Armed {
    frames: usize,
    pane_counts: BTreeSet<usize>,
    ring_frames: usize,
    ring_columns: BTreeSet<usize>,
    seam_frames: usize,
    gap_frames: usize,
    tiled_seam_passes: u64,
    whole_grid_seam_passes: u64,
    resize_frames: usize,
    alt_screen_frames: usize,
    scrolled_beside_live_frames: usize,
    preedit_frames: usize,
    prediction_frames: usize,
    close_frames: usize,
    /// Composed rows this corpus retained and rebuilt, the frames with at least one retained
    /// row, and the soundness witness.
    retained_rows: u64,
    rebuilt_rows: u64,
    retain_frames: usize,
    unsound: u64,
    /// `(step label, rows retained, rows composed)` per frame — the reach
    /// witness a failing census prints, so "the lane did not arm" names the
    /// step it did not arm on.
    retain_log: Vec<(String, u64, usize)>,
}

impl Armed {
    /// Every clause names a producer that can put a cell into a split
    /// composite; a zero means the oracles never saw it on.
    fn assert_non_vacuous(&self) {
        assert!(
            self.frames >= 27,
            "the corpus composed only {} frames",
            self.frames
        );
        for want in [2usize, 3, 4] {
            assert!(
                self.pane_counts.contains(&want),
                "the corpus never composed a {want}-pane layout: {:?}",
                self.pane_counts
            );
        }
        assert_eq!(
            self.gap_frames,
            self.frames,
            "{} of {} composed layouts left no cell outside every pane \
             rectangle — the divider lattice this whole pass is about was \
             never on screen, so the seam pass had nothing to get wrong",
            self.frames - self.gap_frames,
            self.frames
        );
        assert!(
            self.seam_frames > 0,
            "no frame ever carried a PLAIN divider seam: every gap was \
             inked by the active-pane mark, so the seam cell itself was \
             never composed"
        );
        assert!(
            self.ring_frames > 0,
            "the active-pane mark never painted, so the one producer that \
             writes INTO a divider gap was never armed"
        );
        assert!(
            self.ring_columns.len() >= 2,
            "the mark never moved: it inked one column set for the whole \
             corpus, so a focus change was never composed"
        );
        assert!(
            self.resize_frames >= 2,
            "the window grid never changed under the composite"
        );
        assert!(
            self.alt_screen_frames > 0,
            "no pane was ever on the alternate screen"
        );
        assert!(
            self.scrolled_beside_live_frames > 0,
            "no frame put a scrolled-back pane beside a live one"
        );
        assert!(
            self.preedit_frames > 0,
            "the IME composition never painted into a pane's cells"
        );
        assert!(
            self.prediction_frames > 0,
            "the prediction ghosts never painted into a pane's cells"
        );
        assert!(
            self.close_frames >= 1,
            "no pane was ever closed under the composite"
        );
    }

    /// Which per-row verdict the retention lane reached: the rebuild-every-row form
    /// satisfies the differential oracle for free, so a run in which retention never
    /// armed proves nothing.
    fn assert_retention(&self, on: bool) {
        assert_eq!(
            self.unsound, 0,
            "{} composed frame(s) retained a pane's rows and then let a host \
             mutator write into that pane's snapshot — `focus_quiet` is \
             predicting something it cannot see (see COMPOSE_RETAIN_UNSOUND)",
            self.unsound
        );
        let (kept, rebuilt) = (self.retained_rows, self.rebuilt_rows);
        if on {
            assert!(
                kept > 0,
                "the retention arm retained NOT ONE composed row in {} \
                 frames ({rebuilt} rebuilt) — the oracle is comparing the \
                 rebuild against itself",
                self.frames
            );
            assert!(
                self.retain_frames >= 12,
                "only {} of {} corpus frames retained anything; the corpus \
                 is meant to settle often enough that the lane is exercised \
                 against several producers, not once: {:?}",
                self.retain_frames,
                self.frames,
                self.retain_log
            );
            assert!(
                self.retain_log.iter().any(|(_, kept, rows)| {
                    usize::try_from(*kept).expect("row count") + 1 >= *rows
                }),
                "no corpus frame retained all but ONE composed row — the \
                 steady state the whole lane exists for (a keystroke echo \
                 that damages exactly one row) was never composed: {:?}",
                self.retain_log
            );
        } else {
            assert_eq!(
                (kept, self.retain_frames),
                (0, 0),
                "the control arm retained {kept} composed row(s); it is \
                 supposed to rebuild every row of every frame"
            );
            assert!(rebuilt > 0, "the control arm composed no rows at all");
        }
    }

    /// Which seam pass the corpus exercised: the whole-grid fill satisfies the oracles
    /// for free, so a run that never tiled proves nothing.
    fn assert_seam_pass(&self, tiled: bool) {
        let (t, w) = (self.tiled_seam_passes, self.whole_grid_seam_passes);
        if tiled {
            assert_eq!(
                (t as usize, w),
                (self.frames, 0),
                "the corpus meant to compose every one of its {} frames \
                 TILED, and ran {t} tiled / {w} whole-grid seam passes",
                self.frames
            );
        } else {
            assert_eq!(
                (t, w as usize),
                (0, self.frames),
                "the control arm meant to compose every one of its {} frames \
                 WHOLE-GRID, and ran {t} tiled / {w} whole-grid seam passes",
                self.frames
            );
        }
    }
}

/// The corpus driver: a real headless `App` composing through the shipping
/// `redraw_compose`, one recorded frame per step.
struct Corpus {
    app: App,
    wid: WindowId,
    now: Instant,
    poison: bool,
    armed: Armed,
    frames: Vec<Composed>,
}

impl Corpus {
    fn new(poison: bool, retain: bool) -> Self {
        COMPOSE_SEAM_ARMS.with(|arms| arms.set((0, 0)));
        COMPOSE_RETAIN_ARMS.with(|arms| arms.set((0, 0)));
        COMPOSE_RETAIN_UNSOUND.with(|c| c.set(0));
        COMPOSE_RETAIN_OFF.with(|off| off.set(!retain));
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        app.windows.get_mut(&wid).expect("test window").focused = true;
        // The prediction ghosts are one of the producers under test and
        // they are off by default policy, not by construction.
        app.config.predictive_echo = Some("always".to_string());
        app.predict_mode_cache = None;
        Self {
            app,
            wid,
            now: Instant::now(),
            poison,
            armed: Armed::default(),
            frames: Vec::new(),
        }
    }

    fn dims(&self) -> (usize, usize) {
        let ws = &self.app.windows[&self.wid];
        (usize::from(ws.rows), usize::from(ws.cols))
    }

    fn sessions(&self) -> Vec<u64> {
        self.app
            .active_tree(self.wid)
            .expect("a pure-terminal tab")
            .sessions()
    }

    fn focus(&self) -> u64 {
        self.app
            .active_tree(self.wid)
            .expect("a pure-terminal tab")
            .focus()
    }

    fn feed(&mut self, session: u64, bytes: &[u8]) {
        let term = self
            .app
            .pool
            .get(session)
            .expect("a live corpus session")
            .term
            .clone();
        term_lock(&term).process(bytes);
    }

    fn set_focus(&mut self, session: u64) {
        assert!(
            self.app
                .active_tree_mut(self.wid)
                .is_some_and(|tree| tree.set_focus(session)),
            "the pane is live"
        );
        let active = self.app.windows[&self.wid].tabs.active;
        assert!(self.app.sync_tab_model_from_layout(self.wid, active));
        self.app.sync_window(self.wid);
    }

    fn resize(&mut self, rows: u16, cols: u16) {
        {
            let ws = self.app.windows.get_mut(&self.wid).expect("test window");
            ws.rows = rows;
            ws.cols = cols;
        }
        self.app.resize_panes(self.wid);
        self.app.sync_window(self.wid);
        self.armed.resize_frames += 1;
    }

    fn close_focused_pane(&mut self) {
        let before = self.sessions().len();
        let outcome = self
            .app
            .active_tree_mut(self.wid)
            .expect("a pure-terminal tab")
            .close_focused();
        assert!(
            matches!(outcome, pane::CloseOutcome::Collapsed { .. }),
            "the corpus close did not collapse a pane"
        );
        let active = self.app.windows[&self.wid].tabs.active;
        assert!(self.app.sync_tab_model_from_layout(self.wid, active));
        self.app.resize_panes(self.wid);
        self.app.sync_window(self.wid);
        assert_eq!(self.sessions().len(), before - 1, "a pane did not close");
        self.armed.close_frames += 1;
    }

    /// One recorded corpus frame, driving output into the focused pane.
    fn step(&mut self, label: &str) {
        let focus = self.focus();
        self.step_feeding(focus, label);
    }

    /// One recorded corpus frame. Every step first feeds some visible pane a byte, so
    /// the `RepaintKey` always moves: a frame that took the early-out would compose
    /// nothing, and the coverage oracle would read its poison as a tiling failure.
    ///
    /// The pane is a parameter because the prediction ghosts are guessed at the
    /// focused pane's caret and dropped the moment the engine echoes there, so those
    /// steps feed a background pane.
    fn step_feeding(&mut self, session: u64, label: &str) {
        self.feed(session, b"x");
        let (rows, cols) = self.dims();
        if self.poison {
            let ws = self.app.windows.get_mut(&self.wid).expect("test window");
            for row in &mut ws.input_scratch.cells {
                row.fill(poison_cell());
            }
        }
        self.now += FRAME;
        let now = self.now;
        let before = COMPOSE_RETAIN_ARMS.with(std::cell::Cell::get);
        let presented = self
            .app
            .redraw_compose(self.wid, rows, cols, false, false, None, 0, now)
            .is_some();
        let after = COMPOSE_RETAIN_ARMS.with(std::cell::Cell::get);
        let (kept, rebuilt) = (after.0 - before.0, after.1 - before.1);
        assert!(
            presented,
            "corpus step `{label}` took the RepaintKey early-out — it \
             composed nothing, so neither oracle would be looking at a frame"
        );

        let seam = divider_cell(self.app.theme);
        let (mut ring_columns, mut seam_seen, mut stale) = (Vec::new(), false, Vec::new());
        {
            let ws = &self.app.windows[&self.wid];
            for (r, row) in ws.input_scratch.cells.iter().enumerate() {
                for (c, cell) in row.iter().enumerate() {
                    if MARK_GLYPHS.contains(&cell.ch) {
                        ring_columns.push(c);
                    }
                    if *cell == seam {
                        seam_seen = true;
                    }
                    if *cell == poison_cell() {
                        stale.push((r, c));
                    }
                }
            }
        }
        assert!(
            stale.is_empty(),
            "corpus step `{label}`: {} composed cell(s) were written by \
             neither the seam pass nor a pane blit and kept the previous \
             frame's content — first at {:?}",
            stale.len(),
            &stale[..stale.len().min(8)]
        );
        debug_assert!(
            !self.poison || COMPOSE_RETAIN_OFF.with(std::cell::Cell::get),
            "the poison flood is an UNDECLARED write into the composite (no \
             `snapshot_seq` bump), which is precisely the thing the K2 \
             second clock cannot see — the coverage oracle must drive the \
             rebuild arm"
        );
        let layout = self
            .app
            .active_tree(self.wid)
            .expect("a pure-terminal tab")
            .compute_layout(
                u16::try_from(rows).expect("corpus rows"),
                u16::try_from(cols).expect("corpus cols"),
            );
        let covered: usize = layout
            .iter()
            .map(|rect| usize::from(rect.rows) * usize::from(rect.cols))
            .sum();
        let panes = layout.len();
        let (preedit, predicted) = {
            let ws = &self.app.windows[&self.wid];
            (ws.preedit_shown, ws.pred_shown)
        };
        self.armed.frames += 1;
        self.armed.pane_counts.insert(panes);
        self.armed.ring_frames += usize::from(!ring_columns.is_empty());
        self.armed.ring_columns.extend(ring_columns);
        self.armed.seam_frames += usize::from(seam_seen);
        self.armed.gap_frames += usize::from(covered < rows * cols);
        self.armed.preedit_frames += usize::from(preedit);
        self.armed.prediction_frames += usize::from(predicted);
        self.armed.retained_rows += kept;
        self.armed.rebuilt_rows += rebuilt;
        self.armed.retain_frames += usize::from(kept > 0);
        self.armed.retain_log.push((label.to_string(), kept, rows));
        self.frames
            .push(Composed::of(&self.app.windows[&self.wid].input_scratch));
    }
}

/// The corpus: one script, run by both oracles.
fn drive_corpus(poison: bool, retain: bool) -> (Vec<Composed>, Armed) {
    let mut c = Corpus::new(poison, retain);
    let wid = c.wid;

    // 1. Two panes side by side, both carrying text.
    let right = c.app.split_active_stub_tab(wid);
    let left = *c
        .sessions()
        .iter()
        .find(|s| **s != right)
        .expect("two panes");
    c.feed(left, b"left pane content\r\n");
    c.feed(right, b"right pane content\r\n");
    c.step("two panes established");
    c.step("two panes, echo");

    // 2. The focus change moves the mark across the shared divider column.
    c.set_focus(left);
    c.step("focus moved to the left pane");
    c.set_focus(right);
    c.step("focus moved back to the right pane");
    c.step("two panes settled after the focus move");

    // 3. Three panes, then four: a layout change under the composite.
    let third = c
        .app
        .split_active_stub_tab_dir(wid, pane::SplitDir::Horizontal);
    c.feed(third, b"third pane content\r\n");
    c.step("three panes");
    let fourth = c
        .app
        .split_active_stub_tab_dir(wid, pane::SplitDir::Vertical);
    c.feed(fourth, b"fourth pane content\r\n");
    c.step("four panes");
    c.step("four panes settled");

    // 4. The divider drag, as a geometry change: the window grid and every
    //    pane rectangle move.
    c.resize(31, 97);
    c.step("window grid shrunk under four panes");
    c.resize(44, 123);
    c.step("window grid grown under four panes");
    c.step("settled at the grown grid");

    // 5. The alternate screen in one pane, beside three ordinary ones.
    c.feed(left, b"\x1b[?1049h\x1b[Halt screen here\r\n");
    {
        let term = c.app.pool.get(left).expect("left session").term.clone();
        assert!(
            term_lock(&term).is_alternate_screen(),
            "the alt-screen producer did not arm"
        );
    }
    c.armed.alt_screen_frames += 1;
    c.step("one pane on the alternate screen");
    c.step("settled with one pane on the alternate screen");

    // 6. A scrolled-back background pane beside a live one (the focused pane
    //    stays at the live bottom so the composition and ghosts below paint).
    for i in 0..60 {
        c.feed(third, format!("history line {i:03}\r\n").as_bytes());
    }
    {
        let term = c.app.pool.get(third).expect("third session").term.clone();
        term_lock(&term).grid_mut().scroll_display(12);
        assert!(
            term_lock(&term).grid().display_offset() > 0,
            "the scrollback producer did not arm"
        );
    }
    c.armed.scrolled_beside_live_frames += 1;
    c.step("a scrolled-back pane beside live ones");
    c.step("settled with a scrolled-back pane beside live ones");

    // 6b. Back to the live bottom: the scrolled pane shares composed rows
    //     with the focused one, and leaving it parked would keep those rows
    //     from retaining, so the `focus_quiet` clause guarding the mutators
    //     below would never be reached.
    {
        let term = c.app.pool.get(third).expect("third session").term.clone();
        term_lock(&term).grid_mut().scroll_display(-12);
        assert_eq!(
            term_lock(&term).grid().display_offset(),
            0,
            "the scrollback producer did not disarm"
        );
    }
    c.step("the scrolled pane returned to the live bottom");
    c.step("settled with every pane live again");

    // 7. The IME composition, which writes engine cells into the focused
    //    pane's snapshot on the compose path.
    {
        let ws = c.app.windows.get_mut(&wid).expect("test window");
        ws.preedit = "\u{3053}\u{3093}".to_string();
        ws.preedit_caret = Some(1);
    }
    c.step("IME preedit on the focused pane");
    {
        let ws = c.app.windows.get_mut(&wid).expect("test window");
        ws.preedit = "\u{3053}\u{3093}\u{306b}\u{3061}".to_string();
        ws.preedit_caret = Some(3);
    }
    c.step("IME preedit grown");
    {
        let ws = c.app.windows.get_mut(&wid).expect("test window");
        ws.preedit.clear();
        ws.preedit_caret = None;
    }
    c.step("IME preedit committed away");

    // 8. The prediction ghosts, guessed at the focused pane's own caret: the
    //    compose path drops guesses parked anywhere else.
    {
        let focus = c.focus();
        let (caret, coords) = {
            let term = c.app.pool.get(focus).expect("focused session").term.clone();
            let term = term_lock(&term);
            let cursor = term.cursor();
            (
                (cursor.row, cursor.col),
                term.content_scroll_state().coordinate_generation(),
            )
        };
        let now = c.now + FRAME;
        let ws = c.app.windows.get_mut(&wid).expect("test window");
        ws.predictor.set_mode(crate::predict::PredictMode::Always);
        let cols = ws.cols;
        assert!(
            ws.predictor.predict_char('?', caret, coords, cols, now),
            "the prediction producer did not arm"
        );
    }
    c.step_feeding(left, "prediction ghosts on the focused pane");
    c.step_feeding(left, "prediction ghosts settling");

    // 9. A pane closing, twice: the layout shrinks under the composite it
    //    was tiled against.
    c.close_focused_pane();
    c.step("a pane closed");
    c.step("settled after the close");
    c.close_focused_pane();
    c.step("another pane closed");
    c.resize(24, 80);
    c.step("back to the original grid");
    c.step("settled back at the original grid");

    let (tiled, whole) = COMPOSE_SEAM_ARMS.with(std::cell::Cell::get);
    c.armed.tiled_seam_passes = tiled;
    c.armed.whole_grid_seam_passes = whole;
    c.armed.unsound = COMPOSE_RETAIN_UNSOUND.with(std::cell::Cell::get);
    COMPOSE_RETAIN_OFF.with(|off| off.set(false));
    (c.frames, c.armed)
}

/// Compare two corpus runs frame for frame, channel for channel, naming the
/// first divergence exactly.
fn assert_frames_agree(a: &[Composed], b: &[Composed], a_name: &str, b_name: &str) {
    assert_eq!(
        a.len(),
        b.len(),
        "the two arms composed different numbers of frames"
    );
    for (f, (x, y)) in a.iter().zip(b.iter()).enumerate() {
        assert_eq!(
            x.cells.len(),
            y.cells.len(),
            "frame {f}: different row counts"
        );
        for (r, (xrow, yrow)) in x.cells.iter().zip(y.cells.iter()).enumerate() {
            assert_eq!(
                xrow.len(),
                yrow.len(),
                "frame {f} row {r}: different widths"
            );
            if xrow != yrow {
                let c = xrow
                    .iter()
                    .zip(yrow.iter())
                    .position(|(p, q)| p != q)
                    .expect("the rows differ");
                panic!(
                    "frame {f}, cell ({r}, {c}): {a_name} composed {:?} where \
                     {b_name} composed {:?}",
                    xrow[c], yrow[c]
                );
            }
        }
        assert_eq!(
            x.clusters, y.clusters,
            "frame {f}: {a_name} and {b_name} disagree on the emoji-cluster lane"
        );
        assert_eq!(
            x.combining, y.combining,
            "frame {f}: {a_name} and {b_name} disagree on the combining-mark lane"
        );
        assert_eq!(
            x.line_sizes, y.line_sizes,
            "frame {f}: {a_name} and {b_name} disagree on the row-level DEC line sizes"
        );
        assert_eq!(
            x.line_size_spans, y.line_size_spans,
            "frame {f}: {a_name} and {b_name} disagree on the per-pane DEC line-size runs"
        );
        assert_eq!(
            x.default_bg_spans, y.default_bg_spans,
            "frame {f}: {a_name} and {b_name} disagree on the per-pane default-background runs"
        );
        assert_eq!(
            x.image_columns, y.image_columns,
            "frame {f}: {a_name} and {b_name} disagree on the inline-image placements"
        );
    }
}

/// The two clauses of [`super::seed_seam_cells`] the App-level corpus cannot
/// reach, checked against the whole-grid fill:
///
/// * The shortfall: `cell_frame_fill` stamps the requested dimensions, so
///   only a hand-shortened snapshot arms [`super::seam_declared_shortfall`].
/// * The out-of-range tile: no layout produces a rectangle past the window
///   edge.
#[test]
fn the_unreachable_seam_clauses_fall_back_to_the_whole_grid_fill() {
    use aterm_render::{RenderInput, Theme};

    use super::{blit_pane_into, fill_divider_grid, fill_divider_grid_tiled};

    let theme = Theme::default();
    let blank = RenderCell {
        ch: ' ',
        fg: [9, 9, 9],
        bg: [7, 7, 7],
        ..RenderCell::default()
    };
    // A 4x9 window: pane at (0,0) 4x4, divider column 4, pane at (0,5) 4x4. Nine columns,
    // not five, because the pane-count clause caps the tiling at `n * (n + 1) <= cols`
    // and two panes need six.
    let rect = |session, row_off, col_off, rows, cols| pane::PaneRect {
        session,
        row_off,
        col_off,
        rows,
        cols,
    };
    let tiles = [rect(0, 0, 0, 4, 4), rect(1, 0, 5, 4, 4)];

    // A pane snapshot of `rows`x`cols` filled with one cell, with every
    // per-row channel `blit_pane_into` indexes sized to match.
    let snapshot = |rows: usize, cols: usize, cell: RenderCell| RenderInput {
        rows,
        cols,
        cells: vec![vec![cell; cols]; rows],
        line_sizes: vec![aterm_core::grid::LineSize::SingleWidth; rows],
        clusters: vec![Vec::new(); rows],
        combining: vec![Vec::new(); rows],
        images: vec![Vec::new(); rows],
        line_size_spans: vec![Vec::new(); rows],
        default_bg_spans: vec![Vec::new(); rows],
        ..RenderInput::default()
    };
    // The short snapshot: declared 4x4, filled 2x2. Whatever the blit does not reach
    // inside the declared rectangle is the shortfall.
    let short = snapshot(2, 2, blank);
    // The full snapshot carries a glyph no seam or blank produces. Frame one blits it, so
    // the cells the short frame fails to reach read as pane content; without it both arms
    // would read "seam" and the test would pass vacuously.
    let full = snapshot(
        4,
        4,
        RenderCell {
            ch: 'Z',
            fg: [9, 9, 9],
            bg: [7, 7, 7],
            ..RenderCell::default()
        },
    );

    let compose = |tiled: bool, shortfall: bool| {
        let mut dst = RenderInput::default();
        // Two frames into one buffer: frame one fills every rectangle with
        // pane content, frame two's snapshot is short. Whatever the second
        // frame does not write is the first frame's `Z`s.
        for src in [&full, &short] {
            if tiled {
                fill_divider_grid_tiled(&mut dst, 4, 9, theme, None, tiles.iter().copied());
            } else {
                fill_divider_grid(&mut dst, 4, 9, theme, None);
            }
            for tile in tiles {
                blit_pane_into(
                    &mut dst,
                    src,
                    usize::from(tile.row_off),
                    usize::from(tile.col_off),
                    blank,
                );
                if shortfall {
                    super::seam_declared_shortfall(
                        &mut dst,
                        theme.into(),
                        tile,
                        src.rows,
                        src.cols,
                        &[],
                    );
                }
            }
        }
        dst.cells
    };

    let whole = compose(false, false);
    assert_eq!(
        compose(true, true),
        whole,
        "the shortfall clause did not restore the seam under the part of a \
         declared rectangle its snapshot never covered"
    );
    assert_ne!(
        compose(true, false),
        whole,
        "the shortfall region came out right WITHOUT the clause, so this \
         test is not exercising it"
    );

    // An out-of-range tile disowns the tiling for the whole frame.
    let mut off_grid = RenderInput::default();
    let over = [rect(0, 0, 0, 4, 4), rect(1, 0, 5, 4, 99)];
    for _ in 0..2 {
        fill_divider_grid_tiled(&mut off_grid, 4, 9, theme, None, over.iter().copied());
    }
    let mut plain = RenderInput::default();
    for _ in 0..2 {
        fill_divider_grid(&mut plain, 4, 9, theme, None);
    }
    assert_eq!(
        off_grid.cells, plain.cells,
        "a rectangle past the window edge must fall back to the whole-grid \
         seam, not tile around it"
    );
}

/// Coverage: every composed cell is written every frame, by the seam pass or by a pane
/// blit. Nothing survives from the frame before.
#[test]
fn the_tiled_compose_leaves_no_composed_cell_unwritten_over_the_corpus() {
    COMPOSE_SEAM_UNTILED.with(|untiled| untiled.set(false));
    // Retention is off: the poison flood writes into the composite without bumping
    // `snapshot_seq`, so the lane would (per its contract) retain rows the poison
    // overwrote. Retention correctness is `the_retained_tiled_and_whole_grid_composites_agree`.
    let (frames, armed) = drive_corpus(true, false);
    armed.assert_non_vacuous();
    armed.assert_seam_pass(true);
    armed.assert_retention(false);
    assert!(!frames.is_empty());
}

/// A settled four-pane split whose only change is one echoed keystroke in
/// the focused pane retains every composed row but the one the echo
/// touched: the arming witness the differential oracle cannot provide.
#[test]
fn a_settled_split_retains_every_row_but_the_one_the_keystroke_touched() {
    let (mut c, rows) = four_pane_split(true);
    let mut log: Vec<usize> = Vec::new();
    for i in 0..10u8 {
        log.push(settled_frame(&mut c, i));
    }
    COMPOSE_RETAIN_OFF.with(|off| off.set(false));
    let settled = &log[log.len() - 4..];
    assert!(
        settled.iter().all(|kept| *kept == rows - 1),
        "a settled four-pane split retained {settled:?} of {rows} composed rows \
         per frame; the keystroke damages exactly one row, so every other row \
         is one this frame does not need to write (full log {log:?})"
    );
}

/// The second clock at the `App` level. A host write into the finished
/// composite bumps `snapshot_seq` only; the next frame then retains nothing
/// and the intruding cells are gone. A write without the bump is invisible
/// to the lane and its cells survive: the lane relies on that discipline,
/// not on inspecting cells.
#[test]
fn a_host_write_into_the_composite_revokes_the_retention_only_when_it_says_so() {
    for declared in [true, false] {
        let (mut c, rows) = four_pane_split(true);
        for i in 0..6u8 {
            settled_frame(&mut c, i);
        }
        assert_eq!(
            settled_frame(&mut c, 6),
            rows - 1,
            "the fixture is not retaining before the host write, so the \
             revoke below would prove nothing"
        );
        {
            let ws = c.app.windows.get_mut(&c.wid).expect("test window");
            for row in &mut ws.input_scratch.cells {
                row.fill(poison_cell());
            }
            if declared {
                ws.input_scratch.snapshot_seq = ws.input_scratch.snapshot_seq.wrapping_add(1);
            }
        }
        let kept = settled_frame(&mut c, 7);
        let survivors = {
            let ws = &c.app.windows[&c.wid];
            ws.input_scratch
                .cells
                .iter()
                .flatten()
                .filter(|cell| **cell == poison_cell())
                .count()
        };
        if declared {
            assert_eq!(
                (kept, survivors),
                (0, 0),
                "a DECLARED host write (one `snapshot_seq` bump, the shipping \
                 discipline) must revoke the blessing outright: the frame after \
                 it retained {kept} of {rows} rows and left {survivors} of the \
                 intruder's cells on screen"
            );
        } else {
            assert!(
                kept > 0 && survivors > 0,
                "an UNDECLARED write is supposed to be invisible to the second \
                 clock — if it is not, the lane is inspecting cells rather than \
                 provenance and this test is documenting the wrong contract \
                 (kept {kept}, survivors {survivors})"
            );
        }
        COMPOSE_RETAIN_OFF.with(|off| off.set(false));
    }
}

/// A four-pane split with every leaf carrying materialized rows, plus the
/// composed row count. `retain` chooses the arm.
fn four_pane_split(retain: bool) -> (Corpus, usize) {
    let mut c = Corpus::new(false, retain);
    let wid = c.wid;
    c.app.split_active_stub_tab(wid);
    c.app
        .split_active_stub_tab_dir(wid, pane::SplitDir::Horizontal);
    c.app
        .split_active_stub_tab_dir(wid, pane::SplitDir::Vertical);
    c.app.sync_window(wid);
    let sessions = c.sessions();
    assert_eq!(
        sessions.len(),
        4,
        "the fixture did not build a four-way split"
    );
    for session in &sessions {
        c.feed(*session, b"pane content here\r\n");
    }
    let rows = c.dims().0;
    (c, rows)
}

/// One settled frame: a keystroke echo into the focused pane (CR then one
/// glyph: exactly one damaged row, no wrap or scroll), composed. Returns how
/// many composed rows the retention lane left alone.
fn settled_frame(c: &mut Corpus, i: u8) -> usize {
    let focus = c.focus();
    c.feed(focus, if i.is_multiple_of(2) { b"\rx" } else { b"\ry" });
    let (rows, cols) = c.dims();
    c.now += FRAME;
    let now = c.now;
    let wid = c.wid;
    assert!(
        c.app
            .redraw_compose(wid, rows, cols, false, false, None, 0, now)
            .is_some(),
        "settled frame {i} took the RepaintKey early-out"
    );
    c.app.windows[&wid].composed_retain.retained_rows()
}

/// Two differentials over three runs of the same corpus through independent
/// `App`s (retained, rebuilt-and-tiled, rebuilt whole-grid):
///
/// * the retained composite equals the one that rebuilds every row, cells
///   and all six per-row channels, on every frame (a wrongly retained row is
///   stale content on screen);
/// * the tiled composite equals the whole-grid one, cell for cell; both of
///   these rebuild every row, so the comparison is about the seam pass.
#[test]
fn the_retained_tiled_and_whole_grid_composites_agree() {
    COMPOSE_SEAM_UNTILED.with(|untiled| untiled.set(false));
    let (retained, retained_armed) = drive_corpus(false, true);
    let (rebuilt, rebuilt_armed) = drive_corpus(false, false);
    COMPOSE_SEAM_UNTILED.with(|untiled| untiled.set(true));
    let (whole, whole_armed) = drive_corpus(false, false);
    COMPOSE_SEAM_UNTILED.with(|untiled| untiled.set(false));

    retained_armed.assert_non_vacuous();
    retained_armed.assert_seam_pass(true);
    retained_armed.assert_retention(true);
    rebuilt_armed.assert_non_vacuous();
    rebuilt_armed.assert_seam_pass(true);
    rebuilt_armed.assert_retention(false);
    whole_armed.assert_non_vacuous();
    whole_armed.assert_seam_pass(false);
    assert_frames_agree(&retained, &rebuilt, "the retained arm", "the rebuilt arm");
    assert_frames_agree(
        &rebuilt,
        &whole,
        "the tiled seam pass",
        "the whole-grid fill",
    );
}
