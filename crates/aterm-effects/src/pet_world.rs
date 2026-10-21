// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Bounded, text-blind perception of the pet's currently presented pane.
//!
//! This is an observation, not another behavior driver. A cell is usable only
//! after the current snapshot certifies its placement. Clear space is preferred;
//! ordinary text also admits a resident beneath the glyphs. Missing rows, an exhausted
//! scan budget and incoherent metadata are unknown, never empty. No method
//! reads a clock, mutates the terminal, or contributes an animation deadline.

use aterm_core::grid::LineSize;
use aterm_core::grid::extra::ImageRef;
use aterm_core::render::{DefaultBgSpan, LineSizeSpan, RenderInput, SelectionClip};
use aterm_core::selection::TextSelection;
use aterm_core::terminal::{
    BlockState, ContentScrollDelta, ContentScrollState, RenderCell, Terminal, UnderlineStyle,
};
pub use aterm_types::TaskbarProgress;

pub const MAX_WORLD_ROWS: usize = 64;
pub const MAX_WORLD_COLS: usize = 256;
pub const MAX_WORLD_CELLS: usize = MAX_WORLD_ROWS * MAX_WORLD_COLS;
pub const MAX_WORLD_ANCHORS: usize = 8;
const MAX_BLOCK_PROBES: usize = 64;
const MAX_REGIONS: usize = 32;
/// The sliver [`PetWorld::nearest_clear_perch`] leaves between a seat's
/// margin and the cell edges it fills, so float rounding never widens the
/// cell window it was certified against.
const PERCH_SLACK: f32 = 0.02;

/// Fractional, pane-local cells; the complete sprite rectangle, not its feet.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct PetRect {
    pub row: f32,
    pub col: f32,
    pub rows: f32,
    pub cols: f32,
}

impl PetRect {
    #[must_use]
    pub const fn new(row: f32, col: f32, rows: f32, cols: f32) -> Self {
        Self {
            row,
            col,
            rows,
            cols,
        }
    }

    #[must_use]
    pub fn foot_row(self) -> f32 {
        self.row + self.rows - 1.0
    }

    #[must_use]
    pub fn valid(self) -> bool {
        [self.row, self.col, self.rows, self.cols]
            .iter()
            .all(|x| x.is_finite())
            && self.rows > 0.0
            && self.cols > 0.0
    }

    /// Do these two rectangles share any area at all?
    #[must_use]
    pub fn overlaps(self, other: Self) -> bool {
        self.valid()
            && other.valid()
            && self.row < other.row + other.rows
            && other.row < self.row + self.rows
            && self.col < other.col + other.cols
            && other.col < self.col + self.cols
    }

    fn union(self, other: Self) -> Self {
        let row = self.row.min(other.row);
        let col = self.col.min(other.col);
        Self::new(
            row,
            col,
            (self.row + self.rows).max(other.row + other.rows) - row,
            (self.col + self.cols).max(other.col + other.cols) - col,
        )
    }
}

/// The host's late overlays (what it paints into the pane after this
/// observation), by what each is on the glass. Pane-local cells.
#[derive(Clone, Copy, Debug, Default)]
pub struct PetReserve<'a> {
    /// Surfaces the pet never stands on for good: the host's chrome (the
    /// footer's rules and chips, the harness card's glass). A body there steps
    /// aside, like a body on a selection.
    pub protected: &'a [PetRect],
    /// The host's TRANSIENT overlays (the Autopilot menu, the find bar):
    /// protected alike, but, like a live selection, left off the ground plane a
    /// rest seat is solved on ([`PetWorld::nearest_clear_perch`]).
    pub overlays: &'a [PetRect],
    /// The person's own typed glyphs, painted before the program echoes them:
    /// the predicted echo and the IME preedit row. Ordinary ink a few
    /// milliseconds early, so it is classed as ink: a resident may stand under
    /// it, and is drawn beneath it, as it will be once the echo lands.
    ///
    /// Classed as protected (as it was until 2026-10-20), the guessed cell
    /// beside the escort's seat turned protected and then back into ink on
    /// every keystroke, and the step-aside threw the pet up onto a border rule
    /// or three columns ahead in one frame each time.
    pub ink: &'a [PetRect],
}

impl<'a> PetReserve<'a> {
    /// Only protected surfaces: the reserve before typed glyphs were told
    /// apart, and every host that splices none.
    #[must_use]
    pub const fn protected(protected: &'a [PetRect]) -> Self {
        Self {
            protected,
            overlays: &[],
            ink: &[],
        }
    }
}

/// A rectangle's footprint in the observation map: a half-open map-local
/// cell window, or one of the two reasons there is no answer to count over.
#[derive(Clone, Copy, Debug)]
enum Window {
    /// Non-finite geometry — a refusal to answer, not an observation.
    Degenerate,
    /// Outside the coverage window: I cannot see there.
    Outside,
    Cells(usize, usize, usize, usize),
}

/// One map-local cell that only the live caret's 3x3 ring protects.
#[derive(Clone, Copy, Debug)]
struct CaretCell {
    row: usize,
    col: usize,
    /// Would this cell have been [`PetCell::Clear`] with no caret at all?
    /// The strict `blocked_prefix` reading forgives only these; the
    /// under-text reading forgives the whole ring.
    blank: bool,
}

/// Which caret ring cells a clearance reading may discount.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Forgive {
    /// PLACEMENT: the caret is protected, so the pet never rests on it.
    Nothing,
    /// Only ring cells that carry nothing the user can see.
    BlankCaret,
    /// Every ring cell — for the reading that already admits ordinary ink.
    AnyCaret,
}

/// The terminal's rectangle in the supplied frame. All returned geometry is
/// local to this rectangle; chrome and sibling panes are never candidates.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PetPane {
    pub row: usize,
    pub col: usize,
    pub rows: usize,
    pub cols: usize,
}

impl PetPane {
    #[must_use]
    pub fn full(input: &RenderInput) -> Self {
        Self {
            row: 0,
            col: 0,
            rows: input.rows,
            cols: input.cols,
        }
    }
}

/// Identity fences for retained content anchors. Content sequence is separate:
/// a changed cell must be revalidated, but need not destroy a block's identity.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PetSurface {
    pub session: u64,
    pub terminal_id: u64,
    pub alt_screen: bool,
    pub rows: usize,
    pub cols: usize,
    pub absolute_row_revision: u64,
    pub history_renumber_epoch: u64,
    pub scroll_invalidation: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PetWorldStamp {
    pub surface: PetSurface,
    pub content_seq: u64,
    pub display_offset: i32,
    pub top_absolute_row: u64,
    pub uniform_up_rows: u64,
    /// Additional counters needed to decide whether coordinate changes between
    /// observations were completely explained. The pet's home is screen-local:
    /// it needs the reader's validity decision, not sixteen copied band payloads.
    pub scroll_band_batches: u64,
    pub scroll_band_seq: u64,
}

impl PetWorldStamp {
    /// A last visible caret remains a screen-local home through a completely
    /// explained scroll. Content anchors still use the stricter whole-surface
    /// identity: this permission neither translates them nor invents input.
    pub(crate) fn preserves_cursor_home(self, previous: Self) -> bool {
        let a = previous.surface;
        let b = self.surface;
        if a.session != b.session
            || a.terminal_id != b.terminal_id
            || a.alt_screen != b.alt_screen
            || a.rows != b.rows
            || a.cols != b.cols
            || a.history_renumber_epoch != b.history_renumber_epoch
            || previous.display_offset != self.display_offset
        {
            return false;
        }
        // delta_since reads only these counters when deciding completeness.
        // No reconstructed snapshot escapes this method: its empty payload
        // must never be used to replay the band records themselves.
        let counters = |stamp: Self| ContentScrollState {
            uniform_up_rows: stamp.uniform_up_rows,
            invalidation_epoch: stamp.surface.scroll_invalidation,
            band_batches: stamp.scroll_band_batches,
            band_seq: stamp.scroll_band_seq,
            ..ContentScrollState::default()
        };
        match ContentScrollState::delta_since(Some(counters(previous)), counters(self)) {
            // An archival regional scroll updates absolute-row identities
            // while the cursor's screen coordinate remains a valid home.
            ContentScrollDelta::Bands { .. } => true,
            ContentScrollDelta::Unchanged | ContentScrollDelta::Translate(_) => {
                a.absolute_row_revision == b.absolute_row_revision
            }
            ContentScrollDelta::Baseline | ContentScrollDelta::Invalidate => false,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PetBlock {
    pub id: u64,
    pub state: BlockState,
    pub start_row: u64,
    pub end_row: u64,
    pub output_row: Option<u64>,
    pub exit_code: Option<i32>,
}

/// Capture beside the renderer's cell extraction under the same terminal
/// read. There are no strings or retained terminal references in this value.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PetWorldFacts {
    pub stamp: PetWorldStamp,
    pub base_y: i64,
    pub blocks: [Option<PetBlock>; MAX_WORLD_ANCHORS],
    pub progress: Option<TaskbarProgress>,
    pub completed_seq: u64,
}

impl PetWorldFacts {
    #[must_use]
    pub fn read(term: &Terminal, session: u64) -> Self {
        let grid = term.grid();
        let scroll = term.content_scroll_state();
        let rows = usize::from(grid.rows());
        let cols = usize::from(grid.cols());
        let top = grid.top_visible_absolute_row();
        let end = top.saturating_add(rows as u64);
        let mut blocks = [None; MAX_WORLD_ANCHORS];
        if !term.is_alternate_screen() {
            let mut used = 0;
            for block in term.all_blocks().rev().take(MAX_BLOCK_PROBES) {
                // Collapsed geometry is host-owned; absolute output rows no
                // longer certify where that output was presented.
                if block.collapsed {
                    continue;
                }
                let block_end = block.end_row.unwrap_or(end);
                if block.prompt_start_row >= end || block_end <= top {
                    continue;
                }
                blocks[used] = Some(PetBlock {
                    id: block.id,
                    state: block.state,
                    start_row: block.prompt_start_row,
                    end_row: block_end,
                    output_row: block.output_start_row,
                    exit_code: block.exit_code,
                });
                used += 1;
                if used == MAX_WORLD_ANCHORS {
                    break;
                }
            }
        }
        Self {
            stamp: PetWorldStamp {
                surface: PetSurface {
                    session,
                    terminal_id: term.render_identity(),
                    alt_screen: term.is_alternate_screen(),
                    rows,
                    cols,
                    absolute_row_revision: term.absolute_row_revision(),
                    history_renumber_epoch: grid.history_renumber_epoch(),
                    scroll_invalidation: scroll.invalidation_epoch,
                },
                content_seq: term.content_seq(),
                display_offset: grid.display_offset() as i32,
                top_absolute_row: top,
                uniform_up_rows: scroll.uniform_up_rows,
                scroll_band_batches: scroll.band_batches,
                scroll_band_seq: scroll.band_seq,
            },
            base_y: i64::try_from(grid.base_y()).unwrap_or(i64::MAX),
            blocks,
            progress: term.taskbar_progress(),
            completed_seq: term.completed_command_seq(),
        }
    }
}

/// A real visible block, with a stable absolute row and its current projection.
/// It is an attention target only; it does not itself certify blank space.
#[derive(Clone, Copy, Debug)]
pub struct PetAnchor {
    pub surface: PetSurface,
    pub block_id: u64,
    pub absolute_row: u64,
    pub row: f32,
    pub col: f32,
    pub state: BlockState,
    pub exit_code: Option<i32>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum PetCell {
    #[default]
    Unknown,
    Clear,
    Ink,
    Protected,
}

/// Glyph occupancy and locomotion obstruction are different observations.
/// A single isolated Braille dot is still visible ink (and still participates
/// in attention/contact), but does not displace a pet drawn beneath the text.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PetGlyph {
    Blank,
    Speck,
    Substantive,
}

/// A fixed-budget occupancy window and small, text-free observation set.
#[derive(Clone, Debug)]
pub struct PetWorld {
    cells: Vec<PetCell>,
    glyphs: Vec<Option<PetGlyph>>,
    blocked_prefix: Vec<u16>,
    protected_prefix: Vec<u16>,
    /// Per cell: is anything painted there in the text layer — a glyph, a
    /// colored background, a selection, an image, a host overlay spliced in
    /// after this observation, an expanded row, or a cell nobody observed?
    /// `cells` minus the caret's keep-off ring: the Z-ORDER plane
    /// ([`PetWorld::ink_clearance`]).
    inked: Vec<bool>,
    /// THE GROUND PLANE: each cell as it would be with no live selection and
    /// no transient host overlay (the Autopilot menu, the find bar). Written,
    /// with its two prefix
    /// tables, only on an observation where such a cell exists
    /// ([`Self::ground_split`]); otherwise the ground is `cells` itself and
    /// the readers use `blocked_prefix` and `protected_prefix`. A rest seat
    /// is solved on this plane ([`PetWorld::nearest_clear_perch`]), so a
    /// selection the hand drags or a menu it opens never moves where the pet
    /// rests: a seat they cover is hidden in place, as before.
    ground_cells: Vec<PetCell>,
    ground_blocked_prefix: Vec<u16>,
    ground_protected_prefix: Vec<u16>,
    /// Whether this observation saw a transient cell, so the ground plane
    /// differs from `cells`.
    ground_split: bool,
    /// Ring cells under a selection or an overlay and nothing lasting: on
    /// the ground plane only the caret protects them, so its readers forgive
    /// them as `caret_cells` are forgiven on the live plane (a shell's stand
    /// sits in the ring by design). At most nine, with `caret_cells`.
    ground_caret_cells: [Option<CaretCell>; 9],
    /// Per map row, the map-local columns `[lo, hi)` from its first
    /// substantive ink to its last: a glyph (not a lone Braille speck) or a
    /// coloured background. A gap between a line's words, the gutter of a
    /// command list or a table's columns, is inside it: not ground a rest
    /// seat may take ([`PetWorld::nearest_clear_perch`]). `lo >= hi` is a
    /// row with none; a row nobody observed is all hull.
    hull: Vec<(u16, u16)>,
    coverage: PetPane,
    /// Map-local cells only the live caret's 3x3 ring protects, at most nine,
    /// kept apart so each visibility reading forgives the right subset:
    /// [`PetWorld::clearance_past_caret`] forgives only the blank ones, while
    /// [`PetWorld::under_text_clearance_past_caret`] (which admits ordinary
    /// ink) may forgive the whole ring.
    caret_cells: [Option<CaretCell>; 9],
    stamp: Option<PetWorldStamp>,
    anchors: [Option<PetAnchor>; MAX_WORLD_ANCHORS],
    selection_rect: Option<PetRect>,
    selection_target: Option<(f32, f32)>,
    progress: Option<TaskbarProgress>,
    examined_cells: usize,
}

impl Default for PetWorld {
    fn default() -> Self {
        Self {
            cells: vec![PetCell::Unknown; MAX_WORLD_CELLS],
            glyphs: vec![None; MAX_WORLD_CELLS],
            blocked_prefix: vec![0; (MAX_WORLD_ROWS + 1) * (MAX_WORLD_COLS + 1)],
            protected_prefix: vec![0; (MAX_WORLD_ROWS + 1) * (MAX_WORLD_COLS + 1)],
            inked: vec![true; MAX_WORLD_CELLS],
            ground_cells: vec![PetCell::Unknown; MAX_WORLD_CELLS],
            ground_blocked_prefix: vec![0; (MAX_WORLD_ROWS + 1) * (MAX_WORLD_COLS + 1)],
            ground_protected_prefix: vec![0; (MAX_WORLD_ROWS + 1) * (MAX_WORLD_COLS + 1)],
            ground_split: false,
            ground_caret_cells: [None; 9],
            hull: vec![(0, 0); MAX_WORLD_ROWS],
            coverage: PetPane::default(),
            caret_cells: [None; 9],
            stamp: None,
            anchors: [None; MAX_WORLD_ANCHORS],
            selection_rect: None,
            selection_target: None,
            progress: None,
            examined_cells: 0,
        }
    }
}

impl PetWorld {
    /// Retire all geometry without reallocating the bounded buffer.
    pub fn retire(&mut self) {
        self.stamp = None;
        self.coverage = PetPane::default();
        self.ground_split = false;
        self.caret_cells = [None; 9];
        self.ground_caret_cells = [None; 9];
        self.anchors = [None; MAX_WORLD_ANCHORS];
        self.selection_rect = None;
        self.selection_target = None;
        self.progress = None;
        self.examined_cells = 0;
    }

    #[must_use]
    pub fn stamp(&self) -> Option<PetWorldStamp> {
        self.stamp
    }
    #[must_use]
    #[cfg(test)]
    pub fn coverage(&self) -> PetPane {
        self.coverage
    }
    #[must_use]
    pub fn examined_cells(&self) -> usize {
        self.examined_cells
    }
    #[must_use]
    pub fn anchors(&self) -> &[Option<PetAnchor>; MAX_WORLD_ANCHORS] {
        &self.anchors
    }
    #[must_use]
    pub fn selection_rect(&self) -> Option<PetRect> {
        self.selection_rect
    }
    #[must_use]
    pub fn selection_target(&self) -> Option<(f32, f32)> {
        self.selection_target
    }
    /// The application's last explicit state. No task identity, update event,
    /// or command completion is inferred from this sampled value.
    #[must_use]
    pub fn progress(&self) -> Option<TaskbarProgress> {
        self.progress
    }

    /// Rebuild from a coherent pre-effects snapshot. False retires every
    /// observation. Callers must not fall back to a previous successful map.
    pub fn observe(&mut self, input: &RenderInput, facts: &PetWorldFacts, pane: PetPane) -> bool {
        self.observe_with_exclusions(input, facts, pane, &[])
    }

    /// As `observe`, with pane-local search/menu/overlay exclusion rectangles.
    /// An excessive list fails closed rather than dropping a protected region.
    pub fn observe_with_exclusions(
        &mut self,
        input: &RenderInput,
        facts: &PetWorldFacts,
        pane: PetPane,
        exclusions: &[PetRect],
    ) -> bool {
        self.observe_with_reserve(input, facts, pane, PetReserve::protected(exclusions))
    }

    /// As [`Self::observe_with_exclusions`], with the host's TRANSIENT
    /// overlays apart (`overlays`: the Autopilot menu, the find bar). Both
    /// protect their cells alike; an overlay, like a live selection, is left
    /// off the ground plane a rest seat is solved on
    /// ([`Self::nearest_clear_perch`]).
    pub fn observe_with_overlays(
        &mut self,
        input: &RenderInput,
        facts: &PetWorldFacts,
        pane: PetPane,
        exclusions: &[PetRect],
        overlays: &[PetRect],
    ) -> bool {
        self.observe_with_reserve(
            input,
            facts,
            pane,
            PetReserve {
                protected: exclusions,
                overlays,
                ink: &[],
            },
        )
    }

    /// As `observe`, with the host's late overlays told apart
    /// ([`PetReserve`]): its chrome and its transient overlays protected, and
    /// the person's typed glyphs as ink. An excessive list of any fails
    /// closed.
    pub fn observe_with_reserve(
        &mut self,
        input: &RenderInput,
        facts: &PetWorldFacts,
        pane: PetPane,
        reserve: PetReserve<'_>,
    ) -> bool {
        self.observe_with_exclusions_near(input, facts, pane, reserve, None)
    }

    /// Observe fresh cells around an already admitted resident when a console
    /// hides its cursor. The hint selects coverage only: it is neither a caret
    /// nor permission to occupy any cell. Selection and a visible caret keep
    /// their normal precedence, and every coherence check still applies.
    pub(crate) fn observe_with_exclusions_near(
        &mut self,
        input: &RenderInput,
        facts: &PetWorldFacts,
        pane: PetPane,
        reserve: PetReserve<'_>,
        held_center: Option<(f32, f32)>,
    ) -> bool {
        self.retire();
        let PetReserve {
            protected: exclusions,
            overlays,
            ink: reserved_ink,
        } = reserve;
        let s = facts.stamp.surface;
        // A SUB-ROW SCROLL OFFSET IS NOT INCOHERENCE (2026-10-01). The
        // renderer applies `scroll_frac_px` at present time as one rigid
        // shift of the whole grid band — text, caret AND every free sprite
        // composited into it, the pet among them (`aterm-render`'s
        // `render_input_cached` renders the frame, sprites included, and
        // only then translates the band; `aterm-gpu` shifts its offscreen
        // after the sprites and before the chrome).
        // So the cells read here are exactly the cells the pet is drawn
        // against, and the frame's translation moves both together. Refusing
        // it blanked the resident for every frame of a trackpad glide's tail
        // or overscroll bounce at the live bottom.
        if s.terminal_id == 0
            || input.terminal_id != s.terminal_id
            || input.content_seq != facts.stamp.content_seq
            || input.engine_alt != s.alt_screen
            || input.base_y != facts.base_y
            || input.absolute_row_revision != s.absolute_row_revision
            || input.display_offset != facts.stamp.display_offset
            || pane.rows != s.rows
            || pane.cols != s.cols
            || pane.rows == 0
            || pane.cols == 0
            || pane
                .row
                .checked_add(pane.rows)
                .is_none_or(|n| n > input.rows)
            || pane
                .col
                .checked_add(pane.cols)
                .is_none_or(|n| n > input.cols)
            || input.selections.len() > MAX_REGIONS
            || exclusions.len() > MAX_REGIONS
            || exclusions.iter().any(|r| !r.valid())
            || overlays.len() > MAX_REGIONS
            || overlays.iter().any(|r| !r.valid())
            || reserved_ink.len() > MAX_REGIONS
            || reserved_ink.iter().any(|r| !r.valid())
        {
            return false;
        }
        self.stamp = Some(facts.stamp);
        self.progress = facts.progress;
        self.selection_target = selection_target(input, pane);
        let held_center = held_center.filter(|&(row, col)| {
            input.display_offset == 0
                && row.is_finite()
                && col.is_finite()
                && row >= 0.0
                && col >= 0.0
                && row < pane.rows as f32
                && col < pane.cols as f32
        });
        let center = if input.display_offset != 0 || !input.cursor_visible {
            self.selection_target
                .or(held_center)
                .unwrap_or((pane.rows as f32 * 0.5, pane.cols as f32 * 0.5))
        } else {
            (
                (input.cursor_row.saturating_sub(pane.row)).min(pane.rows - 1) as f32,
                (input.cursor_col.saturating_sub(pane.col)).min(pane.cols - 1) as f32,
            )
        };
        let rows = pane.rows.min(MAX_WORLD_ROWS);
        let cols = pane.cols.min(MAX_WORLD_COLS);
        self.coverage = PetPane {
            row: (center.0 as usize)
                .saturating_sub(rows / 2)
                .min(pane.rows - rows),
            col: (center.1 as usize)
                .saturating_sub(cols / 2)
                .min(pane.cols - cols),
            rows,
            cols,
        };
        // With no selection every cell answers `false`, the common frame, so
        // ask once instead of per cell.
        let any_selection = input.any_selection();
        // The ground plane is written only where a transient cell may exist;
        // the common frame (no selection, no overlay) writes nothing more.
        let track_ground = any_selection || !overlays.is_empty();
        for r in 0..rows {
            let local_r = self.coverage.row + r;
            let frame_r = pane.row + local_r;
            let Some(row) = input.cells.get(frame_r) else {
                // Present rows overwrite every covered cell below, including
                // their implicit blank tails. Only a missing row needs clearing;
                // all readers are fenced by coverage, so untouched buffer space
                // outside this observation cannot become stale geometry.
                let start = r * MAX_WORLD_COLS;
                self.cells[start..start + cols].fill(PetCell::Unknown);
                self.glyphs[start..start + cols].fill(None);
                self.inked[start..start + cols].fill(true);
                if track_ground {
                    self.ground_cells[start..start + cols].fill(PetCell::Unknown);
                }
                self.hull[r] = (0, cols as u16);
                continue;
            };
            // This row's ink hull, map-local (`hull`).
            let (mut hull_lo, mut hull_hi) = (u16::MAX, 0u16);
            // The exclusions that touch this row, as pane-local column spans
            // (`exclusion_span`). Every cell asks only this row's few spans,
            // where asking every exclusion of every cell cost a float test
            // per cell per exclusion, every frame.
            let mut spans = [(0, 0); MAX_REGIONS];
            let mut n_spans = 0;
            for rect in exclusions {
                if let Some(span) = exclusion_span(*rect, local_r, pane.cols) {
                    spans[n_spans] = span;
                    n_spans += 1;
                }
            }
            let spans = &spans[..n_spans];
            // The transient overlays that touch this row, the same way.
            let mut overlay_spans = [(0, 0); MAX_REGIONS];
            let mut n_overlay = 0;
            for rect in overlays {
                if let Some(span) = exclusion_span(*rect, local_r, pane.cols) {
                    overlay_spans[n_overlay] = span;
                    n_overlay += 1;
                }
            }
            let overlay_spans = &overlay_spans[..n_overlay];
            // The person's typed glyphs that touch this row, the same way.
            let mut typed_spans = [(0, 0); MAX_REGIONS];
            let mut n_typed = 0;
            for rect in reserved_ink {
                if let Some(span) = exclusion_span(*rect, local_r, pane.cols) {
                    typed_spans[n_typed] = span;
                    n_typed += 1;
                }
            }
            let typed_spans = &typed_spans[..n_typed];
            // The row's sidecars, fetched once and walked with ascending
            // cursors (`RowScan`); identical to the per-cell `input.*_at` calls.
            let mut scan = RowScan::new(input, frame_r);
            // The implicit cell is a pure function of `default_bg`, which only
            // moves at a pane seam — never at all on a single-terminal frame.
            let mut implicit_bg = None;
            let mut implicit = RenderCell::default();
            // Counted, not `for c in 0..cols`: this body runs for every
            // examined cell of every frame, and an unoptimized build calls the
            // range iterator out of line per step. Same columns, same order.
            let mut next_c = 0;
            while next_c < cols {
                let c = next_c;
                next_c += 1;
                let local_c = self.coverage.col + c;
                let frame_c = pane.col + local_c;
                // A present RenderInput row is authoritative even when its
                // materialized prefix is empty: its absent tail is Cell::EMPTY
                // under the live default colors, by the renderer's sparse-row
                // contract. Images and selection can still cover that tail.
                // A missing ROW above remains unknown, as does map overflow.
                let default_bg = scan.default_bg(frame_c);
                if !matches!(implicit_bg, Some(bg) if bg == default_bg) {
                    implicit_bg = Some(default_bg);
                    implicit = RenderCell {
                        bg: [
                            (default_bg >> 16) as u8,
                            (default_bg >> 8) as u8,
                            default_bg as u8,
                        ],
                        ..RenderCell::default()
                    };
                }
                let cell = row.get(frame_c).unwrap_or(&implicit);
                self.examined_cells += 1;
                let selected = any_selection
                    && input.selection_contains_cell(
                        frame_r,
                        frame_c,
                        row.get(frame_c + 1).is_some_and(|n| n.wide),
                        cell.wide,
                    );
                if selected {
                    let rect = PetRect::new(local_r as f32, local_c as f32, 1.0, 1.0);
                    self.selection_rect =
                        Some(self.selection_rect.map_or(rect, |old| old.union(rect)));
                }
                // The caret's 3x3 protection ring keeps the pet from parking in
                // the line's first columns, where output lands. It is a ring
                // around the escort's own seat, so for visibility it must not
                // stand in for ink (see `caret_cells` below).
                let cursor = input.cursor_visible
                    && input.display_offset == 0
                    && frame_r.abs_diff(input.cursor_row) <= 1
                    && frame_c.abs_diff(input.cursor_col) <= 1;
                let excluded = spans.iter().any(|&(lo, hi)| lo <= local_c && local_c < hi);
                let overlaid = overlay_spans
                    .iter()
                    .any(|&(lo, hi)| lo <= local_c && local_c < hi);
                // The person's own glyph, spliced in after this observation:
                // ink in every plane but the glyph plane, which stays what the
                // grid holds (contact and attention read the program's text).
                let typed = typed_spans
                    .iter()
                    .any(|&(lo, hi)| lo <= local_c && local_c < hi);
                let bg = (u32::from(cell.bg[0]) << 16)
                    | (u32::from(cell.bg[1]) << 8)
                    | u32::from(cell.bg[2]);
                let decorated =
                    cell.underline != UnderlineStyle::None || cell.strikethrough || cell.overline;
                let compound = scan.cluster(frame_c).is_some() || scan.combining(frame_c).is_some();
                // Nonstandard line geometry is conservatively all protected.
                // A logical single cell is not a proof about its expanded pixels.
                let line = scan.line_size(frame_c);
                let glyph = cell.ch != ' ' || cell.wide || decorated || compound;
                // Occupancy and protection are independent facts. Moving the
                // caret/selection off a glyph must not look like arriving ink.
                // Expanded rows lack a certified cell-to-pixel projection.
                let speck = glyph
                    && !cell.wide
                    && !decorated
                    && !compound
                    && is_single_braille_dot(cell.ch)
                    // Adjacent ink makes this a textual/graphical run, not
                    // an isolated particle. Keep both probes inside this
                    // bounded observation; an unseen neighbour at either
                    // the pane or coverage edge cannot certify isolation.
                    && c > 0
                    && c + 1 < cols
                    // These probes step backwards, so they ask `input` directly
                    // rather than the ascending `scan` cursors; isolated
                    // braille dots are rare.
                    && [frame_c - 1, frame_c + 1].into_iter().all(|column| {
                        // A present row's implicit tail is certified blank,
                        // just like the cell being classified above.
                        let neighbour = row.get(column).unwrap_or(&implicit);
                        input.line_size_run_at(frame_r, column).0 == LineSize::SingleWidth
                            && neighbour.ch == ' '
                            && !neighbour.wide
                            && neighbour.underline == UnderlineStyle::None
                            && !neighbour.strikethrough
                            && !neighbour.overline
                            && input.cluster_at(frame_r, column).is_none()
                            && input.combining_at(frame_r, column).is_none()
                    });
                self.glyphs[r * MAX_WORLD_COLS + c] =
                    (line == LineSize::SingleWidth).then_some(if !glyph {
                        PetGlyph::Blank
                    } else if speck {
                        PetGlyph::Speck
                    } else {
                        PetGlyph::Substantive
                    });
                // What protects the cell for good (the host's chrome, an
                // image, an expanded row) and what only for now (a selection,
                // an overlay): the ground plane keeps the first alone.
                let lasting =
                    excluded || scan.image(frame_c).is_some() || line != LineSize::SingleWidth;
                let transient = selected || overlaid;
                let others = lasting || transient;
                // Painted in the text layer: a glyph, the person's typed one,
                // or a coloured background. A typed glyph is in its line's ink
                // hull already, as it will be once echoed.
                let painted = glyph || typed || bg != default_bg;
                if (glyph && !speck) || typed || bg != default_bg {
                    hull_lo = hull_lo.min(c as u16);
                    hull_hi = c as u16 + 1;
                }
                // The class this cell would have with no caret at all.
                self.inked[r * MAX_WORLD_COLS + c] = others || painted;
                // Recorded so the visibility layer can forgive a cell the
                // LIVE CARET'S RING alone protects, without forgiving
                // anything else. The pet is seated beside the caret BY
                // DESIGN; deleting the cat because the caret's own keep-off
                // margin slid under it is what v0.81.0 did on every
                // keystroke. `blank` is whether the cell would have been
                // `Clear` with no caret at all — the stricter
                // `blocked_prefix` reading forgives only those, while the
                // `protected_prefix` reading (a resident standing UNDER
                // ordinary ink) may forgive the whole ring, because without
                // the caret every one of these cells is `Ink` or `Clear` and
                // that layer admits both.
                if let Some(slot) = (cursor && !others)
                    .then(|| self.caret_cells.iter_mut().find(|s| s.is_none()))
                    .flatten()
                {
                    *slot = Some(CaretCell {
                        row: r,
                        col: c,
                        blank: !painted,
                    });
                }
                let class = if others || cursor {
                    PetCell::Protected
                } else if painted {
                    PetCell::Ink
                } else {
                    PetCell::Clear
                };
                self.cells[r * MAX_WORLD_COLS + c] = class;
                if let Some(slot) = (track_ground && cursor && transient && !lasting)
                    .then(|| self.ground_caret_cells.iter_mut().find(|s| s.is_none()))
                    .flatten()
                {
                    *slot = Some(CaretCell {
                        row: r,
                        col: c,
                        blank: !painted,
                    });
                }
                if track_ground {
                    self.ground_cells[r * MAX_WORLD_COLS + c] = if transient {
                        self.ground_split = true;
                        if lasting || cursor {
                            PetCell::Protected
                        } else if painted {
                            PetCell::Ink
                        } else {
                            PetCell::Clear
                        }
                    } else {
                        class
                    };
                }
            }
            self.hull[r] = (hull_lo, hull_hi);
        }
        // A rectangle costs four reads regardless of sprite size. Counts fit
        // u16 because the complete inspected window has at most 16,384 cells.
        let stride = MAX_WORLD_COLS + 1;
        // Only the top and left borders need resetting: every interior cell in
        // this scan is overwritten below, and rectangle queries cannot reach
        // retained cells outside coverage.
        self.blocked_prefix[..=cols].fill(0);
        self.protected_prefix[..=cols].fill(0);
        for r in 0..rows {
            self.blocked_prefix[(r + 1) * stride] = 0;
            self.protected_prefix[(r + 1) * stride] = 0;
            // Counted for the same reason as the cell scan above.
            let mut c = 0;
            while c < cols {
                let index = (r + 1) * stride + c + 1;
                self.blocked_prefix[index] = self.blocked_prefix[index - 1]
                    + self.blocked_prefix[index - stride]
                    - self.blocked_prefix[index - stride - 1]
                    + u16::from(self.cells[r * MAX_WORLD_COLS + c] != PetCell::Clear);
                self.protected_prefix[index] = self.protected_prefix[index - 1]
                    + self.protected_prefix[index - stride]
                    - self.protected_prefix[index - stride - 1]
                    + u16::from(matches!(
                        self.cells[r * MAX_WORLD_COLS + c],
                        PetCell::Unknown | PetCell::Protected
                    ));
                c += 1;
            }
        }
        if self.ground_split {
            // The ground plane's two tables, the same two readings of
            // `ground_cells`; built only when a transient cell exists.
            self.ground_blocked_prefix[..=cols].fill(0);
            self.ground_protected_prefix[..=cols].fill(0);
            for r in 0..rows {
                self.ground_blocked_prefix[(r + 1) * stride] = 0;
                self.ground_protected_prefix[(r + 1) * stride] = 0;
                for c in 0..cols {
                    let index = (r + 1) * stride + c + 1;
                    let cell = self.ground_cells[r * MAX_WORLD_COLS + c];
                    self.ground_blocked_prefix[index] = self.ground_blocked_prefix[index - 1]
                        + self.ground_blocked_prefix[index - stride]
                        - self.ground_blocked_prefix[index - stride - 1]
                        + u16::from(cell != PetCell::Clear);
                    self.ground_protected_prefix[index] = self.ground_protected_prefix[index - 1]
                        + self.ground_protected_prefix[index - stride]
                        - self.ground_protected_prefix[index - stride - 1]
                        + u16::from(matches!(cell, PetCell::Unknown | PetCell::Protected));
                }
            }
        }
        // A target must name selected pixels we actually examined. It never
        // stands in for a live caret and never authorizes occupying selection.
        if self.selection_rect.is_none() {
            self.selection_target = None;
        }
        if !s.alt_screen {
            for (slot, block) in self.anchors.iter_mut().zip(facts.blocks.iter()) {
                let Some(block) = block else {
                    continue;
                };
                let top = facts.stamp.top_absolute_row;
                let end = top.saturating_add(pane.rows as u64);
                let start = block.output_row.unwrap_or(block.start_row).max(top);
                let last = block.end_row.min(end).saturating_sub(1);
                if start > last {
                    continue;
                }
                let mut absolute_row = start;
                let mut col = 0.0;
                // The frontier is glyph geometry inside this actual block;
                // repeated frames cannot turn its position into a new event.
                for r in 0..rows {
                    let abs = top.saturating_add((self.coverage.row + r) as u64);
                    if abs < start || abs > last {
                        continue;
                    }
                    for c in 0..cols {
                        if matches!(
                            self.glyphs[r * MAX_WORLD_COLS + c],
                            Some(PetGlyph::Speck | PetGlyph::Substantive)
                        ) {
                            absolute_row = abs;
                            col = (self.coverage.col + c + 1) as f32;
                        }
                    }
                }
                *slot = Some(PetAnchor {
                    surface: s,
                    block_id: block.id,
                    absolute_row,
                    row: (absolute_row - top) as f32,
                    col,
                    state: block.state,
                    exit_code: block.exit_code,
                });
            }
        }
        true
    }

    #[must_use]
    pub fn cell(&self, row: usize, col: usize) -> PetCell {
        if self.stamp.is_none() {
            return PetCell::Unknown;
        }
        let Some(r) = row.checked_sub(self.coverage.row) else {
            return PetCell::Unknown;
        };
        let Some(c) = col.checked_sub(self.coverage.col) else {
            return PetCell::Unknown;
        };
        if r >= self.coverage.rows || c >= self.coverage.cols {
            return PetCell::Unknown;
        }
        self.cells[r * MAX_WORLD_COLS + c]
    }

    /// Actual glyph/decorated-cell occupancy, independent of caret/selection
    /// protection. None means unobserved or unsupported geometry, never blank.
    #[must_use]
    pub fn ink_at(&self, row: usize, col: usize) -> Option<bool> {
        self.glyph_at(row, col)
            .map(|glyph| glyph != PetGlyph::Blank)
    }

    /// Ink substantial enough to displace a pet drawn beneath the glyphs.
    /// Only an isolated, undecorated one-dot Braille cell is exempt; actual
    /// glyph occupancy, protected surfaces and unknown coverage are unchanged.
    #[must_use]
    pub fn locomotion_ink_at(&self, row: usize, col: usize) -> Option<bool> {
        self.glyph_at(row, col)
            .map(|glyph| glyph == PetGlyph::Substantive)
    }

    fn glyph_at(&self, row: usize, col: usize) -> Option<PetGlyph> {
        self.stamp?;
        let r = row.checked_sub(self.coverage.row)?;
        let c = col.checked_sub(self.coverage.col)?;
        if r >= self.coverage.rows || c >= self.coverage.cols {
            return None;
        }
        self.glyphs[r * MAX_WORLD_COLS + c]
    }

    /// Certify all cells touched by the rectangle and a margin on every side.
    ///
    /// Fail-closed and two-valued: a rectangle this map never examined answers
    /// `false`, like an obstructed one. Right for placement (never stand where
    /// you cannot see), wrong for visibility; [`Self::clearance`] is the
    /// three-valued answer.
    #[must_use]
    pub fn clear(&self, rect: PetRect, margin: f32) -> bool {
        self.clearance(rect, margin) == Some(true)
    }

    /// The full body may stand behind ordinary text and coloured backgrounds;
    /// selection, cursor protection, images and unknown geometry still exclude
    /// it. The renderer must draw an admitted occupied body under the text.
    ///
    /// Fail-closed and two-valued like [`Self::clear`];
    /// [`Self::under_text_clearance`] is the three-valued answer.
    #[must_use]
    pub fn under_text_clear(&self, rect: PetRect, margin: f32) -> bool {
        self.under_text_clearance(rect, margin) == Some(true)
    }

    /// Three-valued clearance:
    ///
    /// * `Some(true)`: every cell the rectangle and margin touch was examined by
    ///   this snapshot and is free.
    /// * `Some(false)`: an examined cell is occupied or protected, or the query
    ///   is degenerate (non-finite rectangle or margin).
    /// * `None`: unknown. There is no coherent snapshot, or the rectangle leaves
    ///   the coverage window, which is centred on the caret and slides as the
    ///   user types. Collapsing this to `false` invents ink.
    #[must_use]
    pub fn clearance(&self, rect: PetRect, margin: f32) -> Option<bool> {
        self.clearance_in(&self.blocked_prefix, Forgive::Nothing, rect, margin)
    }

    /// [`Self::clearance`] for a resident admitted BEHIND ordinary text: the
    /// same three answers, counted over protected and unknown cells only.
    #[must_use]
    pub fn under_text_clearance(&self, rect: PetRect, margin: f32) -> Option<bool> {
        self.clearance_in(&self.protected_prefix, Forgive::Nothing, rect, margin)
    }

    /// As [`Self::clearance`], except that the live caret's own ring cells, where
    /// nothing else protects them, do not count as obstruction.
    ///
    /// Placement keeps the caret protected ([`Self::clearance`]) so the pet never
    /// rests on the cursor; visibility uses this, so a pet walking past the
    /// caret is not hidden while it overlaps it.
    ///
    /// Only ring cells that would be `Clear` without the caret are forgiven; a
    /// ring cell with a glyph, selection, image or non-default background stays
    /// blocked.
    #[must_use]
    pub fn clearance_past_caret(&self, rect: PetRect, margin: f32) -> Option<bool> {
        self.clearance_in(&self.blocked_prefix, Forgive::BlankCaret, rect, margin)
    }

    /// [`Self::clearance_past_caret`] as a two-valued, fail-closed answer:
    /// occupancy for a body beside the live caret, discounting only the caret's
    /// blank ring cells.
    #[must_use]
    pub fn clear_past_caret(&self, rect: PetRect, margin: f32) -> bool {
        self.clearance_past_caret(rect, margin) == Some(true)
    }

    /// The visibility verdict the emitted sprite is judged by: a resident may
    /// stand under ordinary ink, and the caret's ring does not obstruct its
    /// escort. Every ring cell is forgivable here (absent the caret each is
    /// `Ink` or `Clear`); selections, images and unknown geometry still veto.
    #[must_use]
    pub fn under_text_clearance_past_caret(&self, rect: PetRect, margin: f32) -> Option<bool> {
        self.clearance_in(&self.protected_prefix, Forgive::AnyCaret, rect, margin)
    }

    /// [`Self::under_text_clearance_past_caret`] over what this map SEES of
    /// `rect`, for a body that was already on glass before this map judged
    /// it (a crossfade ghost on the surface that replaced its own):
    ///
    /// * `Some(false)`: a cell this map covers under it is protected, or the
    ///   query is degenerate — however much of it lies outside the coverage
    ///   window;
    /// * `Some(true)`: it lies wholly inside the coverage window and nothing
    ///   there is protected;
    /// * `None`: the part this map covers, if any, is clear, and the rest is
    ///   outside the window. Unknown is not evidence, and the caller keeps the
    ///   verdict the body came with; but a protected cell the map can see
    ///   always is.
    #[must_use]
    pub fn seen_under_text_clearance_past_caret(&self, rect: PetRect) -> Option<bool> {
        self.stamp?;
        let whole = self.under_text_clearance_past_caret(rect, 0.0);
        if whole.is_some() {
            return whole;
        }
        let cov = self.coverage;
        let (r0, c0) = (rect.row.max(cov.row as f32), rect.col.max(cov.col as f32));
        let r1 = (rect.row + rect.rows).min((cov.row + cov.rows) as f32);
        let c1 = (rect.col + rect.cols).min((cov.col + cov.cols) as f32);
        if !(r0 < r1 && c0 < c1) {
            return None;
        }
        let seen = PetRect::new(r0, c0, r1 - r0, c1 - c0);
        match self.under_text_clearance_past_caret(seen, 0.0) {
            Some(false) => Some(false),
            _ => None,
        }
    }

    /// Z-ORDER, three-valued: is the rectangle free of anything painted in
    /// the text layer, so a body there may be drawn IN FRONT of it? Every
    /// non-`Clear` cell counts except the live caret's keep-off ring. `None`
    /// outside this map; a degenerate query and an unobserved cell answer
    /// inked, the safe side for layering.
    ///
    /// THE CARET'S RING IS NOT INK. It keeps the pet from coming to rest on
    /// the cursor, but a blank ring cell paints nothing for the body to sit
    /// in front of or behind (the cursor itself is drawn above every pet
    /// layer). Counting it (the z-order was `!clear(body)`) flipped a typing
    /// escort under and over the glyphs at the keystroke rate as its body
    /// crossed caret+1.
    ///
    /// A walk, not a prefix plane: its one caller asks once per frame about
    /// a sprite-sized window, far cheaper than a table rebuilt per observation.
    #[must_use]
    pub fn ink_clearance(&self, rect: PetRect, margin: f32) -> Option<bool> {
        self.stamp?;
        match self.window(rect, margin) {
            Window::Degenerate => Some(false),
            Window::Outside => None,
            Window::Cells(r0, c0, r1, c1) => Some((r0..r1).all(|r| {
                !self.inked[r * MAX_WORLD_COLS + c0..r * MAX_WORLD_COLS + c1].contains(&true)
            })),
        }
    }

    fn clearance_in(
        &self,
        prefix: &[u16],
        forgive: Forgive,
        rect: PetRect,
        margin: f32,
    ) -> Option<bool> {
        self.clearance_on(prefix, false, forgive, rect, margin)
    }

    /// The live caret's ring cells a plane forgives: `caret_cells`, and on
    /// the ground plane also those only a selection or an overlay hid.
    fn carets(&self, ground: bool) -> impl Iterator<Item = &CaretCell> {
        let extra: &[Option<CaretCell>] = if ground && self.ground_split {
            &self.ground_caret_cells
        } else {
            &[]
        };
        self.caret_cells.iter().chain(extra).flatten()
    }

    /// [`Self::clearance_in`] on the live plane, or (`ground`) the ground
    /// plane `prefix` was read from.
    fn clearance_on(
        &self,
        prefix: &[u16],
        ground: bool,
        forgive: Forgive,
        rect: PetRect,
        margin: f32,
    ) -> Option<bool> {
        self.stamp?;
        match self.window(rect, margin) {
            Window::Degenerate => Some(false),
            Window::Outside => None,
            Window::Cells(r0, c0, r1, c1) => {
                let forgiven = self
                    .carets(ground)
                    .filter(|cell| match forgive {
                        Forgive::Nothing => false,
                        Forgive::BlankCaret => cell.blank,
                        Forgive::AnyCaret => true,
                    })
                    .filter(|cell| {
                        cell.row >= r0 && cell.row < r1 && cell.col >= c0 && cell.col < c1
                    })
                    .count();
                Some(usize::from(self.blocked(prefix, r0, c0, r1, c1)) == forgiven)
            }
        }
    }

    /// The half-open map-local cell window a rectangle and its margin touch.
    fn window(&self, rect: PetRect, margin: f32) -> Window {
        if !rect.valid() || !margin.is_finite() || margin < 0.0 {
            return Window::Degenerate;
        }
        let (r0, c0) = (rect.row - margin, rect.col - margin);
        let (r1, c1) = (rect.row + rect.rows + margin, rect.col + rect.cols + margin);
        if !r1.is_finite() || !c1.is_finite() {
            return Window::Degenerate;
        }
        if r0 < self.coverage.row as f32
            || c0 < self.coverage.col as f32
            || r1 > (self.coverage.row + self.coverage.rows) as f32
            || c1 > (self.coverage.col + self.coverage.cols) as f32
        {
            return Window::Outside;
        }
        Window::Cells(
            r0.floor() as usize - self.coverage.row,
            c0.floor() as usize - self.coverage.col,
            r1.ceil() as usize - self.coverage.row,
            c1.ceil() as usize - self.coverage.col,
        )
    }

    /// Cells the supplied prefix table counts in a map-local window: four
    /// reads, whatever the sprite's size.
    fn blocked(&self, prefix: &[u16], r0: usize, c0: usize, r1: usize, c1: usize) -> u16 {
        let at = |r: usize, c: usize| prefix[r * (MAX_WORLD_COLS + 1) + c];
        at(r1, c1) + at(r0, c0) - at(r1, c0) - at(r0, c1)
    }

    /// The swept-body test for a resident travelling behind text, for a body
    /// leaving the caret's escort seat.
    ///
    /// A swept corridor contains the body it departs from, and [`STATION_LEAD`]
    /// seats the escort one cell past the caret, inside the caret's keep-off
    /// ring; read strictly, the pet could never leave. So the caret does not
    /// obstruct its escort here either, matching
    /// [`Self::under_text_clearance_past_caret`].
    #[must_use]
    pub fn under_text_corridor_clear_past_caret(
        &self,
        from: PetRect,
        to: PetRect,
        margin: f32,
    ) -> bool {
        from.valid()
            && to.valid()
            && self.under_text_clearance_past_caret(from.union(to), margin) == Some(true)
    }

    /// The ground plane's `(blocked, protected)` prefix tables
    /// (`ground_cells`): the live ones where no transient cell was observed.
    fn ground_prefixes(&self) -> (&[u16], &[u16]) {
        if self.ground_split {
            (&self.ground_blocked_prefix, &self.ground_protected_prefix)
        } else {
            (&self.blocked_prefix, &self.protected_prefix)
        }
    }

    /// [`Self::clear`] on the GROUND PLANE: as if no selection were live and
    /// no transient overlay were open (`ground_cells`).
    #[must_use]
    pub fn ground_clear(&self, rect: PetRect, margin: f32) -> bool {
        self.clearance_on(
            self.ground_prefixes().0,
            true,
            Forgive::Nothing,
            rect,
            margin,
        ) == Some(true)
    }

    /// [`Self::under_text_clearance_past_caret`] on the GROUND PLANE: what
    /// protects `rect` for good (the host's chrome, an image, an expanded
    /// row, the caret's ring is forgiven), not a selection or an overlay.
    #[must_use]
    pub fn ground_under_text_clearance_past_caret(
        &self,
        rect: PetRect,
        margin: f32,
    ) -> Option<bool> {
        self.clearance_on(
            self.ground_prefixes().1,
            true,
            Forgive::AnyCaret,
            rect,
            margin,
        )
    }

    /// Whether a body can go from `from` to `to` without crossing anything
    /// that protects the ground for good: their swept box, with no margin,
    /// clear on the ground plane past the caret
    /// ([`Self::ground_under_text_clearance_past_caret`]). Ink is crossed
    /// behind the text.
    #[must_use]
    pub fn ground_corridor_clear(&self, from: PetRect, to: PetRect) -> bool {
        from.valid()
            && to.valid()
            && self.ground_under_text_clearance_past_caret(from.union(to), 0.0) == Some(true)
    }

    /// Whether something that protects the ground for good is OBSERVED
    /// between `from` and `to`: a protected cell in their swept box, with no
    /// margin, on the ground plane past the caret. Not the negation of
    /// [`Self::ground_corridor_clear`]: an unknown cell walls nothing.
    #[must_use]
    pub fn ground_corridor_walled(&self, from: PetRect, to: PetRect) -> bool {
        from.valid()
            && to.valid()
            && self.ground_under_text_clearance_past_caret(from.union(to), 0.0) == Some(false)
    }

    /// Whether `rect` and its margin stay off every observed row's ink hull
    /// (`hull`): no gap inside a line counts as ground. `false` outside the
    /// coverage window and for a degenerate query.
    #[must_use]
    pub fn hull_clear(&self, rect: PetRect, margin: f32) -> bool {
        if self.stamp.is_none() {
            return false;
        }
        match self.window(rect, margin) {
            Window::Cells(r0, c0, r1, c1) => (r0..r1).all(|r| {
                let (lo, hi) = self.hull[r];
                lo >= hi || c1 <= usize::from(lo) || c0 >= usize::from(hi)
            }),
            _ => false,
        }
    }

    /// Cells the ground plane protects in a map-local window, less the live
    /// caret's ring (forgiven, as [`Self::ground_under_text_clearance_past_caret`]
    /// forgives it).
    fn ground_protected_count(&self, r0: usize, c0: usize, r1: usize, c1: usize) -> u16 {
        let forgiven = self
            .carets(true)
            .filter(|cell| cell.row >= r0 && cell.row < r1 && cell.col >= c0 && cell.col < c1)
            .count() as u16;
        self.blocked(self.ground_prefixes().1, r0, c0, r1, c1)
            .saturating_sub(forgiven)
    }

    /// The map-local columns `[lo, hi)` a body at map-local cells
    /// `from = (r0, c0, r1, c1)` can reach on rows `r0..r1` widened to
    /// `rows`, crossing nothing the ground plane protects: the widest span
    /// round its own columns that is clear on every row in between. `None`
    /// when even its own columns are not (protected ground lies between its
    /// rows and those). A seat whose cells lie inside the span is reachable
    /// by a corridor ([`Self::ground_corridor_clear`]) that box contains.
    fn reach_span(
        &self,
        from: (usize, usize, usize, usize),
        rows: (usize, usize),
    ) -> Option<(usize, usize)> {
        let (fr0, fc0, fr1, fc1) = from;
        let (r0, r1) = (rows.0.min(fr0), rows.1.max(fr1));
        if self.ground_protected_count(r0, fc0, r1, fc1) > 0 {
            return None;
        }
        // The protected count of `r0..r1 x [x, fc0)` falls as `x` grows:
        // the least `x` where it is zero.
        let (mut lo, mut hi) = (0, fc0);
        while lo < hi {
            let mid = (lo + hi) / 2;
            if self.ground_protected_count(r0, mid, r1, fc0) == 0 {
                hi = mid;
            } else {
                lo = mid + 1;
            }
        }
        let left = lo;
        // ... and that of `[fc1, y)` grows with `y`: the most `y` where it is.
        let (mut lo, mut hi) = (fc1, self.coverage.cols);
        while lo < hi {
            let mid = (lo + hi).div_ceil(2);
            if self.ground_protected_count(r0, fc1, r1, mid) == 0 {
                lo = mid;
            } else {
                hi = mid - 1;
            }
        }
        Some((left, lo))
    }

    /// Resolve a retained content row only while the exact owner and row
    /// identity fences hold and its block is still observed. A content change
    /// still requires the caller to certify the intended body with `clear`.
    #[must_use]
    pub fn resolve(&self, anchor: PetAnchor) -> Option<PetAnchor> {
        let stamp = self.stamp?;
        if stamp.surface != anchor.surface || stamp.surface.alt_screen {
            return None;
        }
        let current = self
            .anchors
            .iter()
            .flatten()
            .find(|a| a.block_id == anchor.block_id)?;
        let row = anchor.absolute_row.checked_sub(stamp.top_absolute_row)?;
        if row >= stamp.surface.rows as u64 {
            return None;
        }
        Some(PetAnchor {
            row: row as f32,
            state: current.state,
            exit_code: current.exit_code,
            ..anchor
        })
    }

    /// Nearest certified body placement among eight local row searches. Each
    /// search inspects at most the bounded map width and never allocates.
    #[must_use]
    pub fn nearest_perch(&self, near: PetRect, margin: f32, max_distance: f32) -> Option<PetRect> {
        self.perch_near((near.foot_row(), near.col), near, margin, max_distance)
    }

    /// Prefer empty space near home, then the same bounded search under text.
    /// Never expand the search across the console just to avoid ordinary ink.
    #[must_use]
    pub fn home_perch(&self, near: PetRect, margin: f32, max_distance: f32) -> Option<PetRect> {
        if !max_distance.is_finite() || max_distance < 0.0 {
            return None;
        }
        if self.clear(near, margin) {
            return Some(near);
        }
        self.nearest_perch(near, margin, max_distance).or_else(|| {
            if self.under_text_clear(near, margin) {
                Some(near)
            } else {
                self.nearest_under_text_perch(near, margin, max_distance)
            }
        })
    }

    /// The nearest REST SEAT: a placement whose whole body, margin included,
    /// is clean ground, for a resident that has to leave its stand
    /// (`kitty_pet::console_life`'s `console_station`), so it does not settle
    /// on words when a blank seat is in reach. Clean ground is, on the
    /// GROUND PLANE (`ground_cells`: no live selection, no transient
    /// overlay; see [`Self::ground_clear`]), no glyph, no coloured
    /// background, nothing protected, no live caret's ring, nothing
    /// unobserved, and no row's ink hull ([`Self::hull_clear`]): a gap
    /// inside a line, between a command and its description, is not ground.
    ///
    /// `from`, the body as it stands on glass, keeps the seat within its
    /// reach: a seat counts only where the box swept from `from` to it is
    /// clear of protected ground ([`Self::ground_corridor_clear`]). A seat
    /// beyond the host's chrome (a rule it may never cross) is never chosen
    /// for a body on the near side: chasing it, the console law stepped
    /// every flight back off the rule, and the cat leapt in place without
    /// end. `None` (a body not yet on glass) asks for no path.
    ///
    /// [`Self::perch_near`] tries eight candidate rows around the stand. This
    /// walks every row band in reach instead, each aligned so the body and its
    /// margin fill whole rows: a 1.7-row body with a 0.1 margin then needs two
    /// clear rows, not the three a body with integral feet touches. Two blank
    /// rows under a one-line footer, or a footer's blank tail above the last
    /// row, is exactly the ground a full-screen program leaves.
    ///
    /// Rows cost twice a column, as in [`Self::perch_near`], but the two add
    /// as a Euclidean length, not a maximum. `perch_near` looks only a few
    /// rows away. Across a whole pane a maximum lets a seat eight rows up
    /// beat one beside the last line, if its line ends three columns sooner
    /// (lab, Claude Code 2.1.304, 120 x 16). Ties go down, then right, the ink
    /// ladder's order (`kitty_pet`'s `ink_stand_for`), so a pet below a line
    /// never covers it.
    ///
    /// It runs on the frame path, so it reads the prefix tables sparingly. A
    /// band with a row that has no room for the body in reach costs two reads
    /// (a rule, a full line). Elsewhere a blocked seat's rightmost (or
    /// leftmost) blocked column, or a row's hull, sends the walk past it, so
    /// a line of words is crossed a word at a time; `from`'s reach is a span
    /// per band, found by bisection. The walk stops at the first band that
    /// cannot beat the best seat found.
    #[must_use]
    pub fn nearest_clear_perch(
        &self,
        near: PetRect,
        margin: f32,
        max_distance: f32,
        from: Option<PetRect>,
    ) -> Option<PetRect> {
        self.stamp?;
        if !near.valid()
            || !margin.is_finite()
            || margin < 0.0
            || !max_distance.is_finite()
            || max_distance < 0.0
        {
            return None;
        }
        // Whole cells the body and its margin fill, with a sliver of slack
        // on each side so float rounding can never tip the window into the
        // neighbouring row or column ([`Self::window`] floors and ceils).
        let fill = |extent: f32| -> Option<(usize, f32)> {
            let cells = (extent + 2.0 * margin + 2.0 * PERCH_SLACK).ceil();
            (cells.is_finite() && cells >= 1.0).then_some((
                cells as usize,
                margin + (cells - extent - 2.0 * margin) * 0.5,
            ))
        };
        let (span_r, inset_r) = fill(near.rows)?;
        let (span_c, inset_c) = fill(near.cols)?;
        let cov = self.coverage;
        if span_r > cov.rows || span_c > cov.cols {
            return None;
        }
        // The body on glass as map-local cells; one outside this map is no
        // evidence about a path, so it asks for none.
        let from = from.and_then(|from| match self.window(from, 0.0) {
            Window::Cells(r0, c0, r1, c1) => Some((r0, c0, r1, c1)),
            Window::Degenerate | Window::Outside => None,
        });
        let last_band = (cov.rows - span_r) as i64;
        let last_col = (cov.cols - span_c) as i64;
        // The map-local band and column whose seat sits on `near`, and the
        // seat column's distance from it.
        let home_band = ((near.row - inset_r - cov.row as f32).round() as i64).clamp(0, last_band);
        let home_x = near.col - inset_c - cov.col as f32;
        let home_col = (home_x.round() as i64).clamp(0, last_col);
        let top = |band: i64| cov.row as f32 + band as f32 + inset_r;
        let left = |col: i64| cov.col as f32 + col as f32 + inset_c;
        let prefix = self.ground_prefixes().0;
        let mut best = None;
        let mut best_distance = f32::INFINITY;
        // Bands outward from home, down before up; a step whose bands are all
        // out of the map, out of reach or no nearer than the best ends it.
        for step in 0..=last_band {
            let mut live_band = false;
            for band in [home_band + step, home_band - step] {
                if !(0..=last_band).contains(&band) || (step == 0 && band != home_band) {
                    continue;
                }
                let row_distance = (top(band) - near.row).abs() * 2.0;
                let reach = max_distance.min(best_distance);
                if row_distance > max_distance || row_distance >= best_distance {
                    continue;
                }
                live_band = true;
                let r0 = band as usize;
                let r1 = r0 + span_r;
                // The columns `from` reaches on this band's rows.
                let (reach_lo, reach_hi) = match from {
                    Some(from) => match self.reach_span(from, (r0, r1)) {
                        Some(span) => span,
                        None => continue,
                    },
                    None => (0, cov.cols),
                };
                if reach_hi < reach_lo + span_c {
                    continue;
                }
                // The seat columns this band may still win with.
                let span = (reach * reach - row_distance * row_distance)
                    .max(0.0)
                    .sqrt();
                let lo = ((home_x - span).ceil() as i64).max(reach_lo as i64);
                let hi = ((home_x + span).floor() as i64)
                    .min(last_col)
                    .min((reach_hi - span_c) as i64);
                if lo > hi {
                    continue;
                }
                let (c_lo, c_hi) = (lo as usize, hi as usize + span_c);
                // A row with fewer clear cells in reach than the body is wide
                // rules the band out: a rule, a full line, a status bar.
                let width = (c_hi - c_lo) as u16;
                if (r0..r1)
                    .any(|r| width - self.blocked(prefix, r, c_lo, r + 1, c_hi) < span_c as u16)
                {
                    continue;
                }
                let seat_blocked = |c: i64| {
                    let c = c as usize;
                    self.blocked(prefix, r0, c, r1, c + span_c) > 0
                };
                let column_blocked = |c: i64| {
                    let c = c as usize;
                    self.blocked(prefix, r0, c, r1, c + 1) > 0
                };
                // The hulls a seat at `c` meets: the least start and the
                // greatest end among them, or `None`.
                let hulls = |c: i64| {
                    let (c0, c1) = (c as u16, (c as usize + span_c) as u16);
                    self.hull[r0..r1]
                        .iter()
                        .filter(|&&(lo, hi)| lo < hi && c0 < hi && c1 > lo)
                        .fold(None, |met: Option<(u16, u16)>, &(lo, hi)| {
                            Some(met.map_or((lo, hi), |(a, b)| (a.min(lo), b.max(hi))))
                        })
                };
                // Rightward from home: past a blocked seat's rightmost
                // blocked column, or past the hull it meets.
                let mut right = None;
                let mut c = home_col.max(lo);
                while c <= hi {
                    if seat_blocked(c) {
                        let mut x = c + span_c as i64 - 1;
                        while x > c && !column_blocked(x) {
                            x -= 1;
                        }
                        c = x + 1;
                    } else if let Some((_, end)) = hulls(c) {
                        c = i64::from(end);
                    } else {
                        right = Some(c);
                        break;
                    }
                }
                // Leftward: past a blocked seat's leftmost blocked column, or
                // before the hull it meets.
                let mut leftward = None;
                let mut c = (home_col - 1).min(hi);
                while c >= lo {
                    if seat_blocked(c) {
                        let end = c + span_c as i64 - 1;
                        let mut x = c;
                        while x < end && !column_blocked(x) {
                            x += 1;
                        }
                        c = x - span_c as i64;
                    } else if let Some((start, _)) = hulls(c) {
                        c = i64::from(start) - span_c as i64;
                    } else {
                        leftward = Some(c);
                        break;
                    }
                }
                // Right before left on a tie.
                for col in [right, leftward].into_iter().flatten() {
                    let distance = row_distance.hypot((left(col) - near.col).abs());
                    if distance <= max_distance && distance < best_distance {
                        best = Some(PetRect::new(top(band), left(col), near.rows, near.cols));
                        best_distance = distance;
                    }
                }
            }
            if !live_band && step > 0 {
                break;
            }
        }
        best
    }

    /// Closest protected-free placement, without preferring distant blank ink.
    #[must_use]
    pub fn nearest_under_text_perch(
        &self,
        near: PetRect,
        margin: f32,
        max_distance: f32,
    ) -> Option<PetRect> {
        self.nearest_under_text_perch_from(near, margin, max_distance, None)
    }

    /// Closest protected-free placement reachable from `from` without crossing
    /// protected ground ([`Self::ground_corridor_clear`]).
    #[must_use]
    pub fn nearest_under_text_perch_from(
        &self,
        near: PetRect,
        margin: f32,
        max_distance: f32,
        from: Option<PetRect>,
    ) -> Option<PetRect> {
        self.perch_near_with(
            (near.foot_row(), near.col),
            near,
            margin,
            max_distance,
            true,
            from,
        )
    }

    /// `target` is an attention location `(row, col)`, not a fabricated caret.
    /// The returned destination is clear; travel must separately clear the
    /// corridor from the current body ([`Self::clear`] over the union of the
    /// two bodies).
    #[must_use]
    pub fn perch_near(
        &self,
        target: (f32, f32),
        body: PetRect,
        margin: f32,
        max_distance: f32,
    ) -> Option<PetRect> {
        self.perch_near_with(target, body, margin, max_distance, false, None)
    }

    fn perch_near_with(
        &self,
        target: (f32, f32),
        body: PetRect,
        margin: f32,
        max_distance: f32,
        under_text: bool,
        from: Option<PetRect>,
    ) -> Option<PetRect> {
        if !body.valid()
            || !target.0.is_finite()
            || !target.1.is_finite()
            || !margin.is_finite()
            || margin < 0.0
            || !max_distance.is_finite()
            || max_distance < 0.0
        {
            return None;
        }
        let want_row = target.0 + 1.0 - body.rows;
        let step = body.rows + 2.0 * margin;
        let rows = [
            body.row,
            want_row,
            want_row + step,
            want_row - step,
            want_row + 1.0,
            want_row - 1.0,
            want_row + 2.0 * step,
            want_row - 2.0 * step,
        ];
        let mut best = None;
        let mut best_distance = f32::INFINITY;
        let mut best_tie = f32::INFINITY;
        // The allowed neighbourhood bounds the loop, not just its answers.
        let first_col = (target.1 - max_distance - self.coverage.col as f32 - margin)
            .ceil()
            .max(0.0) as usize;
        let end_col = ((target.1 + max_distance - self.coverage.col as f32 - margin).floor() + 1.0)
            .max(0.0) as usize;
        // The target's own column first, then the cell-aligned ones. Within one
        // distance (a row costs two columns, so a step a row down ties with
        // every column two either side) the nearest wins: taken in column
        // order, the leftmost did, and a body stepped down off a rule the
        // host's chips had moved onto slid a column and more sideways as it
        // went (2026-10-20, Claude Code's composer growing at a wrap).
        let cols = std::iter::once(target.1).chain(
            (first_col..end_col.min(self.coverage.cols))
                .map(|c| (self.coverage.col + c) as f32 + margin),
        );
        for row in rows {
            for col in cols.clone() {
                let rect = PetRect::new(row, col, body.rows, body.cols);
                let distance = ((row - want_row).abs() * 2.0).max((col - target.1).abs());
                let tie = (row - want_row).abs() * 2.0 + (col - target.1).abs();
                if distance <= max_distance
                    && (distance < best_distance || (distance == best_distance && tie < best_tie))
                    && if under_text {
                        self.under_text_clear(rect, margin)
                            && from.is_none_or(|from| self.ground_corridor_clear(from, rect))
                    } else {
                        self.clear(rect, margin)
                    }
                {
                    best = Some(rect);
                    best_distance = distance;
                    best_tie = tie;
                }
            }
        }
        best
    }
}

/// One row fetch and one cursor per sidecar instead of a search per cell.
///
/// The classification walk asks `RenderInput` five questions per cell, and each
/// method re-fetches the row and searches inside it. Columns ascend by one, so
/// this holds the row slices and walks each with its own cursor: amortised O(1)
/// per question, and a constant read on the usual frame where four sidecars are
/// empty and the fifth uniform.
///
/// The classification must be byte-identical to the methods'. A cursor matches
/// only while the row's list is ordered as the method's search requires, so each
/// list is checked once per row and an unordered one falls back to the method
/// itself.
///
/// The cursors answer a non-decreasing column sequence only. The speck probe's
/// neighbours (`col - 1`, `col + 1`) call `input` directly: they step backwards
/// and are reached only by an isolated braille dot.
struct RowScan<'a> {
    input: &'a RenderInput,
    row: usize,
    bg: BgRow<'a>,
    line: LineRow<'a>,
    clusters: SparseRow<'a, Box<str>>,
    combining: SparseRow<'a, Box<[char]>>,
    /// Asked through `aterm_core::render::images_in_row_at`, as
    /// `RenderInput::image_at` does: a cell may hold several image layers, and
    /// image rows are nearly always empty.
    images: &'a [(usize, ImageRef)],
}

/// The live default background along one row — `RenderInput::default_bg_at`.
enum BgRow<'a> {
    /// No spans on this row (every single-terminal frame, and every composed
    /// row the compositor left alone): one frame-wide default governs it.
    Uniform(u32),
    /// Pane spans, ascending by `end_col` as `partition_point` requires.
    Spans {
        spans: &'a [DefaultBgSpan],
        cursor: usize,
    },
    /// Spans whose `end_col` is not non-decreasing: `partition_point` is then
    /// unspecified, so reproduce it by calling it rather than approximating it.
    Unordered,
}

/// The DEC line size along one row — `RenderInput::line_size_run_at().0`.
enum LineRow<'a> {
    /// The ordinary path: no runs on this row, so `line_sizes[row]` governs it
    /// end to end.
    Uniform(LineSize),
    /// Composed runs, strictly ascending by `start_col` as `binary_search`
    /// requires to be a well-defined exact probe.
    Runs {
        spans: &'a [LineSizeSpan],
        cursor: usize,
    },
    /// Runs that are not strictly ascending; defer to the method.
    Unordered,
}

/// A sparse `(col, value)` sidecar row: clusters, combining marks.
struct SparseRow<'a, T> {
    items: &'a [(usize, T)],
    /// Strictly ascending — what `binary_search_by_key` needs to be a
    /// well-defined exact-match probe rather than an arbitrary hit.
    ordered: bool,
    cursor: usize,
}

impl<'a, T> SparseRow<'a, T> {
    fn new(items: Option<&'a Vec<(usize, T)>>) -> Self {
        let items = items.map_or([].as_slice(), Vec::as_slice);
        Self {
            ordered: items.windows(2).all(|w| w[0].0 < w[1].0),
            items,
            cursor: 0,
        }
    }

    /// The value at `col`. Exactly `binary_search_by_key(&col, ..).ok()`, for a
    /// non-decreasing sequence of `col`.
    fn get(&mut self, col: usize) -> Option<&'a T> {
        let items = self.items;
        if !self.ordered {
            return items
                .binary_search_by_key(&col, |(c, _)| *c)
                .ok()
                .map(|i| &items[i].1);
        }
        while items.get(self.cursor).is_some_and(|(c, _)| *c < col) {
            self.cursor += 1;
        }
        match items.get(self.cursor) {
            Some((c, v)) if *c == col => Some(v),
            _ => None,
        }
    }
}

impl<'a> RowScan<'a> {
    fn new(input: &'a RenderInput, row: usize) -> Self {
        let bg = match input.default_bg_spans.get(row).map(Vec::as_slice) {
            // A missing row and an empty one both resolve every column to the
            // frame-wide default, as the method does.
            None | Some([]) => BgRow::Uniform(input.default_bg),
            Some(spans) if spans.windows(2).all(|w| w[0].end_col <= w[1].end_col) => {
                BgRow::Spans { spans, cursor: 0 }
            }
            Some(_) => BgRow::Unordered,
        };
        let line = match input.line_size_spans.get(row).map(Vec::as_slice) {
            None | Some([]) => {
                LineRow::Uniform(input.line_sizes.get(row).copied().unwrap_or_default())
            }
            Some(spans) if spans.windows(2).all(|w| w[0].start_col < w[1].start_col) => {
                LineRow::Runs { spans, cursor: 0 }
            }
            Some(_) => LineRow::Unordered,
        };
        Self {
            input,
            row,
            bg,
            line,
            clusters: SparseRow::new(input.clusters.get(row)),
            combining: SparseRow::new(input.combining.get(row)),
            images: input.images.get(row).map_or(&[], Vec::as_slice),
        }
    }

    fn default_bg(&mut self, col: usize) -> u32 {
        let input = self.input;
        match &mut self.bg {
            BgRow::Uniform(bg) => *bg,
            BgRow::Spans { spans, cursor } => {
                while spans.get(*cursor).is_some_and(|s| s.end_col <= col) {
                    *cursor += 1;
                }
                // The same validity filter the method applies: a malformed span
                // yields the frame-wide default, it does not claim the column.
                spans
                    .get(*cursor)
                    .filter(|s| {
                        s.start_col < s.end_col
                            && s.end_col <= input.cols
                            && s.start_col <= col
                            && col < s.end_col
                    })
                    .map_or(input.default_bg, |s| s.default_bg)
            }
            BgRow::Unordered => input.default_bg_at(self.row, col),
        }
    }

    fn line_size(&mut self, col: usize) -> LineSize {
        let input = self.input;
        match &mut self.line {
            LineRow::Uniform(size) => *size,
            LineRow::Runs { spans, cursor } => {
                while spans.get(*cursor).is_some_and(|s| s.start_col <= col) {
                    *cursor += 1;
                }
                // `cursor` is the method's `binary_search` insertion point.
                // `Err(0)` (nothing starts at or before `col`) is unclaimed
                // composite space, rendered single-width. Otherwise the last run
                // starting at or before `col` governs it, by exact hit on its
                // `start_col` or by covering it.
                match cursor.checked_sub(1).map(|i| &spans[i]) {
                    Some(s) if s.start_col == col || col < s.end_col => s.line_size,
                    _ => LineSize::SingleWidth,
                }
            }
            LineRow::Unordered => input.line_size_run_at(self.row, col).0,
        }
    }

    fn cluster(&mut self, col: usize) -> Option<&'a str> {
        self.clusters.get(col).map(AsRef::as_ref)
    }

    fn combining(&mut self, col: usize) -> Option<&'a [char]> {
        self.combining.get(col).map(AsRef::as_ref)
    }

    fn image(&mut self, col: usize) -> Option<&'a ImageRef> {
        aterm_core::render::images_in_row_at(self.images, col)
            .last()
            .map(|(_, image)| image)
    }
}

fn is_single_braille_dot(ch: char) -> bool {
    let code = u32::from(ch);
    (0x2800..=0x28ff).contains(&code) && (code - 0x2800).count_ones() == 1
}

/// The pane-local columns `[lo, hi)` of row `row` whose cells overlap
/// `rect` (the open intersection of the cell's unit square and the rect),
/// up to `limit`, or `None` when the rect misses the row. Each column test
/// is monotone in the column, so both endpoints are found by bounded bisection
/// with the very comparisons a cell-by-cell test makes. It agrees with that
/// predicate however the rect's edges round
/// (`exclusion_spans_agree_with_the_cell_test`).
fn exclusion_span(rect: PetRect, row: usize, limit: usize) -> Option<(usize, usize)> {
    if !(rect.row < row as f32 + 1.0 && rect.row + rect.rows > row as f32) {
        return None;
    }
    let starts_before = |col: usize| rect.col < col as f32 + 1.0;
    let ends_after = |col: usize| rect.col + rect.cols > col as f32;
    // The predicates are monotone, but f32 plateaus can cover many integer
    // columns. Bisect the whole domain instead of repairing one column at a time.
    let (mut a, mut b) = (0, limit);
    while a < b {
        let mid = a + (b - a) / 2;
        if starts_before(mid) {
            b = mid;
        } else {
            a = mid + 1;
        }
    }
    let lo = a;
    let (mut a, mut b) = (0, limit);
    while a < b {
        let mid = a + (b - a) / 2;
        if ends_after(mid) {
            a = mid + 1;
        } else {
            b = mid;
        }
    }
    let hi = a;
    (lo < hi).then_some((lo, hi))
}

#[cfg(test)]
fn intersects_cell(rect: PetRect, row: usize, col: usize) -> bool {
    rect.row < row as f32 + 1.0
        && rect.row + rect.rows > row as f32
        && rect.col < col as f32 + 1.0
        && rect.col + rect.cols > col as f32
}

fn selection_target(input: &RenderInput, pane: PetPane) -> Option<(f32, f32)> {
    let project = |selection: &TextSelection, offset: i32, clip: Option<SelectionClip>| {
        let bounds = selection
            .project_range(u16::try_from(input.cols.saturating_sub(1)).unwrap_or(u16::MAX))?;
        let clip = clip.unwrap_or(SelectionClip::new(
            pane.row,
            pane.row + pane.rows,
            pane.col,
            pane.col + pane.cols,
        ));
        let first = i64::from(bounds.start_row) + i64::from(offset);
        let last = i64::from(bounds.end_row) + i64::from(offset);
        let row_start = first.max(pane.row.max(clip.row_start) as i64);
        let row_end = last.min((pane.row + pane.rows).min(clip.row_end) as i64 - 1);
        let col_start = pane.col.max(clip.col_start);
        let col_end = (pane.col + pane.cols).min(clip.col_end);
        if row_start > row_end || col_start >= col_end {
            return None;
        }
        // The active endpoint can be offscreen while selected pixels remain
        // visible. Follow its nearest visible selected cell, including an
        // interior row when the clipped first/last row has no selected column.
        // Five geometric candidates suffice; never scan unbounded scrollback.
        let end = selection.end();
        let preferred = (i64::from(end.row) + i64::from(offset)).clamp(row_start, row_end);
        for row in [preferred, row_start, row_end, row_start + 1, row_end - 1] {
            if row < row_start || row > row_end {
                continue;
            }
            let lo = if bounds.is_block || row == first {
                usize::from(bounds.start_col).max(col_start)
            } else {
                col_start
            };
            let hi = if bounds.is_block || row == last {
                usize::from(bounds.end_col).min(col_end - 1)
            } else {
                col_end - 1
            };
            if lo <= hi {
                let col = usize::from(end.col).clamp(lo, hi);
                return Some(((row as usize - pane.row) as f32, (col - pane.col) as f32));
            }
        }
        None
    };
    if input.selections.is_empty() {
        project(&input.selection, input.display_offset, input.selection_clip)
    } else {
        input
            .selections
            .iter()
            .take(MAX_REGIONS)
            .filter(|s| !s.inactive)
            .find_map(|s| project(&s.selection, 0, Some(s.clip)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aterm_core::grid::extra::{ImageData, ImageFormat, ImageRef, ImageScaling};
    use aterm_core::selection::{SelectionSide, SelectionType};
    use std::sync::Arc;

    fn snapshot(term: &mut Terminal) -> (RenderInput, PetWorldFacts) {
        // A real configured frame has an authoritative theme. Pristine
        // Terminal::new deliberately leaves renderer theme choice unresolved.
        term.process(b"\x1b]11;#000000\x07");
        let rows = usize::from(term.grid().rows());
        let cols = usize::from(term.grid().cols());
        let input = term.cell_frame(rows, cols);
        let facts = PetWorldFacts::read(term, 7);
        (input, facts)
    }

    fn observe(term: &mut Terminal) -> PetWorld {
        let (input, facts) = snapshot(term);
        let mut world = PetWorld::default();
        assert!(world.observe(&input, &facts, PetPane::full(&input)));
        world
    }

    /// CLEAN GROUND IN TWO ROWS. A 1.7-row body with a 0.1 margin fits the
    /// two blank rows between two lines of words when it is seated so its
    /// margin fills them exactly; with integral feet the same body touches
    /// three rows and the gap is refused. The search answers clear ground or
    /// nothing, the nearest (a row costs two columns), down before up on a
    /// tie, and nothing past its reach.
    ///
    /// NEGATIVE CONTROL: the same body with integral feet in that gap is not
    /// clear (the assertion below), which is the seat a three-row search
    /// would have to refuse.
    #[test]
    fn nearest_clear_perch_fits_two_blank_rows_and_never_counts_ink() {
        let words = "x".repeat(40);
        let mut term = Terminal::new(12, 40);
        term.process(format!("\x1b[3;1H{words}\x1b[6;1H{words}\x1b[1;1H").as_bytes());
        let world = observe(&mut term);
        // Feet on row 3, the body over row 2's words.
        let near = PetRect::new(2.3, 10.0, 1.7, 5.0);
        let seat = world
            .nearest_clear_perch(near, 0.1, 20.0, None)
            .expect("the gap between the two lines is clean ground");
        assert!(world.clear(seat, 0.1), "the seat is clear, margin included");
        assert!(
            (3.0..3.3).contains(&seat.row) && (seat.col - 10.0).abs() <= 0.5,
            "the two blank rows 3 and 4, at the stand's column: {seat:?}"
        );
        assert!(
            !world.clear(PetRect::new(3.3, 10.0, 1.7, 5.0), 0.1),
            "negative control: integral feet in the gap touch row 5's words"
        );
        assert_eq!(
            world.nearest_clear_perch(near, 0.1, 1.0, None),
            None,
            "no clean ground within the reach is no answer"
        );
        // A tie: rows 3..=4 above and 6..=7 below, the same distance away.
        let mut term = Terminal::new(12, 40);
        term.process(
            format!("\x1b[3;1H{words}\x1b[6;1H{words}\x1b[9;1H{words}\x1b[1;1H").as_bytes(),
        );
        let world = observe(&mut term);
        let between = PetRect::new(4.65, 10.0, 1.7, 5.0);
        let seat = world
            .nearest_clear_perch(between, 0.1, 20.0, None)
            .expect("clean ground on both sides");
        assert!(
            (6.0..6.3).contains(&seat.row),
            "down before up on a tie: {seat:?}"
        );
    }

    /// The world's exclusion test reads each row's column spans, not every
    /// exclusion per cell. On 2,000 seeded rects with fractional edges, in
    /// and past a 30-column pane, a column is in the span exactly when the
    /// cell-by-cell test puts its cell in the rect. On a blank screen the
    /// observed world protects exactly those cells.
    ///
    /// NEGATIVE CONTROL: a span one column short at either end, or a guess
    /// with no step back, disagrees with the cell test, and this test fails.
    #[test]
    fn exclusion_spans_agree_with_the_cell_test() {
        let mut seed = 0x9e37_79b9_u64;
        let mut next = move |n: u64| {
            seed = seed.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1);
            (seed >> 33) % n
        };
        let limit = 30;
        for _ in 0..2000 {
            let edge = |n: u64| n as f32 * 0.25 - 2.0;
            let rect = PetRect::new(
                edge(next(48)),
                edge(next(144)),
                0.25 + next(24) as f32 * 0.25,
                0.25 + next(48) as f32 * 0.25,
            );
            for row in 0..10 {
                let span = exclusion_span(rect, row, limit);
                for col in 0..limit {
                    let inside = span.is_some_and(|(lo, hi)| lo <= col && col < hi);
                    assert_eq!(
                        inside,
                        intersects_cell(rect, row, col),
                        "{rect:?} at ({row}, {col}): span {span:?}"
                    );
                }
            }
        }
        assert_eq!(exclusion_span(PetRect::new(0.0, 0.0, 1.0, 1.0), 0, 0), None);
        assert_eq!(
            exclusion_span(PetRect::new(0.0, 0.0, 1.0, 1.0), 1, 30),
            None
        );
        assert_eq!(
            exclusion_span(PetRect::new(0.0, 0.0, 1.0, 1.0), 0, 1),
            Some((0, 1))
        );
        // A valid finite rect on a huge public pane: adjacent integers round
        // to the same f32 for billions of columns. No huge buffer is made.
        if let Ok(base) = usize::try_from(1_u64 << 60) {
            let step = usize::try_from(1_u64 << 37).expect("same integer domain");
            let limit = base + 8 * step;
            let thin = PetRect::new(0.0, base as f32, 1.0, 1.0);
            assert_eq!(exclusion_span(thin, 0, limit), None);
            let wide = PetRect::new(0.0, base as f32, 1.0, (4 * step) as f32);
            let span = exclusion_span(wide, 0, limit).expect("a wide rect still has interior");
            assert!(span.0 <= base + step && base + step < span.1);
            for col in [
                0,
                base - step,
                base,
                base + step,
                base + 4 * step,
                limit - 1,
            ] {
                assert_eq!(span.0 <= col && col < span.1, intersects_cell(wide, 0, col));
            }
            // The real observer admits this coherent public geometry even
            // though the materialized row stays tiny. The exclusion misses it.
            let mut tiny = Terminal::new(1, 1);
            tiny.process(b"x");
            let (mut input, mut facts) = snapshot(&mut tiny);
            input.cols = limit;
            facts.stamp.surface.cols = limit;
            let mut world = PetWorld::default();
            assert!(world.observe_with_exclusions(&input, &facts, PetPane::full(&input), &[thin]));
            assert_eq!(world.examined_cells(), MAX_WORLD_COLS);
        }
        let mut term = Terminal::new(10, 30);
        term.process(b"\x1b[?25l");
        let (input, facts) = snapshot(&mut term);
        let rects = [
            PetRect::new(0.5, 2.25, 1.0, 3.5),
            PetRect::new(4.0, 10.0, 2.0, 5.0),
            PetRect::new(7.9, 27.5, 3.0, 9.0),
        ];
        let mut world = PetWorld::default();
        assert!(world.observe_with_exclusions(&input, &facts, PetPane::full(&input), &rects));
        for row in 0..10 {
            for col in 0..30 {
                let want = rects.iter().any(|rect| intersects_cell(*rect, row, col));
                assert_eq!(
                    world.cell(row, col) == PetCell::Protected,
                    want,
                    "cell ({row}, {col})"
                );
            }
        }
    }

    /// The walk skips bands, jumps past words and hulls, and bisects its
    /// reach to stay cheap on the frame path, and must still find what an
    /// exhaustive scan finds. On 300 seeded screens of words, gaps and blank
    /// rows, with the host's chrome and a transient overlay at random, from
    /// a body on glass or none, at every reach, it answers a seat an
    /// independent reading of the screen accepts — no word, no chrome, no
    /// line's hull under the body and its margin, the overlay ignored, and a
    /// box swept from the body to it free of chrome — at the exhaustive
    /// scan's distance, or nothing exactly when that scan finds nothing.
    ///
    /// NEGATIVE CONTROLS (as run, each failing this test): a jump one
    /// column too far (`c = x + 2`); a band skipped on a row with exactly
    /// the body's width of room; a hull jump one past its end; the reach's
    /// right bisection one column short; the hulls ignored; the seat read on
    /// the live plane (overlays counted).
    #[test]
    fn nearest_clear_perch_finds_what_an_exhaustive_scan_finds() {
        let mut seed = 0x2545_f491_u64;
        let mut next = move |n: u64| {
            seed = seed.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1);
            (seed >> 33) % n
        };
        let (rows, cols) = (14u16, 60u16);
        for _ in 0..300 {
            let mut term = Terminal::new(rows, cols);
            let mut screen = String::new();
            let mut text = vec![String::new(); usize::from(rows)];
            for r in 0..rows {
                if next(3) == 0 {
                    continue;
                }
                let mut line = String::new();
                let len = next(u64::from(cols) + 1);
                while (line.len() as u64) < len {
                    let word = 1 + next(6);
                    line.push_str(&"w".repeat(word as usize));
                    line.push_str(&" ".repeat(1 + next(8) as usize));
                }
                line.truncate(len as usize);
                screen.push_str(&format!("\x1b[{};1H{line}", r + 1));
                text[usize::from(r)] = line;
            }
            screen.push_str("\x1b[?25l");
            term.process(screen.as_bytes());
            let (n_chrome, n_overlays) = (next(4), next(3));
            let mut rect = || {
                PetRect::new(
                    next(u64::from(rows)) as f32,
                    next(u64::from(cols)) as f32,
                    1.0 + next(2) as f32,
                    1.0 + next(20) as f32,
                )
            };
            let chrome: Vec<PetRect> = (0..n_chrome).map(|_| rect()).collect();
            let overlays: Vec<PetRect> = (0..n_overlays).map(|_| rect()).collect();
            let (input, facts) = snapshot(&mut term);
            let mut world = PetWorld::default();
            assert!(world.observe_with_overlays(
                &input,
                &facts,
                PetPane::full(&input),
                &chrome,
                &overlays
            ));
            let near = PetRect::new(
                next(130) as f32 * 0.1,
                next(560) as f32 * 0.1,
                1.7,
                5.0 + next(8) as f32 * 0.1,
            );
            let from = (next(2) == 0).then(|| {
                PetRect::new(
                    next(124) as f32 * 0.1,
                    next(550) as f32 * 0.1,
                    1.7,
                    near.cols,
                )
            });
            // The screen as written, read without the world.
            let ink = |r: usize, c: usize| text[r].as_bytes().get(c).is_some_and(|&b| b != b' ');
            let lasting = |r: usize, c: usize| chrome.iter().any(|x| intersects_cell(*x, r, c));
            let hull = |r: usize| {
                let line = text[r].trim_end();
                (line.len() - line.trim_start().len(), line.len())
            };
            let cells = |rect: PetRect, margin: f32| {
                let (r0, c0) = (rect.row - margin, rect.col - margin);
                let (r1, c1) = (rect.row + rect.rows + margin, rect.col + rect.cols + margin);
                (r0 >= 0.0 && c0 >= 0.0 && r1 <= f32::from(rows) && c1 <= f32::from(cols)).then(
                    || {
                        (
                            r0.floor() as usize,
                            c0.floor() as usize,
                            r1.ceil() as usize,
                            c1.ceil() as usize,
                        )
                    },
                )
            };
            let from_cells = from.and_then(|f| cells(f, 0.0));
            let seat_ok = |seat: PetRect| {
                let Some((r0, c0, r1, c1)) = cells(seat, 0.1) else {
                    return false;
                };
                (r0..r1).all(|r| (c0..c1).all(|c| !ink(r, c) && !lasting(r, c)))
                    && (r0..r1).all(|r| {
                        let (lo, hi) = hull(r);
                        lo >= hi || c1 <= lo || c0 >= hi
                    })
                    && from_cells.is_none_or(|_| {
                        let (a0, b0, a1, b1) = cells(from.unwrap().union(seat), 0.0).unwrap();
                        (a0..a1).all(|r| (b0..b1).all(|c| !lasting(r, c)))
                    })
            };
            for reach in [3.0, 9.0, 64.0] {
                // Exhaustive: every seat whose body and margin fill whole
                // cells.
                let mut want: Option<f32> = None;
                for band in 0..=(rows - 2) {
                    for c in 0..=(cols - 6) {
                        let inset_c = 0.1 + (6.0 - near.cols - 0.2) * 0.5;
                        let seat = PetRect::new(
                            f32::from(band) + 0.15,
                            f32::from(c) + inset_c,
                            near.rows,
                            near.cols,
                        );
                        let d = ((seat.row - near.row) * 2.0).hypot(seat.col - near.col);
                        if d <= reach && seat_ok(seat) {
                            want = Some(want.map_or(d, |w| w.min(d)));
                        }
                    }
                }
                let got = world.nearest_clear_perch(near, 0.1, reach, from);
                match (got, want) {
                    (None, None) => {}
                    (Some(seat), Some(d)) => {
                        assert!(seat_ok(seat), "{seat:?} is rest ground");
                        assert!(
                            world.ground_clear(seat, 0.1) && world.hull_clear(seat, 0.1),
                            "and the world reads it so: {seat:?}"
                        );
                        let got_d = ((seat.row - near.row) * 2.0).hypot(seat.col - near.col);
                        assert!(
                            (got_d - d).abs() < 1e-3,
                            "from {near:?} within {reach} ({from:?}): {seat:?} at {got_d}, the scan's best is {d}"
                        );
                    }
                    (got, want) => {
                        panic!(
                            "from {near:?} within {reach} ({from:?}): got {got:?}, the scan's best is {want:?}"
                        )
                    }
                }
            }
        }
    }

    /// The lab's full Claude Code pane (120 x 16, Aterm Day): the last
    /// output line, just above home, ends at column 34, and an older line
    /// eight rows up ends at 31. The seat beside the last line is nearer.
    /// Under a maximum of the row and column costs, the old line's three
    /// columns won, and the cat slept eight rows from its caret.
    ///
    /// NEGATIVE CONTROL: the stand's own two rows are not clear, so the
    /// search has to move. Measured by the maximum, the far seat is the
    /// nearer (28.5 against 31.5), and this test fails.
    #[test]
    fn nearest_clear_perch_weighs_rows_and_columns_together() {
        let full = "x".repeat(60);
        let mut term = Terminal::new(14, 60);
        let mut screen = String::new();
        for row in [1, 2, 5, 6, 7, 8, 9, 12, 13, 14] {
            screen.push_str(&format!("\x1b[{row};1H{full}"));
        }
        screen.push_str(&format!("\x1b[3;1H{}", "x".repeat(31)));
        screen.push_str(&format!("\x1b[10;1H{}", "x".repeat(34)));
        screen.push_str("\x1b[13;50H");
        term.process(screen.as_bytes());
        let world = observe(&mut term);
        let near = PetRect::new(10.0, 3.0, 1.7, 5.0);
        assert!(!world.clear(near, 0.1), "fixture: the stand is not clean");
        let seat = world
            .nearest_clear_perch(near, 0.1, 64.0, None)
            .expect("blank ground beside both lines");
        assert!(world.clear(seat, 0.1), "the seat is clear: {seat:?}");
        assert!(
            (9.0..9.3).contains(&seat.row) && seat.col >= 34.0,
            "beside the last line, not eight rows up: {seat:?}"
        );
    }

    #[test]
    fn exact_cells_allow_a_real_gutter_and_protect_the_whole_body() {
        let mut term = Terminal::new(10, 40);
        term.process(b"\x1b[?25l\x1b[4;3Hleft\x1b[4;26Hright");
        let world = observe(&mut term);
        // A first/last-ink hull would wrongly reject this genuine clear gap.
        assert!(world.clear(PetRect::new(2.3, 11.0, 1.7, 6.0), 0.2));
        assert!(!world.clear(PetRect::new(2.3, 4.0, 1.7, 6.0), 0.2));
        // The feet row is blank; the upper half of this body covers real ink.
        assert!(!world.clear(PetRect::new(3.0, 3.0, 2.0, 5.0), 0.0));
        assert_eq!(world.cell(4, 3), PetCell::Clear);
    }

    #[test]
    fn cursor_protection_does_not_erase_or_invent_actual_ink() {
        let mut term = Terminal::new(10, 40);
        term.process(b"\x1b[3;5HA\x1b[3;5H");
        let before = observe(&mut term);
        assert_eq!(before.cell(2, 4), PetCell::Protected);
        assert_eq!(before.ink_at(2, 4), Some(true));
        assert_eq!(before.ink_at(2, 5), Some(false));
        term.process(b"\x1b[6;30H");
        let after = observe(&mut term);
        assert_eq!(after.cell(2, 4), PetCell::Ink);
        assert_eq!(after.ink_at(2, 4), before.ink_at(2, 4));
        assert_eq!(after.ink_at(2, 5), before.ink_at(2, 5));
        assert_eq!(after.ink_at(100, 100), None);
    }

    /// A bound on what the map knows is not a fact about the world: the
    /// coverage window slides with the caret, and `clear` answers `false` both
    /// for ink and for outside the window, so visibility must use `clearance`.
    #[test]
    fn outside_the_coverage_window_is_unknown_not_obstructed() {
        let mut term = Terminal::new(100, 300);
        term.process(b"\x1b[100;300H");
        let world = observe(&mut term);
        assert_eq!(world.coverage().row, 36);
        assert_eq!(world.coverage().col, 44);
        let outside = PetRect::new(0.0, 0.0, 2.0, 5.0);
        let inside = PetRect::new(50.0, 60.0, 2.0, 5.0);
        assert_eq!(world.clearance(outside, 0.0), None, "unobserved");
        assert_eq!(world.clearance(inside, 0.0), Some(true));
        assert_eq!(world.ink_clearance(outside, 0.0), None);
        assert_eq!(world.ink_clearance(inside, 0.0), Some(true));
        assert_eq!(world.ink_clearance(inside, f32::NAN), Some(false));
        // `clear` stays fail-closed for both, which is right for PLACEMENT.
        assert!(!world.clear(outside, 0.0));
        assert!(world.clear(inside, 0.0));
        // A retired map knows nothing anywhere.
        let mut retired = PetWorld::default();
        assert_eq!(retired.clearance(inside, 0.0), None);
        assert_eq!(retired.clearance_past_caret(inside, 0.0), None);
        retired.retire();
        assert_eq!(retired.clearance(inside, 0.0), None);
        // Degenerate geometry is a REFUSAL, not an unknown: it must never be
        // spent as "keep the previous verdict".
        assert_eq!(
            world.clearance(
                PetRect {
                    row: f32::NAN,
                    ..inside
                },
                0.0
            ),
            Some(false)
        );
        assert_eq!(world.clearance(inside, f32::INFINITY), Some(false));
    }

    /// What the map sees of a body straddling its coverage window still
    /// judges it: unknown cells are not evidence, a protected cell the map
    /// covers is (`seen_under_text_clearance_past_caret`).
    #[test]
    fn a_protected_cell_the_map_sees_vetoes_a_body_half_outside_it() {
        let mut term = Terminal::new(100, 300);
        // A double-width row on pane row 36, the window's first.
        term.process(b"\x1b[37;1H\x1b#6\x1b[100;300H");
        let world = observe(&mut term);
        assert_eq!(world.coverage().row, 36);
        let half = PetRect::new(35.0, 60.0, 2.0, 5.0);
        assert_eq!(world.under_text_clearance_past_caret(half, 0.0), None);
        assert_eq!(
            world.seen_under_text_clearance_past_caret(half),
            Some(false),
            "the seen half stands on a protected row"
        );
        let inside = PetRect::new(97.0, 60.0, 2.0, 5.0);
        assert_eq!(
            world.seen_under_text_clearance_past_caret(inside),
            Some(true),
            "fixture: inside the window and clear"
        );
        let mut plain = Terminal::new(100, 300);
        plain.process(b"\x1b[100;300H");
        let plain = observe(&mut plain);
        assert_eq!(
            plain.seen_under_text_clearance_past_caret(half),
            None,
            "the seen half is clear and the rest is unseen"
        );
        assert_eq!(
            plain.seen_under_text_clearance_past_caret(PetRect::new(0.0, 0.0, 2.0, 5.0)),
            None,
            "wholly unseen"
        );
        assert_eq!(
            plain.seen_under_text_clearance_past_caret(PetRect::new(f32::NAN, 0.0, 2.0, 5.0)),
            Some(false),
            "degenerate is a refusal"
        );
        assert_eq!(
            PetWorld::default().seen_under_text_clearance_past_caret(inside),
            None,
            "a map that saw nothing"
        );
    }

    /// The caret protects its own cells from placement but not visibility, and
    /// forgives exactly those: never a glyph, a selection or a neighbour.
    #[test]
    fn the_caret_is_not_an_obstruction_to_its_own_escort() {
        let mut term = Terminal::new(10, 40);
        term.process(b"\x1b[3;11H");
        let world = observe(&mut term);
        let on_caret = PetRect::new(2.0, 10.0, 1.0, 1.0);
        assert_eq!(world.cell(2, 10), PetCell::Protected);
        assert_eq!(world.clearance(on_caret, 0.0), Some(false));
        assert_eq!(world.clearance_past_caret(on_caret, 0.0), Some(true));
        // The whole 3x3 ring (nine blank cells): still protected, so placement
        // keeps its distance; still forgiven, so a sprite beside the caret is
        // not hidden for standing in its keep-off margin.
        let ring = PetRect::new(1.0, 9.0, 3.0, 3.0);
        for (r, c) in [(1, 10), (3, 10), (2, 9), (2, 11), (1, 9), (3, 11)] {
            assert_eq!(world.cell(r, c), PetCell::Protected, "ring {r},{c}");
        }
        assert_eq!(world.cell(2, 13), PetCell::Clear, "outside the ring");
        assert_eq!(world.clearance(ring, 0.0), Some(false));
        assert_eq!(world.clearance_past_caret(ring, 0.0), Some(true));
        // And it is PROTECTION, not ink: nothing there to draw behind.
        assert_eq!(world.ink_clearance(ring, 0.0), Some(true));
        // One cell past the ring on every side is still forgiven, because
        // those cells were never blocked in the first place.
        assert_eq!(
            world.clearance_past_caret(PetRect::new(0.0, 8.0, 5.0, 5.0), 0.0),
            Some(true)
        );
        // A glyph UNDER the caret is still a glyph.
        term.process(b"\x1b[5;21HX\x1b[5;21H");
        let world = observe(&mut term);
        let on_glyph = PetRect::new(4.0, 20.0, 1.0, 1.0);
        assert_eq!(world.clearance(on_glyph, 0.0), Some(false));
        assert_eq!(world.clearance_past_caret(on_glyph, 0.0), Some(false));
        assert_eq!(world.ink_clearance(on_glyph, 0.0), Some(false));
        // And a wider body that also covers real ink is still obstructed.
        term.process(b"\x1b[7;5Hword\x1b[7;12H");
        let world = observe(&mut term);
        assert_eq!(
            world.clearance_past_caret(PetRect::new(6.0, 4.0, 1.0, 9.0), 0.0),
            Some(false)
        );
        assert_eq!(
            world.clearance_past_caret(PetRect::new(6.0, 9.0, 1.0, 4.0), 0.0),
            Some(true)
        );
    }

    #[test]
    fn ordinary_ink_and_protected_pixels_have_distinct_placement_planes() {
        let mut term = Terminal::new(12, 40);
        term.process(b"\x1b[?25l\x1b[3;11H\x1b[41m \x1b[0m\x1b[4;11H\x1b[4m \x1b[0m");
        term.text_selection_mut().start_selection(
            5,
            10,
            SelectionSide::Left,
            SelectionType::Simple,
        );
        term.text_selection_mut()
            .update_selection(5, 12, SelectionSide::Right);
        let (mut input, facts) = snapshot(&mut term);
        input.images.resize_with(input.rows, Vec::new);
        input.images[7] = vec![(
            10,
            ImageRef {
                image: Arc::new(ImageData {
                    bytes: Vec::new(),
                    format: ImageFormat::Png,
                    cols: 1,
                    rows: 1,
                    z_index: -1,
                    band_lift_px: 0,
                    cell_offset_px: (0, 0),
                    scaling: ImageScaling::Fit,
                    source_rect: None,
                }),
                cell_row: 0,
                cell_col: 0,
                kitty: None,
            },
        )];
        let mut world = PetWorld::default();
        assert!(world.observe(&input, &facts, PetPane::full(&input)));
        for row in [2, 3, 5, 7] {
            assert!(
                !world.clear(PetRect::new(row as f32, 10.0, 1.0, 1.0), 0.0),
                "row {row}"
            );
        }
        assert!(world.clear(PetRect::new(9.0, 10.0, 1.0, 1.0), 0.0));
        assert_eq!(world.selection_target(), Some((5.0, 12.0)));
        for row in [2, 3] {
            assert!(
                world.under_text_clear(PetRect::new(row as f32, 10.0, 1.0, 1.0), 0.0),
                "colored backgrounds and decorated text permit an under-text body"
            );
        }
        for row in [5, 7] {
            assert!(
                !world.under_text_clear(PetRect::new(row as f32, 10.0, 1.0, 1.0), 0.0),
                "selection and images remain protected in both planes"
            );
        }
        // The z-order plane: what is PAINTED there, not what is protected.
        for (row, inked) in [(2, true), (3, true), (5, true), (7, true), (9, false)] {
            assert_eq!(
                world.ink_clearance(PetRect::new(row as f32, 10.0, 1.0, 1.0), 0.0),
                Some(!inked),
                "row {row}"
            );
        }
        // A host overlay spliced in after the observation (the find bar) is
        // pixels the map cannot see: drawn under, like any other ink.
        let blank = PetRect::new(9.0, 10.0, 1.0, 1.0);
        assert!(world.observe_with_exclusions(&input, &facts, PetPane::full(&input), &[blank]));
        assert_eq!(world.ink_clearance(blank, 0.0), Some(false));
    }

    /// The person's typed glyphs the host paints ahead of the echo (the
    /// predicted echo, the IME preedit) are ordinary ink in every plane but
    /// the glyph plane: a resident may stand under them and is drawn beneath
    /// them, nothing steps aside for them, and the strict placement reading
    /// still refuses them. Inside the visible caret's ring the caret protects
    /// the cell as it would a real glyph: forgiven to the escort, not blank.
    ///
    /// Negative control: the same rectangles reserved as a transient overlay
    /// (what the native host did until 2026-10-20) veto the under-text reading
    /// the pet's step-aside asks, which is what threw it off its seat on every
    /// key.
    #[test]
    fn typed_ink_reserve_is_ink_not_a_protected_surface() {
        let mut term = Terminal::new(12, 40);
        // A visible caret at (5,10); the guesses: its own cell and the next
        // (in the ring), and one out on its own.
        term.process(b"\x1b[6;11H");
        let (input, facts) = snapshot(&mut term);
        let guess = PetRect::new(5.0, 10.0, 1.0, 2.0);
        let far = PetRect::new(9.0, 20.0, 1.0, 1.0);
        let ring = PetRect::new(5.0, 11.0, 1.0, 1.0);
        let typed = [guess, far];
        let mut world = PetWorld::default();
        assert!(world.observe_with_reserve(
            &input,
            &facts,
            PetPane::full(&input),
            PetReserve {
                protected: &[],
                overlays: &[],
                ink: &typed,
            }
        ));
        assert_eq!(world.cell(9, 20), PetCell::Ink);
        assert_eq!(
            world.ink_at(9, 20),
            Some(false),
            "the glyph plane is the grid's"
        );
        assert_eq!(world.ink_clearance(far, 0.0), Some(false), "drawn under");
        assert_eq!(world.under_text_clearance(far, 0.0), Some(true));
        assert_eq!(world.under_text_clearance_past_caret(far, 0.0), Some(true));
        assert_eq!(world.clearance(far, 0.0), Some(false));
        assert_eq!(world.clearance_past_caret(far, 0.0), Some(false));
        assert_eq!(world.cell(5, 11), PetCell::Protected, "the caret's ring");
        assert_eq!(world.under_text_clearance_past_caret(ring, 0.0), Some(true));
        assert_eq!(
            world.clearance_past_caret(ring, 0.0),
            Some(false),
            "a typed ring cell is not blank"
        );
        assert_eq!(world.ink_clearance(ring, 0.0), Some(false));

        assert!(world.observe_with_reserve(
            &input,
            &facts,
            PetPane::full(&input),
            PetReserve {
                protected: &[],
                overlays: &typed,
                ink: &[],
            }
        ));
        assert_eq!(world.cell(9, 20), PetCell::Protected);
        assert_eq!(world.under_text_clearance_past_caret(far, 0.0), Some(false));
        assert_eq!(
            world.under_text_clearance_past_caret(ring, 0.0),
            Some(false)
        );

        // The typed list is fenced like the protected one.
        let nan = [PetRect::new(f32::NAN, 0.0, 1.0, 1.0)];
        assert!(!world.observe_with_reserve(
            &input,
            &facts,
            PetPane::full(&input),
            PetReserve {
                protected: &[],
                overlays: &[],
                ink: &nan,
            }
        ));
        assert_eq!(world.stamp(), None);
    }

    #[test]
    fn reused_observations_match_fresh_maps_after_coverage_changes() {
        let mut reused = PetWorld::default();
        // Seed the whole backing buffer with ink, shrink to blank, then move
        // the bounded window across partially/wholly missing rows. A present
        // sparse row is blank; an absent row must forget the previous ink.
        // A refused observation must also hide every retained cell.
        for (rows, cols, caret, present_rows, ink, coherent) in [
            (64, 256, (0, 0), 64, true, true),
            (8, 20, (0, 0), 8, false, true),
            (100, 300, (99, 299), 50, true, true),
            (100, 300, (0, 0), 0, false, true),
            (57, 151, (0, 0), 57, false, true),
            (12, 40, (0, 0), 12, true, false),
            (64, 256, (63, 255), 64, false, true),
        ] {
            let mut term = Terminal::new(rows, cols);
            if ink {
                term.process(&vec![b'x'; usize::from(rows) * usize::from(cols)]);
            }
            term.process(format!("\x1b[{};{}H", caret.0 + 1, caret.1 + 1).as_bytes());
            let (mut input, mut facts) = snapshot(&mut term);
            if !ink {
                for row in &mut input.cells {
                    row.clear();
                }
            }
            input.cells.truncate(present_rows);
            if !coherent {
                facts.stamp.content_seq = facts.stamp.content_seq.wrapping_add(1);
            }
            let pane = PetPane::full(&input);
            let mut fresh = PetWorld::default();
            assert_eq!(reused.observe(&input, &facts, pane), coherent);
            assert_eq!(fresh.observe(&input, &facts, pane), coherent);
            assert_eq!(reused.stamp(), fresh.stamp());
            assert_eq!(reused.coverage(), fresh.coverage());
            assert_eq!(reused.examined_cells(), fresh.examined_cells());
            let anchor_state = |world: &PetWorld| {
                world.anchors().map(|anchor| {
                    anchor.map(|a| {
                        (
                            a.surface,
                            a.block_id,
                            a.absolute_row,
                            a.row,
                            a.col,
                            a.state,
                            a.exit_code,
                        )
                    })
                })
            };
            assert_eq!(anchor_state(&reused), anchor_state(&fresh));
            // Include the row/column immediately outside the pane as well as
            // cells outside a sliding coverage window. Those must stay unknown
            // even when their old backing slots still hold clear or ink cells.
            for row in 0..=pane.rows {
                for col in 0..=pane.cols {
                    assert_eq!(
                        (
                            reused.cell(row, col),
                            reused.ink_at(row, col),
                            reused.locomotion_ink_at(row, col),
                        ),
                        (
                            fresh.cell(row, col),
                            fresh.ink_at(row, col),
                            fresh.locomotion_ink_at(row, col),
                        ),
                        "{rows}x{cols}, present_rows={present_rows}, at ({row},{col})"
                    );
                    // Both prefix planes and caret forgiveness must agree for
                    // individual cells and a body crossing several rows/columns.
                    for (height, width) in [(1.0, 1.0), (3.0, 5.0)] {
                        let rect = PetRect::new(row as f32, col as f32, height, width);
                        assert_eq!(
                            (
                                reused.clearance(rect, 0.0),
                                reused.under_text_clearance(rect, 0.0),
                                reused.clearance_past_caret(rect, 0.0),
                                reused.under_text_clearance_past_caret(rect, 0.0),
                                reused.ink_clearance(rect, 0.0),
                            ),
                            (
                                fresh.clearance(rect, 0.0),
                                fresh.under_text_clearance(rect, 0.0),
                                fresh.clearance_past_caret(rect, 0.0),
                                fresh.under_text_clearance_past_caret(rect, 0.0),
                                fresh.ink_clearance(rect, 0.0),
                            ),
                            "{rows}x{cols}, present_rows={present_rows}, rect={rect:?}"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn wide_continuations_combining_and_expanded_lines_are_not_blank() {
        let mut term = Terminal::new(10, 40);
        term.process("\x1b[?25l\x1b[3;11H界\x1b[4;11H \u{301}\x1b[6;1H\x1b#6X".as_bytes());
        let world = observe(&mut term);
        assert_ne!(world.cell(2, 11), PetCell::Clear);
        assert_ne!(world.cell(3, 10), PetCell::Clear);
        assert_ne!(
            world.cell(5, 30),
            PetCell::Clear,
            "expanded row has no certified geometry"
        );
        assert_eq!(world.cell(7, 30), PetCell::Clear);
    }

    #[test]
    fn budget_overflow_and_missing_rows_are_unknown_not_empty() {
        let mut term = Terminal::new(100, 300);
        term.process(b"\x1b[100;300H");
        let (mut input, facts) = snapshot(&mut term);
        let mut world = PetWorld::default();
        assert!(world.observe(&input, &facts, PetPane::full(&input)));
        assert!(world.examined_cells() <= MAX_WORLD_CELLS);
        assert_eq!(world.coverage().row, 36);
        assert_eq!(world.coverage().col, 44);
        assert_eq!(world.cell(0, 0), PetCell::Unknown);
        assert!(!world.clear(PetRect::new(0.0, 0.0, 2.0, 5.0), 0.0));
        assert!(world.clear(PetRect::new(50.0, 60.0, 2.0, 5.0), 0.0));
        assert_eq!(
            world.ink_clearance(PetRect::new(50.0, 60.0, 2.0, 5.0), 0.0),
            Some(true)
        );
        input.cells.truncate(50);
        assert!(world.observe(&input, &facts, PetPane::full(&input)));
        assert_eq!(world.cell(50, 60), PetCell::Unknown);
        assert!(!world.clear(PetRect::new(50.0, 60.0, 2.0, 5.0), 0.0));
        // An unobserved row is drawn under, the safe side for layering.
        assert_eq!(
            world.ink_clearance(PetRect::new(50.0, 60.0, 2.0, 5.0), 0.0),
            Some(false)
        );
    }

    #[test]
    fn stale_frame_and_nonfinite_geometry_fail_closed() {
        let mut term = Terminal::new(10, 40);
        term.process(b"\x1b[?25l");
        let (input, old_facts) = snapshot(&mut term);
        let mut world = PetWorld::default();
        assert!(world.observe(&input, &old_facts, PetPane::full(&input)));
        let clear = PetRect::new(3.0, 10.0, 2.0, 6.0);
        assert!(world.clear(clear, 0.2));
        assert!(!world.clear(
            PetRect {
                row: f32::NAN,
                ..clear
            },
            0.2
        ));
        assert!(!world.clear(clear, f32::INFINITY));
        term.process(b"\x1b[4;11Hnew text");
        let new_facts = PetWorldFacts::read(&term, 7);
        assert!(!world.observe(&input, &new_facts, PetPane::full(&input)));
        assert!(!world.clear(clear, 0.0));
        assert!(world.stamp().is_none());
    }

    #[test]
    fn selected_history_centers_the_budget_without_a_fake_caret() {
        let mut term = Terminal::new(100, 300);
        for _ in 0..110 {
            term.process(b"line\r\n");
        }
        term.scroll_display(10);
        term.text_selection_mut().start_selection(
            -4,
            20,
            SelectionSide::Left,
            SelectionType::Simple,
        );
        term.text_selection_mut()
            .update_selection(-2, 25, SelectionSide::Right);
        let (input, facts) = snapshot(&mut term);
        assert_eq!(input.display_offset, 10);
        let mut world = PetWorld::default();
        assert!(world.observe(&input, &facts, PetPane::full(&input)));
        assert_eq!(world.selection_target(), Some((8.0, 25.0)));
        assert_eq!(world.coverage().row, 0);
        assert!(world.selection_rect().is_some());
        assert_eq!(world.cell(8, 24), PetCell::Protected);
        assert_ne!(
            facts.stamp.top_absolute_row,
            term.grid().visible_to_absolute(0)
        );
    }

    #[test]
    fn visible_history_selection_survives_an_offscreen_active_endpoint() {
        let mut term = Terminal::new(100, 80);
        for _ in 0..140 {
            term.process(b"row\r\n");
        }
        term.scroll_display(20);
        for (start, end, wanted_row) in [(-10, 90, 99.0), (90, -10, 10.0)] {
            term.text_selection_mut().start_selection(
                start,
                2,
                SelectionSide::Left,
                SelectionType::Simple,
            );
            term.text_selection_mut()
                .update_selection(end, 7, SelectionSide::Right);
            let world = observe(&mut term);
            let (row, col) = world
                .selection_target()
                .expect("visible selected intersection");
            assert_eq!(row, wanted_row);
            assert_eq!(world.cell(row as usize, col as usize), PetCell::Protected);
            assert_eq!(world.examined_cells(), MAX_WORLD_ROWS * 80);
            assert!(row as usize >= world.coverage().row);
            assert!((row as usize) < world.coverage().row + world.coverage().rows);
        }
        // Late host clipping limits both the target and scanned highlight.
        let (mut input, facts) = snapshot(&mut term);
        input.selection_clip = Some(SelectionClip::new(20, 40, 20, 30));
        let mut world = PetWorld::default();
        assert!(world.observe(&input, &facts, PetPane::full(&input)));
        assert_eq!(world.selection_target(), Some((20.0, 20.0)));
        assert!(!world.clear(PetRect::new(20.0, 20.0, 1.0, 1.0), 0.0));
        term.text_selection_mut().start_selection(
            -80,
            2,
            SelectionSide::Left,
            SelectionType::Simple,
        );
        term.text_selection_mut()
            .update_selection(-60, 7, SelectionSide::Right);
        let world = observe(&mut term);
        assert_eq!(
            world.selection_target(),
            None,
            "entirely offscreen is no interest"
        );
        assert_eq!(world.selection_rect(), None);
    }

    #[test]
    fn real_blocks_resolve_through_scroll_but_not_a_new_surface() {
        let mut term = Terminal::new(12, 50);
        term.process(b"\x1b[?25l\x1b]133;A\x07$ \x1b]133;B\x07build\r\n\x1b]133;C\x07result\r\n\x1b]133;D;0\x07");
        let world = observe(&mut term);
        let anchor = *world
            .anchors()
            .iter()
            .flatten()
            .next()
            .expect("actual OSC block");
        assert_eq!(anchor.exit_code, Some(0));
        // Move the whole content plane by one row, keeping this output visible.
        term.process(b"\x1b[12;1H\n");
        let world = observe(&mut term);
        let resolved = world.resolve(anchor).expect("same retained absolute row");
        assert_eq!(resolved.row, anchor.row - 1.0);
        assert_eq!(resolved.absolute_row, anchor.absolute_row);
        assert_eq!(
            resolved.surface.absolute_row_revision, anchor.surface.absolute_row_revision,
            "ordinary full-screen scroll is not an absolute-row splice"
        );
        let (input, mut facts) = snapshot(&mut term);
        facts.stamp.surface.history_renumber_epoch += 1;
        let mut changed = PetWorld::default();
        assert!(changed.observe(&input, &facts, PetPane::full(&input)));
        assert!(changed.resolve(anchor).is_none());
        term.process(b"\x1b[2;10r\x1b[10;1H\n");
        assert!(
            observe(&mut term).resolve(anchor).is_none(),
            "a genuine interior-region scroll cannot carry retained anchors"
        );
        term.process(b"\x1b[?1049h");
        let alt = observe(&mut term);
        assert!(alt.anchors().iter().all(Option::is_none));
        assert!(alt.resolve(anchor).is_none());
    }

    /// A FULL CLEAR WHILE A COMMAND RUNS (`clear; make` from a prompt low on
    /// the screen): the clear blanked every row of the running block, which
    /// moves them to the cleared screen's top row (`aterm-core`'s
    /// `note_full_clear`), so the cat's anchor is the last glyph the command
    /// wrote after the clear, rows above where its output had started; an
    /// anchor never starts above a block's output row, so the old rows could
    /// not name it. After its D and the next prompt at the screen's top, no
    /// block the facts carry runs backwards and the finished block anchors on
    /// its own last line. CONTROL: before the clear the anchor is the output
    /// line it had.
    #[test]
    fn a_full_clear_moves_the_running_blocks_anchor_onto_what_followed_it() {
        let mut term = Terminal::new(12, 50);
        term.process(
            b"\x1b[?25l\x1b[9;1H\x1b]133;A\x07$ \x1b]133;B\x07make\r\n\x1b]133;C\x07compiling\r\n",
        );
        let running = |world: &PetWorld| {
            world
                .anchors()
                .iter()
                .flatten()
                .find(|a| a.state == BlockState::Executing)
                .map(|a| (a.block_id, a.absolute_row, a.col))
        };
        let id = term.current_block().expect("running").id;
        let output_row = term
            .current_block()
            .and_then(|b| b.output_start_row)
            .expect("output started");
        assert_eq!(
            running(&observe(&mut term)),
            Some((id, output_row, 9.0)),
            "CONTROL: the output line it had"
        );
        term.process(b"\x1b[H\x1b[2Jafter-1\r\nafter-2");
        let (block, row, col) = running(&observe(&mut term)).expect("still running");
        assert_eq!((block, row, col), (id, 1, 7.0));
        assert!(row < output_row, "above where its output had started");
        term.process(b"\r\n\x1b]133;D;0\x07\x1b]133;A\x07$ \x1b]133;B\x07");
        let (_, facts) = snapshot(&mut term);
        for block in facts.blocks.iter().flatten() {
            let output = block.output_row.unwrap_or(block.start_row);
            assert!(
                block.start_row <= output && output <= block.end_row,
                "a forward range: {block:?}"
            );
        }
        let finished = observe(&mut term)
            .anchors()
            .iter()
            .flatten()
            .find(|a| a.block_id == id)
            .map(|a| (a.state, a.exit_code, a.absolute_row, a.col));
        assert_eq!(finished, Some((BlockState::Complete, Some(0), 1, 7.0)));
    }

    #[test]
    fn explicit_progress_is_sampled_without_claiming_completion() {
        let mut term = Terminal::new(10, 40);
        term.process(b"printed 100% error\x1b]9;4;1;100\x07");
        let world = observe(&mut term);
        assert_eq!(world.progress(), Some(TaskbarProgress::Normal(100)));
        assert!(world.anchors().iter().all(Option::is_none));
        assert_eq!(PetWorldFacts::read(&term, 7).completed_seq, 0);
        term.process(b"\x1b]9;4;4;70\x07");
        assert_eq!(
            observe(&mut term).progress(),
            Some(TaskbarProgress::Paused(70))
        );
    }

    /// A STEP OFF PROTECTED GROUND KEEPS ITS COLUMN WHERE IT CAN. Chips on a
    /// rule come down onto a body's head; a row down (which costs two
    /// columns) ties with every column two either side of the body, and the
    /// search took the leftmost of them: the body went down a row and slid
    /// 1.3 columns left in the same present. Within one distance the nearest
    /// wins, and the body's own column is a candidate: one row down, not a
    /// column over. (Before 2026-10-20 this answered column 26.1.)
    #[test]
    fn a_step_off_protected_ground_keeps_the_nearest_column() {
        let mut term = Terminal::new(12, 60);
        let (input, facts) = snapshot(&mut term);
        let mut world = PetWorld::default();
        let chips = [PetRect::new(5.0, 30.0, 1.0, 29.0)];
        assert!(world.observe_with_exclusions(&input, &facts, PetPane::full(&input), &chips));
        let body = PetRect::new(5.15, 27.4, 1.7, 5.2);
        assert!(
            !world.under_text_clear(body, 0.0),
            "fixture: the chips are on the body"
        );
        let perch = world
            .nearest_under_text_perch(body, 0.1, 6.0)
            .expect("a perch in reach");
        assert!(world.under_text_clear(perch, 0.1));
        assert!(
            (perch.row - 6.15).abs() < 1e-4 && perch.col == body.col,
            "one row down in its own column, not {perch:?}"
        );
    }

    #[test]
    fn perch_search_finds_a_right_pocket_and_rejects_dense_screen() {
        let mut term = Terminal::new(12, 50);
        term.process(b"\x1b[?25l\x1b[5;1Houtput block edge");
        let world = observe(&mut term);
        let body = PetRect::new(3.3, 25.0, 1.7, 6.0);
        let perch = world
            .perch_near((4.0, 17.0), body, 0.2, 20.0)
            .expect("right pocket");
        assert!(world.clear(perch, 0.2));
        assert!(perch.col >= 17.0);
        term.process(b"\x1b[H");
        term.process(&vec![b'X'; 12 * 50]);
        let world = observe(&mut term);
        assert!(world.nearest_perch(body, 0.2, 50.0).is_none());
    }

    /// Sidecars shaped to hit every branch the row cursors must reproduce:
    /// absent rows, empty rows, ordered multi-pane spans, malformed spans, run
    /// gaps, a zero-width run, and unordered rows (where only the method's own
    /// search answers).
    fn adversarial_sidecars(input: &mut RenderInput) {
        let cols = input.cols;
        input.default_bg_spans = vec![
            // A row of spans is EMPTY: the frame-wide default governs it.
            Vec::new(),
            // Two panes abutting at column 20, as a compositor emits; separates
            // "first span ending after this column" from "not ending before it".
            vec![
                DefaultBgSpan::new(0, 20, 0x0011_2233),
                DefaultBgSpan::new(20, cols, 0x0044_5566),
            ],
            // Two panes with an unclaimed divider column between them.
            vec![
                DefaultBgSpan::new(0, 19, 0x0077_8899),
                DefaultBgSpan::new(20, cols, 0x00AA_BBCC),
            ],
            // Ordered by `end_col`, yet individually malformed: empty, inverted,
            // and past the frame's own width. Each must fall back, not claim.
            vec![
                DefaultBgSpan::new(0, 0, 0x00AA_AAAA),
                DefaultBgSpan::new(3, 2, 0x00BB_BBBB),
                DefaultBgSpan::new(10, 20, 0x00CC_CCCC),
                DefaultBgSpan::new(30, cols + 60, 0x00DD_DDDD),
            ],
            // NOT ascending by `end_col`: `partition_point` is unspecified here,
            // so the cursor must not try to be cleverer than the method.
            vec![
                DefaultBgSpan::new(20, cols, 0x0011_1111),
                DefaultBgSpan::new(0, 10, 0x0022_2222),
            ],
        ];
        input.line_sizes = vec![
            LineSize::SingleWidth,
            LineSize::DoubleWidth,
            LineSize::SingleWidth,
            LineSize::DoubleHeightTop,
            LineSize::DoubleHeightBottom,
        ];
        input.line_size_spans = vec![
            // Empty: `line_sizes[0]` governs the whole row.
            Vec::new(),
            vec![
                LineSizeSpan::new(0, 20, LineSize::DoubleWidth),
                LineSizeSpan::new(20, cols, LineSize::SingleWidth),
            ],
            // Gaps before, between and after the runs: unclaimed composite
            // space, which is single-width whatever `line_sizes[2]` says.
            vec![
                LineSizeSpan::new(5, 10, LineSize::DoubleHeightTop),
                LineSizeSpan::new(20, 25, LineSize::DoubleWidth),
            ],
            // A zero-width run: `binary_search` still finds column 10 by its
            // `start_col`, so the run governs a column it does not cover.
            vec![LineSizeSpan::new(10, 10, LineSize::DoubleWidth)],
            // Two runs sharing a `start_col`, and two in descending order: not
            // sorted, so the cursor must reproduce whatever `binary_search`
            // answers.
            vec![
                LineSizeSpan::new(0, 10, LineSize::DoubleWidth),
                LineSizeSpan::new(0, 20, LineSize::DoubleHeightBottom),
            ],
            vec![
                LineSizeSpan::new(20, 30, LineSize::DoubleWidth),
                LineSizeSpan::new(0, 10, LineSize::DoubleHeightBottom),
            ],
        ];
        input.clusters = vec![
            Vec::new(),
            vec![(3, "a".into()), (4, "bb".into()), (cols - 1, "tail".into())],
            // A duplicated column is not strictly ascending.
            vec![(5, "x".into()), (5, "y".into())],
            // Descending.
            vec![(9, "z".into()), (2, "w".into())],
        ];
        input.combining = vec![
            vec![(0, vec!['\u{301}'].into()), (7, vec!['\u{308}'].into())],
            Vec::new(),
            vec![(11, vec!['\u{327}'].into()), (11, vec!['\u{300}'].into())],
        ];
        let image = |col: usize| {
            (
                col,
                ImageRef {
                    image: Arc::new(ImageData {
                        bytes: Vec::new(),
                        format: ImageFormat::Png,
                        cols: 1,
                        rows: 1,
                        z_index: 0,
                        band_lift_px: 0,
                        cell_offset_px: (0, 0),
                        scaling: ImageScaling::Fit,
                        source_rect: None,
                    }),
                    cell_row: 0,
                    cell_col: u16::try_from(col).expect("test column"),
                    kitty: None,
                },
            )
        };
        input.images = vec![
            Vec::new(),
            vec![image(0), image(1), image(30)],
            vec![image(30), image(1)],
        ];
    }

    /// `RowScan` answers exactly what the per-cell `RenderInput` lookups answer,
    /// at every column of every row, including rows past each sidecar's end.
    #[test]
    fn the_row_cursors_answer_exactly_what_the_per_cell_lookups_do() {
        let mut term = Terminal::new(12, 40);
        term.process(b"\x1b[?25l");
        let (mut input, _) = snapshot(&mut term);
        adversarial_sidecars(&mut input);
        for row in 0..input.rows + 2 {
            let mut scan = RowScan::new(&input, row);
            // Ascending columns (the cursors' contract), two past the frame
            // width, which the walk's neighbour arithmetic can reach.
            for col in 0..input.cols + 2 {
                let at = (row, col);
                assert_eq!(
                    scan.default_bg(col),
                    input.default_bg_at(row, col),
                    "default_bg at {at:?}"
                );
                assert_eq!(
                    scan.line_size(col),
                    input.line_size_run_at(row, col).0,
                    "line size at {at:?}"
                );
                assert_eq!(
                    scan.cluster(col),
                    input.cluster_at(row, col),
                    "cluster at {at:?}"
                );
                assert_eq!(
                    scan.combining(col),
                    input.combining_at(row, col),
                    "combining at {at:?}"
                );
                assert_eq!(
                    scan.image(col).map(std::ptr::from_ref),
                    input.image_at(row, col).map(std::ptr::from_ref),
                    "image at {at:?}"
                );
            }
        }
    }

    /// The same equivalence through `observe`: a pane-local default background
    /// decides Ink from Clear column by column, so a cursor one column off
    /// changes what the pet may stand on. Reads both sides of the seam at 20.
    #[test]
    fn a_pane_local_default_background_decides_ink_column_by_column() {
        let mut term = Terminal::new(10, 40);
        term.process(b"\x1b[?25l");
        let (mut input, facts) = snapshot(&mut term);
        let pane_bg = 0x0012_3456;
        input.default_bg_spans = vec![Vec::new(); input.rows];
        input.default_bg_spans[5] = vec![
            DefaultBgSpan::new(0, 20, 0x00FF_FFFF),
            DefaultBgSpan::new(20, input.cols, pane_bg),
        ];
        input.cells[5] = vec![
            RenderCell {
                bg: [0x12, 0x34, 0x56],
                ..RenderCell::default()
            };
            input.cols
        ];
        let mut world = PetWorld::default();
        assert!(world.observe(&input, &facts, PetPane::full(&input)));
        // Left of the seam a different pane's default rules, so this pane's
        // background reads there as painted ink.
        assert_eq!(world.cell(5, 0), PetCell::Ink);
        assert_eq!(world.cell(5, 19), PetCell::Ink);
        // From the seam on it is that pane's EMPTY.
        assert_eq!(world.cell(5, 20), PetCell::Clear);
        assert_eq!(world.cell(5, 39), PetCell::Clear);
    }

    /// The real walk does not query every accessor at every column:
    /// `cluster`/`combining` short-circuit on `||` and `image` is skipped when
    /// `selected || excluded` holds, so a cursor can be asked for column 40
    /// having last seen column 3.
    #[test]
    fn review_row_cursors_survive_skipped_and_sparse_query_orders() {
        let mut seed = 0x2545_F491_4F6C_DD1Du64;
        let mut rnd = move || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed
        };
        for trial in 0..400u32 {
            let mut term = Terminal::new(8, 24);
            term.process(b"\x1b[?25l");
            let (mut input, _) = snapshot(&mut term);
            let cols = input.cols;
            let mut bg_rows = Vec::new();
            let mut ls_rows = Vec::new();
            let mut cl_rows = Vec::new();
            let mut cb_rows = Vec::new();
            let mut im_rows = Vec::new();
            for _ in 0..input.rows {
                let n = (rnd() % 5) as usize;
                let mut bg = Vec::new();
                let mut cur = 0usize;
                for _ in 0..n {
                    let start = cur + (rnd() % 3) as usize;
                    let end = start + (rnd() % 8) as usize;
                    bg.push(DefaultBgSpan::new(
                        start.min(cols + 4),
                        end.min(cols + 4),
                        (rnd() % 0x00FF_FFFF) as u32,
                    ));
                    cur = end;
                }
                if rnd().is_multiple_of(4) {
                    bg.reverse();
                }
                if rnd().is_multiple_of(6) && bg.len() > 1 {
                    bg.swap(0, 1);
                }
                bg_rows.push(bg);

                let n = (rnd() % 4) as usize;
                let mut ls = Vec::new();
                let mut cur = 0usize;
                for _ in 0..n {
                    let start = cur + (rnd() % 3) as usize;
                    let end = start + (rnd() % 6) as usize;
                    ls.push(LineSizeSpan::new(
                        start.min(cols + 4),
                        end.min(cols + 4),
                        match rnd() % 4 {
                            0 => LineSize::SingleWidth,
                            1 => LineSize::DoubleWidth,
                            2 => LineSize::DoubleHeightTop,
                            _ => LineSize::DoubleHeightBottom,
                        },
                    ));
                    cur = end;
                }
                if rnd().is_multiple_of(5) {
                    ls.reverse();
                }
                if rnd().is_multiple_of(7) && !ls.is_empty() {
                    let d = ls[0];
                    ls.push(d);
                }
                ls_rows.push(ls);

                let cols_of = |rnd: &mut dyn FnMut() -> u64| {
                    let n = (rnd() % 6) as usize;
                    let mut v: Vec<usize> = Vec::new();
                    let mut c = 0usize;
                    for _ in 0..n {
                        c += (rnd() % 5) as usize;
                        v.push(c.min(cols + 3));
                    }
                    if rnd().is_multiple_of(5) {
                        v.reverse();
                    }
                    if rnd().is_multiple_of(6) && !v.is_empty() {
                        v.push(v[0]);
                    }
                    v
                };
                let a = cols_of(&mut rnd);
                let b = cols_of(&mut rnd);
                let d = cols_of(&mut rnd);
                cl_rows.push(
                    a.into_iter()
                        .map(|c| (c, format!("g{c}").into_boxed_str()))
                        .collect::<Vec<_>>(),
                );
                cb_rows.push(
                    b.into_iter()
                        .map(|c| (c, vec!['\u{301}'].into_boxed_slice()))
                        .collect::<Vec<_>>(),
                );
                im_rows.push(
                    d.into_iter()
                        .map(|c| {
                            (
                                c,
                                ImageRef {
                                    image: Arc::new(ImageData {
                                        bytes: Vec::new(),
                                        format: ImageFormat::Png,
                                        cols: 1,
                                        rows: 1,
                                        z_index: 0,
                                        band_lift_px: 0,
                                        cell_offset_px: (0, 0),
                                        scaling: ImageScaling::Fit,
                                        source_rect: None,
                                    }),
                                    cell_row: 0,
                                    cell_col: u16::try_from(c.min(65535)).unwrap_or(0),
                                    kitty: None,
                                },
                            )
                        })
                        .collect::<Vec<_>>(),
                );
            }
            if trial % 3 == 0 {
                ls_rows.truncate(input.rows.saturating_sub(2));
                cl_rows.truncate(input.rows.saturating_sub(3));
            }
            input.default_bg_spans = bg_rows;
            input.line_size_spans = ls_rows;
            input.clusters = cl_rows;
            input.combining = cb_rows;
            input.images = im_rows;

            for row in 0..input.rows + 1 {
                let mut scan = RowScan::new(&input, row);
                let mut col = 0usize;
                while col < cols + 2 {
                    if rnd().is_multiple_of(2) {
                        assert_eq!(
                            scan.default_bg(col),
                            input.default_bg_at(row, col),
                            "trial {trial} default_bg ({row},{col})"
                        );
                    }
                    if rnd().is_multiple_of(2) {
                        assert_eq!(
                            scan.line_size(col),
                            input.line_size_run_at(row, col).0,
                            "trial {trial} line_size ({row},{col})"
                        );
                    }
                    if rnd().is_multiple_of(2) {
                        assert_eq!(
                            scan.cluster(col),
                            input.cluster_at(row, col),
                            "trial {trial} cluster ({row},{col})"
                        );
                    }
                    if rnd().is_multiple_of(2) {
                        assert_eq!(
                            scan.combining(col),
                            input.combining_at(row, col),
                            "trial {trial} combining ({row},{col})"
                        );
                    }
                    if rnd().is_multiple_of(2) {
                        assert_eq!(
                            scan.image(col).map(std::ptr::from_ref),
                            input.image_at(row, col).map(std::ptr::from_ref),
                            "trial {trial} image ({row},{col})"
                        );
                    }
                    col += 1 + (rnd() % 3) as usize;
                }
            }
        }
    }

    /// The implicit sparse-tail cell is rebuilt only when the row's default
    /// background moves. This row has an empty materialized prefix, so every
    /// cell is implicit, and a stale implicit would paint the second pane's
    /// blank tail as Ink.
    #[test]
    fn review_sparse_tail_implicit_cell_tracks_a_mid_row_background_seam() {
        let mut term = Terminal::new(10, 40);
        term.process(b"\x1b[?25l");
        let (mut input, facts) = snapshot(&mut term);
        input.default_bg_spans = vec![Vec::new(); input.rows];
        input.default_bg_spans[5] = vec![
            DefaultBgSpan::new(0, 20, 0x00FF_FFFF),
            DefaultBgSpan::new(20, input.cols, 0x0012_3456),
        ];
        // NO materialized cells: the whole row is the sparse tail, which the
        // renderer's contract says is EMPTY under each pane's live default.
        input.cells[5] = Vec::new();
        let mut world = PetWorld::default();
        assert!(world.observe(&input, &facts, PetPane::full(&input)));
        for col in [0, 19, 20, 39] {
            assert_eq!(
                world.cell(5, col),
                PetCell::Clear,
                "sparse tail at column {col} is its own pane's EMPTY, not ink"
            );
        }
    }
}
