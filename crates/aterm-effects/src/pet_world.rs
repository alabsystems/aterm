// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Bounded, text-blind perception of the pet's currently presented pane.

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
/// Fractional, pane-local sprite rectangle in cells.
pub struct PetRect {
    pub row: f32,
    pub col: f32,
    pub rows: f32,
    pub cols: f32,
}

impl PetRect {
    #[inline]
    #[must_use]
    pub const fn new(row: f32, col: f32, rows: f32, cols: f32) -> Self {
        Self {
            row,
            col,
            rows,
            cols,
        }
    }

    #[inline]
    #[must_use]
    pub const fn foot_row(self) -> f32 {
        self.row + self.rows - 1.0
    }

    #[inline]
    #[must_use]
    pub const fn valid(self) -> bool {
        self.row.is_finite()
            && self.col.is_finite()
            && self.rows.is_finite()
            && self.cols.is_finite()
            && self.rows > 0.0
            && self.cols > 0.0
    }

    /// Do these two rectangles share any area at all?
    #[inline]
    #[must_use]
    /// Whether `self` and `other` share any area.
    pub const fn overlaps(self, other: Self) -> bool {
        self.valid()
            && other.valid()
            && self.row < other.row + other.rows
            && other.row < self.row + self.rows
            && self.col < other.col + other.cols
            && other.col < self.col + self.cols
    }

    #[inline]
    const fn union(self, other: Self) -> Self {
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
/// observation), by what each is on the glass.
#[derive(Clone, Copy, Debug, Default)]
/// Host late overlays painted into the pane after this observation.
pub struct PetReserve<'a> {
    /// Surfaces the pet never stands on for good (chrome, footer rules/chips, active blocks).
    pub protected: &'a [PetRect],
    /// Transient host overlays (Autopilot menu, find bar) omitted from the ground plane.
    pub overlays: &'a [PetRect],
    /// Predicted echo and IME preedit rectangles treated as ink.
    pub ink: &'a [PetRect],
}

impl<'a> PetReserve<'a> {
    /// Only protected surfaces: the reserve before typed glyphs were told
    /// apart, and every host that splices none.
    #[inline]
    #[must_use]
    /// Reserve containing only protected surfaces.
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
    /// Outside the coverage window: unseen, so unknown.
    Outside,
    Cells(usize, usize, usize, usize),
}

/// One map-local cell that only the live caret's 3x3 ring protects.
#[derive(Clone, Copy, Debug, Default)]
struct CaretCell {
    row: usize,
    col: usize,
    /// Would this cell have been [`PetCell::Clear`] with no caret at all?
    blank: bool,
}

/// Which caret ring cells a clearance reading may discount.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Forgive {
    /// Placement: the caret stays protected, so the pet never rests on it.
    Nothing,
    /// Only ring cells that carry nothing the user can see.
    BlankCaret,
    /// Every ring cell — for the reading that already admits ordinary ink.
    AnyCaret,
}

/// The terminal's rectangle in the supplied frame. All returned geometry is
/// local to this rectangle; chrome and sibling panes are never candidates.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
/// Terminal pane rectangle in the supplied frame.
pub struct PetPane {
    pub row: usize,
    pub col: usize,
    pub rows: usize,
    pub cols: usize,
}

impl PetPane {
    pub const EMPTY: Self = Self {
        row: 0,
        col: 0,
        rows: 0,
        cols: 0,
    };

    #[inline]
    #[must_use]
    pub const fn full(input: &RenderInput) -> Self {
        Self {
            row: 0,
            col: 0,
            rows: input.rows,
            cols: input.cols,
        }
    }
}

/// Identity fences for retained content anchors.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
/// Identity fences for retained content anchors.
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
    /// Scroll-band batch counter used to verify whether coordinate changes are explained by scroll.
    pub scroll_band_batches: u64,
    pub scroll_band_seq: u64,
}

impl PetWorldStamp {
    /// A last visible caret remains a screen-local home through a completely
    /// explained scroll.
    #[inline]
    /// Whether a visible caret remains a valid screen-local home across `previous` and `self`.
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
        let counters = |stamp: Self| ContentScrollState {
            uniform_up_rows: stamp.uniform_up_rows,
            invalidation_epoch: stamp.surface.scroll_invalidation,
            band_batches: stamp.scroll_band_batches,
            band_seq: stamp.scroll_band_seq,
            ..ContentScrollState::default()
        };
        match ContentScrollState::delta_since(Some(counters(previous)), counters(self)) {
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
/// read.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
/// Terminal geometry and selection/caret facts captured alongside cell extraction.
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
#[derive(Clone, Copy, Debug)]
/// Visible block with a stable absolute row and current pane projection.
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
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PetGlyph {
    Blank,
    Speck,
    Substantive,
}

/// A fixed-budget occupancy window and small, text-free observation set.
#[derive(Clone, Debug)]
/// Fixed-budget occupancy window and text-free observation set.
pub struct PetWorld {
    cells: Vec<PetCell>,
    glyphs: Vec<Option<PetGlyph>>,
    /// Per cell: a letterform (a letter or digit of any script, a wide lead
    /// and its continuation alike), the ink a reader reads; a rule, a box's
    /// border or punctuation is not one ([`Self::letter_at`]).
    letters: Vec<bool>,
    blocked_prefix: Vec<u16>,
    protected_prefix: Vec<u16>,
    /// Per cell: is anything painted in the text layer (glyph, coloured background, selection,
    /// image, exclusion, expanded row, or an unobserved cell)?
    inked: Vec<bool>,
    /// THE GROUND PLANE: each cell as it would be with no live selection and
    /// no transient host overlay (the Autopilot menu, the find bar).
    ground_cells: Vec<PetCell>,
    ground_blocked_prefix: Vec<u16>,
    ground_protected_prefix: Vec<u16>,
    /// Whether this observation saw a transient cell, so the ground plane
    /// differs from `cells`.
    ground_split: bool,
    /// Ring cells under a selection or an overlay and nothing lasting: on
    /// the ground plane only the caret protects them, so its readers forgive
    /// them as `caret_cells` are forgiven on the live plane (a shell's stand
    /// sits in the ring by design).
    ground_caret_cells: [CaretCell; 9],
    ground_caret_cells_len: u8,
    /// Per map row, the map-local columns `[lo, hi)` from its first
    /// substantive ink to its last: a glyph (not a lone Braille speck) or a
    /// coloured background.
    hull: Vec<(u16, u16)>,
    coverage: PetPane,
    /// Map-local cells only the live caret's 3x3 ring protects (at most nine), kept apart so
    /// each visibility reading forgives its own subset ([`Forgive`]).
    caret_cells: [CaretCell; 9],
    caret_cells_len: u8,
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
            cells: Vec::new(),
            glyphs: Vec::new(),
            letters: Vec::new(),
            blocked_prefix: Vec::new(),
            protected_prefix: Vec::new(),
            inked: Vec::new(),
            ground_cells: Vec::new(),
            ground_blocked_prefix: Vec::new(),
            ground_protected_prefix: Vec::new(),
            ground_split: false,
            ground_caret_cells: [CaretCell::default(); 9],
            ground_caret_cells_len: 0,
            hull: Vec::new(),
            coverage: PetPane::default(),
            caret_cells: [CaretCell::default(); 9],
            caret_cells_len: 0,
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
    #[inline]
    /// Retires all observed geometry without reallocating the bounded buffer.
    pub const fn retire(&mut self) {
        self.stamp = None;
        self.coverage = PetPane::EMPTY;
        self.ground_split = false;
        self.caret_cells_len = 0;
        self.ground_caret_cells_len = 0;
        self.anchors = [None; MAX_WORLD_ANCHORS];
        self.selection_rect = None;
        self.selection_target = None;
        self.progress = None;
        self.examined_cells = 0;
    }

    #[inline]
    #[must_use]
    pub const fn stamp(&self) -> Option<PetWorldStamp> {
        self.stamp
    }
    #[inline]
    #[must_use]
    #[cfg(test)]
    pub const fn coverage(&self) -> PetPane {
        self.coverage
    }
    #[inline]
    #[must_use]
    pub const fn examined_cells(&self) -> usize {
        self.examined_cells
    }
    #[inline]
    #[must_use]
    pub const fn anchors(&self) -> &[Option<PetAnchor>; MAX_WORLD_ANCHORS] {
        &self.anchors
    }
    #[inline]
    #[must_use]
    pub const fn selection_rect(&self) -> Option<PetRect> {
        self.selection_rect
    }
    #[inline]
    #[must_use]
    pub const fn selection_target(&self) -> Option<(f32, f32)> {
        self.selection_target
    }
    /// The application's last explicit state. No task identity, update event,
    /// or command completion is inferred from this sampled value.
    #[inline]
    #[must_use]
    /// Application's last explicit taskbar progress state.
    pub const fn progress(&self) -> Option<TaskbarProgress> {
        self.progress
    }

    /// Rebuild from a coherent pre-effects snapshot. False retires every
    /// observation.
    #[inline]
    pub fn observe(&mut self, input: &RenderInput, facts: &PetWorldFacts, pane: PetPane) -> bool {
        self.observe_with_exclusions(input, facts, pane, &[])
    }

    /// As `observe`, with pane-local search/menu/overlay exclusion rectangles.
    #[inline]
    /// Like [`Self::observe`], with pane-local exclusion rectangles.
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
    /// overlays apart (`overlays`: the Autopilot menu, the find bar).
    #[inline]
    /// Like [`Self::observe_with_exclusions`], separating transient `overlays` from `protected`.
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
    /// the person's typed glyphs as ink.
    #[inline]
    /// Like [`Self::observe`], with host late overlays classified in [`PetReserve`].
    pub fn observe_with_reserve(
        &mut self,
        input: &RenderInput,
        facts: &PetWorldFacts,
        pane: PetPane,
        reserve: PetReserve<'_>,
    ) -> bool {
        self.observe_with_exclusions_near(input, facts, pane, reserve, None)
    }

    /// Observes cells around `hint_rect` when the console hides its cursor.
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
        let any_selection = input.any_selection();
        let track_ground = any_selection || !overlays.is_empty();
        const PREFIX_LEN: usize = (MAX_WORLD_ROWS + 1) * (MAX_WORLD_COLS + 1);
        if self.cells.is_empty() {
            self.cells.resize(MAX_WORLD_CELLS, PetCell::Unknown);
            self.glyphs.resize(MAX_WORLD_CELLS, None);
            self.letters.resize(MAX_WORLD_CELLS, false);
            self.blocked_prefix.resize(PREFIX_LEN, 0);
            self.protected_prefix.resize(PREFIX_LEN, 0);
            self.inked.resize(MAX_WORLD_CELLS, true);
            self.hull.resize(MAX_WORLD_ROWS, (0, 0));
        }
        if track_ground && self.ground_cells.is_empty() {
            self.ground_cells.resize(MAX_WORLD_CELLS, PetCell::Unknown);
        }
        let stride = MAX_WORLD_COLS + 1;
        self.blocked_prefix[..=cols].fill(0);
        self.protected_prefix[..=cols].fill(0);
        let mut n_caret_cells = 0usize;
        let mut n_ground_caret_cells = 0usize;
        for r in 0..rows {
            let row_base = r * MAX_WORLD_COLS;
            let prefix_row = (r + 1) * stride;
            self.blocked_prefix[prefix_row] = 0;
            self.protected_prefix[prefix_row] = 0;
            let mut row_blocked = 0u16;
            let mut row_protected = 0u16;
            let local_r = self.coverage.row + r;
            let frame_r = pane.row + local_r;
            let Some(row) = input.cells.get(frame_r) else {
                self.cells[row_base..row_base + cols].fill(PetCell::Unknown);
                self.glyphs[row_base..row_base + cols].fill(None);
                self.letters[row_base..row_base + cols].fill(false);
                self.inked[row_base..row_base + cols].fill(true);
                if track_ground {
                    self.ground_cells[row_base..row_base + cols].fill(PetCell::Unknown);
                }
                let mut c = 0;
                while c < cols {
                    let index = prefix_row + c + 1;
                    row_blocked += 1;
                    row_protected += 1;
                    self.blocked_prefix[index] = self.blocked_prefix[index - stride] + row_blocked;
                    self.protected_prefix[index] =
                        self.protected_prefix[index - stride] + row_protected;
                    c += 1;
                }
                self.hull[r] = (0, cols as u16);
                continue;
            };
            let (mut hull_lo, mut hull_hi) = (u16::MAX, 0u16);
            let mut spans = [(0, 0); MAX_REGIONS];
            let mut n_spans = 0;
            for rect in exclusions {
                if let Some(span) = exclusion_span(*rect, local_r, pane.cols) {
                    spans[n_spans] = span;
                    n_spans += 1;
                }
            }
            let spans = &spans[..n_spans];
            let mut overlay_spans = [(0, 0); MAX_REGIONS];
            let mut n_overlay = 0;
            for rect in overlays {
                if let Some(span) = exclusion_span(*rect, local_r, pane.cols) {
                    overlay_spans[n_overlay] = span;
                    n_overlay += 1;
                }
            }
            let overlay_spans = &overlay_spans[..n_overlay];
            let mut typed_spans = [(0, 0); MAX_REGIONS];
            let mut n_typed = 0;
            for rect in reserved_ink {
                if let Some(span) = exclusion_span(*rect, local_r, pane.cols) {
                    typed_spans[n_typed] = span;
                    n_typed += 1;
                }
            }
            let typed_spans = &typed_spans[..n_typed];
            let mut scan = RowScan::new(input, frame_r);
            let mut implicit_bg = None;
            let mut implicit = RenderCell::default();
            let cursor_row_near = input.cursor_visible
                && input.display_offset == 0
                && frame_r.abs_diff(input.cursor_row) <= 1;
            let mut next_c = 0;
            while next_c < cols {
                let c = next_c;
                next_c += 1;
                let local_c = self.coverage.col + c;
                let frame_c = pane.col + local_c;
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
                let cursor = cursor_row_near && frame_c.abs_diff(input.cursor_col) <= 1;
                let excluded = spans.iter().any(|&(lo, hi)| lo <= local_c && local_c < hi);
                let overlaid = overlay_spans
                    .iter()
                    .any(|&(lo, hi)| lo <= local_c && local_c < hi);
                let typed = typed_spans
                    .iter()
                    .any(|&(lo, hi)| lo <= local_c && local_c < hi);
                let bg = (u32::from(cell.bg[0]) << 16)
                    | (u32::from(cell.bg[1]) << 8)
                    | u32::from(cell.bg[2]);
                let decorated =
                    cell.underline != UnderlineStyle::None || cell.strikethrough || cell.overline;
                let compound = scan.cluster(frame_c).is_some() || scan.combining(frame_c).is_some();
                let line = scan.line_size(frame_c);
                let glyph = cell.ch != ' ' || cell.wide || decorated || compound;
                let speck = glyph
                    && !cell.wide
                    && !decorated
                    && !compound
                    && is_single_braille_dot(cell.ch)
                    && c > 0
                    && c + 1 < cols
                    && self.glyphs[row_base + c - 1] == Some(PetGlyph::Blank)
                    && {
                        let column = frame_c + 1;
                        let neighbour = row.get(column).unwrap_or(&implicit);
                        neighbour.ch == ' '
                            && !neighbour.wide
                            && neighbour.underline == UnderlineStyle::None
                            && !neighbour.strikethrough
                            && !neighbour.overline
                            && input.line_size_run_at(frame_r, column).0 == LineSize::SingleWidth
                            && input.cluster_at(frame_r, column).is_none()
                            && input.combining_at(frame_r, column).is_none()
                    };
                // A wide glyph's continuation reads as its lead does.
                self.letters[row_base + c] = line == LineSize::SingleWidth
                    && if cell.wide {
                        c > 0 && self.letters[row_base + c - 1]
                    } else {
                        cell.ch.is_alphanumeric()
                    };
                self.glyphs[row_base + c] = (line == LineSize::SingleWidth).then_some(if !glyph {
                    PetGlyph::Blank
                } else if speck {
                    PetGlyph::Speck
                } else {
                    PetGlyph::Substantive
                });
                let lasting =
                    excluded || scan.image(frame_c).is_some() || line != LineSize::SingleWidth;
                let transient = selected || overlaid;
                let others = lasting || transient;
                let painted = glyph || typed || bg != default_bg;
                if (glyph && !speck) || typed || bg != default_bg {
                    hull_lo = hull_lo.min(c as u16);
                    hull_hi = c as u16 + 1;
                }
                self.inked[row_base + c] = others || painted;
                if cursor && !others && n_caret_cells < self.caret_cells.len() {
                    self.caret_cells[n_caret_cells] = CaretCell {
                        row: r,
                        col: c,
                        blank: !painted,
                    };
                    n_caret_cells += 1;
                }
                let class = if others || cursor {
                    PetCell::Protected
                } else if painted {
                    PetCell::Ink
                } else {
                    PetCell::Clear
                };
                self.cells[row_base + c] = class;
                let index = prefix_row + c + 1;
                row_blocked += u16::from(class != PetCell::Clear);
                row_protected += u16::from(matches!(class, PetCell::Unknown | PetCell::Protected));
                self.blocked_prefix[index] = self.blocked_prefix[index - stride] + row_blocked;
                self.protected_prefix[index] =
                    self.protected_prefix[index - stride] + row_protected;
                if track_ground
                    && cursor
                    && transient
                    && !lasting
                    && n_ground_caret_cells < self.ground_caret_cells.len()
                {
                    self.ground_caret_cells[n_ground_caret_cells] = CaretCell {
                        row: r,
                        col: c,
                        blank: !painted,
                    };
                    n_ground_caret_cells += 1;
                }
                if track_ground {
                    self.ground_cells[row_base + c] = if transient {
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
        self.caret_cells_len = n_caret_cells as u8;
        self.ground_caret_cells_len = n_ground_caret_cells as u8;
        if self.ground_split {
            if self.ground_blocked_prefix.is_empty() {
                self.ground_blocked_prefix.resize(PREFIX_LEN, 0);
                self.ground_protected_prefix.resize(PREFIX_LEN, 0);
            }
            self.ground_blocked_prefix[..=cols].fill(0);
            self.ground_protected_prefix[..=cols].fill(0);
            for r in 0..rows {
                let row_base = r * MAX_WORLD_COLS;
                let prefix_row = (r + 1) * stride;
                self.ground_blocked_prefix[prefix_row] = 0;
                self.ground_protected_prefix[prefix_row] = 0;
                let mut row_blocked = 0u16;
                let mut row_protected = 0u16;
                let mut c = 0;
                while c < cols {
                    let index = prefix_row + c + 1;
                    let cell = self.ground_cells[row_base + c];
                    row_blocked += u16::from(cell != PetCell::Clear);
                    row_protected +=
                        u16::from(matches!(cell, PetCell::Unknown | PetCell::Protected));
                    self.ground_blocked_prefix[index] =
                        self.ground_blocked_prefix[index - stride] + row_blocked;
                    self.ground_protected_prefix[index] =
                        self.ground_protected_prefix[index - stride] + row_protected;
                    c += 1;
                }
            }
        }
        if self.selection_rect.is_none() {
            self.selection_target = None;
        }
        if !s.alt_screen {
            let top = facts.stamp.top_absolute_row;
            let end = top.saturating_add(pane.rows as u64);
            for (slot, block) in self.anchors.iter_mut().zip(facts.blocks.iter()) {
                let Some(block) = block else {
                    continue;
                };
                let start = block.output_row.unwrap_or(block.start_row).max(top);
                let last = block.end_row.min(end).saturating_sub(1);
                if start > last {
                    continue;
                }
                let mut absolute_row = start;
                let mut col = 0.0;
                'rows: for r in (0..rows).rev() {
                    let abs = top.saturating_add((self.coverage.row + r) as u64);
                    if abs < start || abs > last {
                        continue;
                    }
                    let row_base = r * MAX_WORLD_COLS;
                    let row_glyphs = &self.glyphs[row_base..row_base + cols];
                    for (c, glyph) in row_glyphs.iter().enumerate().rev() {
                        if matches!(glyph, Some(PetGlyph::Speck | PetGlyph::Substantive)) {
                            absolute_row = abs;
                            col = (self.coverage.col + c + 1) as f32;
                            break 'rows;
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

    #[inline]
    #[must_use]
    pub const fn cell(&self, row: usize, col: usize) -> PetCell {
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
        self.cells.as_slice()[r * MAX_WORLD_COLS + c]
    }

    /// Actual glyph/decorated-cell occupancy, independent of caret/selection
    /// protection.
    #[inline]
    #[must_use]
    /// Whether cell `(row, col)` has glyph or decoration ink (`None` if unobserved).
    pub const fn ink_at(&self, row: usize, col: usize) -> Option<bool> {
        let Some(glyph) = self.glyph_at(row, col) else {
            return None;
        };
        Some(!matches!(glyph, PetGlyph::Blank))
    }

    /// Whether an observed cell holds a letterform ([`PetWorld::letters`]):
    /// a letter or digit a reader reads, which a cat at rest never lays its
    /// body on. `None` outside this observation.
    #[inline]
    #[must_use]
    pub fn letter_at(&self, row: usize, col: usize) -> Option<bool> {
        self.glyph_at(row, col)?;
        let (r, c) = (row - self.coverage.row, col - self.coverage.col);
        Some(self.letters[r * MAX_WORLD_COLS + c])
    }

    /// Ink substantial enough to displace a pet drawn beneath the glyphs.
    #[inline]
    #[must_use]
    /// Whether cell `(row, col)` has ink substantial enough to displace a pet under text.
    pub const fn locomotion_ink_at(&self, row: usize, col: usize) -> Option<bool> {
        let Some(glyph) = self.glyph_at(row, col) else {
            return None;
        };
        Some(matches!(glyph, PetGlyph::Substantive))
    }

    #[inline]
    const fn glyph_at(&self, row: usize, col: usize) -> Option<PetGlyph> {
        if self.stamp.is_none() {
            return None;
        }
        let Some(r) = row.checked_sub(self.coverage.row) else {
            return None;
        };
        let Some(c) = col.checked_sub(self.coverage.col) else {
            return None;
        };
        if r >= self.coverage.rows || c >= self.coverage.cols {
            return None;
        }
        self.glyphs.as_slice()[r * MAX_WORLD_COLS + c]
    }

    /// Certify all cells touched by the rectangle and a margin on every side.
    #[inline]
    #[must_use]
    /// Whether all cells touched by `rect` plus `margin` are certified clear.
    pub fn clear(&self, rect: PetRect, margin: f32) -> bool {
        self.clearance(rect, margin) == Some(true)
    }

    /// The full body may stand behind ordinary text and coloured backgrounds;
    /// selection, cursor protection, images and unknown geometry still exclude
    /// it.
    #[inline]
    #[must_use]
    /// Whether `rect` plus `margin` is certified clear or under ordinary text.
    pub fn under_text_clear(&self, rect: PetRect, margin: f32) -> bool {
        self.under_text_clearance(rect, margin) == Some(true)
    }

    /// Three-valued clearance:
    ///
    /// * `Some(true)`: every cell the rectangle and margin touch was examined by
    ///   this snapshot and is free.
    /// * `Some(false)`: a touched cell is not free (ink, protected, or unknown inside
    ///   the coverage window), or the query is degenerate (an invalid rectangle or margin).
    /// * `None`: unknown. There is no coherent snapshot, or the rectangle leaves
    ///   the coverage window, which is centred on the caret while it shows at live
    ///   and slides as the user types. Collapsing this to `false` invents ink.
    #[inline]
    #[must_use]
    pub fn clearance(&self, rect: PetRect, margin: f32) -> Option<bool> {
        self.clearance_in(&self.blocked_prefix, Forgive::Nothing, rect, margin)
    }

    /// [`Self::clearance`] for a resident admitted behind ordinary text: the same three
    /// answers, counted over protected and unknown cells only.
    #[inline]
    #[must_use]
    /// [`Self::clearance`] admitting ordinary text (`Clear` or `UnderText`).
    pub fn under_text_clearance(&self, rect: PetRect, margin: f32) -> Option<bool> {
        self.clearance_in(&self.protected_prefix, Forgive::Nothing, rect, margin)
    }

    /// As [`Self::clearance`], except that the live caret's ring cells that would be `Clear`
    /// without the caret do not count.
    #[inline]
    #[must_use]
    /// [`Self::clearance`] ignoring live caret ring cells that would otherwise be clear.
    pub fn clearance_past_caret(&self, rect: PetRect, margin: f32) -> Option<bool> {
        self.clearance_in(&self.blocked_prefix, Forgive::BlankCaret, rect, margin)
    }

    /// [`Self::clearance_past_caret`] as a two-valued, fail-closed answer:
    /// occupancy for a body beside the live caret, discounting only the caret's
    /// blank ring cells.
    #[inline]
    #[must_use]
    /// Two-valued fail-closed [`Self::clearance_past_caret`].
    pub fn clear_past_caret(&self, rect: PetRect, margin: f32) -> bool {
        self.clearance_past_caret(rect, margin) == Some(true)
    }

    /// The visibility verdict the emitted sprite is judged by: a resident may
    /// stand under ordinary ink, and the caret's ring does not obstruct its
    /// escort.
    #[inline]
    #[must_use]
    /// Visibility clearance under ordinary text past the live caret ring.
    pub fn under_text_clearance_past_caret(&self, rect: PetRect, margin: f32) -> Option<bool> {
        self.clearance_in(&self.protected_prefix, Forgive::AnyCaret, rect, margin)
    }

    /// [`Self::under_text_clearance_past_caret`] over the part of `rect` this map sees, for a
    /// body already on glass before this map judged it (a crossfade ghost):
    ///
    /// * `Some(false)`: a covered cell under it is protected, or the query is degenerate,
    ///   however much of it lies outside the coverage window;
    /// * `Some(true)`: it lies wholly inside the window and nothing there is protected;
    /// * `None`: the map has no stamp yet, or the covered part, if any, is clear and the rest
    ///   is outside; the caller keeps the verdict the body came with.
    #[inline]
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

    /// Z-order, three-valued: is the rectangle free of anything painted in the text layer, so a
    /// body there may be drawn in front of it?
    #[inline]
    #[must_use]
    /// Three-valued check whether `rect` plus `margin` is free of text-layer ink.
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

    #[inline]
    fn clearance_in(
        &self,
        prefix: &[u16],
        forgive: Forgive,
        rect: PetRect,
        margin: f32,
    ) -> Option<bool> {
        self.clearance_on(prefix, false, forgive, rect, margin)
    }

    #[inline]
    const fn carets_len(&self, ground: bool) -> usize {
        (self.caret_cells_len as usize)
            + if ground && self.ground_split {
                self.ground_caret_cells_len as usize
            } else {
                0
            }
    }

    /// The live caret's ring cells a plane forgives: `caret_cells`, and on
    /// the ground plane also those only a selection or an overlay hid.
    #[inline]
    fn carets(&self, ground: bool) -> impl Iterator<Item = &CaretCell> {
        let extra: &[CaretCell] = if ground && self.ground_split {
            &self.ground_caret_cells[..usize::from(self.ground_caret_cells_len)]
        } else {
            &[]
        };
        self.caret_cells[..usize::from(self.caret_cells_len)]
            .iter()
            .chain(extra)
    }

    /// [`Self::clearance_in`] on the live plane, or (`ground`) the ground
    /// plane `prefix` was read from.
    #[inline]
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
                let blocked = usize::from(self.blocked(prefix, r0, c0, r1, c1));
                if blocked == 0
                    || matches!(forgive, Forgive::Nothing)
                    || blocked > self.carets_len(ground)
                {
                    return Some(blocked == 0);
                }
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
                Some(blocked == forgiven)
            }
        }
    }

    /// The half-open map-local cell window a rectangle and its margin touch.
    #[inline]
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
    #[inline]
    const fn blocked(&self, prefix: &[u16], r0: usize, c0: usize, r1: usize, c1: usize) -> u16 {
        let r0_base = r0 * (MAX_WORLD_COLS + 1);
        let r1_base = r1 * (MAX_WORLD_COLS + 1);
        prefix[r1_base + c1] + prefix[r0_base + c0] - prefix[r1_base + c0] - prefix[r0_base + c1]
    }

    /// The swept-body test for a resident travelling behind text. The corridor contains the
    /// body it departs from, and the escort seat (`kitty_pet`'s `STATION_LEAD`) lies inside the
    /// caret's ring, so the caret is forgiven here as in
    /// [`Self::under_text_clearance_past_caret`]; read strictly, the pet could never leave.
    #[inline]
    #[must_use]
    /// Swept-body under-text clearance along the corridor between `from` and `to`.
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
    #[inline]
    const fn ground_prefixes(&self) -> (&[u16], &[u16]) {
        if self.ground_split {
            (
                self.ground_blocked_prefix.as_slice(),
                self.ground_protected_prefix.as_slice(),
            )
        } else {
            (
                self.blocked_prefix.as_slice(),
                self.protected_prefix.as_slice(),
            )
        }
    }

    /// [`Self::clear`] on the GROUND PLANE: as if no selection were live and
    /// no transient overlay were open (`ground_cells`).
    #[inline]
    #[must_use]
    /// [`Self::clear`] on the ground plane (ignoring live selection and transient overlays).
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
    #[inline]
    #[must_use]
    /// [`Self::under_text_clearance_past_caret`] on the ground plane.
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

    /// THE GROUND UNDER A CAT, THE HOST'S LABELS OVERHEAD. A body whose feet
    /// are on `rect`'s last row, judged as
    /// [`Self::under_text_clearance_past_caret`] on that row (its margin kept
    /// off the row above), and on the rows over its feet only against what
    /// protects them for now: a selection or a transient overlay, whose cells
    /// a body drawn over them would hide.
    #[inline]
    #[must_use]
    /// Ground-plane under-text clearance for `rect`'s feet row while passing under labels.
    pub fn passing_under_text_clearance_past_caret(
        &self,
        rect: PetRect,
        margin: f32,
    ) -> Option<bool> {
        self.stamp?;
        if !rect.valid() || !margin.is_finite() || margin < 0.0 {
            return Some(false);
        }
        let bottom = rect.row + rect.rows;
        let feet_top = Self::feet_row_of(rect).row;
        let feet = PetRect::new(
            feet_top + margin,
            rect.col,
            (bottom.min(feet_top + 1.0) - feet_top - 2.0 * margin).max(f32::EPSILON),
            rect.cols,
        );
        let ground = self.under_text_clearance_past_caret(feet, margin);
        if feet_top <= rect.row || ground == Some(false) {
            return ground;
        }
        let over = PetRect::new(rect.row, rect.col, feet_top - rect.row, rect.cols);
        let overhead = match self.window(over, margin) {
            Window::Degenerate => Some(false),
            Window::Outside => None,
            Window::Cells(r0, c0, r1, c1) => Some(
                !self.ground_split
                    || self.blocked(&self.protected_prefix, r0, c0, r1, c1)
                        == self.blocked(self.ground_prefixes().1, r0, c0, r1, c1),
            ),
        };
        match (ground, overhead) {
            (_, Some(false)) => Some(false),
            (Some(true), Some(true)) => Some(true),
            _ => None,
        }
    }

    /// The part of `rect` on its feet's row: from the top of the cell row its
    /// bottom edge lies in ([`Self::passing_under_text_clearance_past_caret`]).
    #[inline]
    #[must_use]
    /// Slice of `rect` lying on its bottom edge's cell row ([`Self::passing_under_text_clearance_past_caret`]).
    pub fn feet_row_of(rect: PetRect) -> PetRect {
        let bottom = rect.row + rect.rows;
        let top = ((bottom - 1e-3).ceil() - 1.0).max(rect.row);
        PetRect::new(top, rect.col, (bottom - top).max(f32::EPSILON), rect.cols)
    }

    /// Whether a body can go from `from` to `to` without crossing anything
    /// that protects the ground for good: their swept box, with no margin,
    /// clear on the ground plane past the caret
    /// ([`Self::ground_under_text_clearance_past_caret`]).
    #[inline]
    #[must_use]
    /// Whether the ground corridor from `from` to `to` is certified free of permanent protection.
    pub fn ground_corridor_clear(&self, from: PetRect, to: PetRect) -> bool {
        from.valid()
            && to.valid()
            && self.ground_under_text_clearance_past_caret(from.union(to), 0.0) == Some(true)
    }

    /// Whether something that protects the ground for good is OBSERVED
    /// between `from` and `to`: a protected cell in their swept box, with no
    /// margin, on the ground plane past the caret.
    #[inline]
    #[must_use]
    /// Whether any permanently protected cell is observed along the corridor from `from` to `to`.
    pub fn ground_corridor_walled(&self, from: PetRect, to: PetRect) -> bool {
        from.valid()
            && to.valid()
            && self.ground_under_text_clearance_past_caret(from.union(to), 0.0) == Some(false)
    }

    /// Whether `rect` and its margin stay off every observed row's ink hull
    /// (`hull`): no gap inside a line counts as ground.
    #[inline]
    #[must_use]
    /// Whether `rect` plus `margin` stays outside every observed row's ink hull.
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
    #[inline]
    fn ground_protected_count(&self, r0: usize, c0: usize, r1: usize, c1: usize) -> u16 {
        let raw = self.blocked(self.ground_prefixes().1, r0, c0, r1, c1);
        if raw == 0 || self.carets_len(true) == 0 {
            return raw;
        }
        let forgiven = self
            .carets(true)
            .filter(|cell| cell.row >= r0 && cell.row < r1 && cell.col >= c0 && cell.col < c1)
            .count() as u16;
        raw.saturating_sub(forgiven)
    }

    /// The map-local columns `[lo, hi)` a body at map-local cells
    /// `from = (r0, c0, r1, c1)` can reach on rows `r0..r1` widened to
    /// `rows`, crossing nothing the ground plane protects: the widest span
    /// round its own columns that is clear on every row in between.
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
    /// identity fences hold and its block is still observed.
    #[inline]
    #[must_use]
    /// Resolves `anchor` if its owner and row identity fences still hold and it remains clear.
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
    #[inline]
    #[must_use]
    /// Nearest certified clear perch to `near` within `max_distance`.
    pub fn nearest_perch(&self, near: PetRect, margin: f32, max_distance: f32) -> Option<PetRect> {
        self.perch_near((near.foot_row(), near.col), near, margin, max_distance)
    }

    /// Prefer empty space near home, then the same bounded search under text.
    #[inline]
    #[must_use]
    /// Nearest clear perch to `near`, falling back to under-text perch within `max_distance`.
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
    /// on words when a blank seat is in reach.
    #[must_use]
    /// Nearest clean-ground rest seat to `near` within `max_distance`.
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
        let from = from.and_then(|from| match self.window(from, 0.0) {
            Window::Cells(r0, c0, r1, c1) => Some(((r0, c0, r1, c1), from.row + from.rows)),
            Window::Degenerate | Window::Outside => None,
        });
        let last_band = (cov.rows - span_r) as i64;
        let last_col = (cov.cols - span_c) as i64;
        let home_band = ((near.row - inset_r - cov.row as f32).round() as i64).clamp(0, last_band);
        let home_x = near.col - inset_c - cov.col as f32;
        let home_col = (home_x.round() as i64).clamp(0, last_col);
        let top = |band: i64| cov.row as f32 + band as f32 + inset_r;
        let left = |col: i64| cov.col as f32 + col as f32 + inset_c;
        let prefix = self.ground_prefixes().0;
        let mut best = None;
        let mut best_distance = f32::INFINITY;
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
                let (reach_lo, reach_hi) = match from {
                    Some((from, feet)) if (top(band) + near.rows - feet).abs() < 0.5 => {
                        match self.reach_span(from, (r0, r1)) {
                            Some(span) => span,
                            None => continue,
                        }
                    }
                    _ => (0, cov.cols),
                };
                if reach_hi < reach_lo + span_c {
                    continue;
                }
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
                let width = (c_hi - c_lo) as u16;
                if (r0..r1)
                    .any(|r| width - self.blocked(prefix, r, c_lo, r + 1, c_hi) < span_c as u16)
                {
                    continue;
                }
                let r0_row = &prefix[r0 * (MAX_WORLD_COLS + 1)..];
                let r1_row = &prefix[r1 * (MAX_WORLD_COLS + 1)..];
                let band_hulls = &self.hull[r0..r1];
                let seat_blocked = |c: i64| {
                    let c0 = c as usize;
                    let c1 = c0 + span_c;
                    (r1_row[c1] + r0_row[c0] - r1_row[c0] - r0_row[c1]) > 0
                };
                let column_blocked = |c: i64| {
                    let c0 = c as usize;
                    let c1 = c0 + 1;
                    (r1_row[c1] + r0_row[c0] - r1_row[c0] - r0_row[c1]) > 0
                };
                let hulls = |c: i64| {
                    let (c0, c1) = (c as u16, (c as usize + span_c) as u16);
                    band_hulls
                        .iter()
                        .filter(|&&(lo, hi)| lo < hi && c0 < hi && c1 > lo)
                        .fold(None, |met: Option<(u16, u16)>, &(lo, hi)| {
                            Some(met.map_or((lo, hi), |(a, b)| (a.min(lo), b.max(hi))))
                        })
                };
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
    #[inline]
    #[must_use]
    /// Closest protected-free placement to `near` within `max_distance`.
    pub fn nearest_under_text_perch(
        &self,
        near: PetRect,
        margin: f32,
        max_distance: f32,
    ) -> Option<PetRect> {
        self.nearest_under_text_perch_from(near, margin, max_distance, None)
    }

    /// The margin round `rect` that stays inside the map: `margin`, or less
    /// for a body against the map's edge.
    #[inline]
    #[must_use]
    /// Largest margin up to `margin` around `rect` that stays inside the map.
    pub const fn margin_in_map(&self, rect: PetRect, margin: f32) -> f32 {
        let cov = self.coverage;
        let (top, left) = (cov.row as f32, cov.col as f32);
        let (bottom, right) = ((cov.row + cov.rows) as f32, (cov.col + cov.cols) as f32);
        let room = (rect.row - top)
            .min(rect.col - left)
            .min(bottom - (rect.row + rect.rows))
            .min(right - (rect.col + rect.cols));
        if !rect.valid() || !margin.is_finite() || room < 0.0 {
            return margin;
        }
        margin.min(room)
    }

    /// Whether `to` is on the ground `from` stands on: the same feet row,
    /// give or take half a row.
    #[inline]
    #[must_use]
    /// Whether `from` and `to` share the same feet row within half a cell.
    pub const fn same_ground(from: PetRect, to: PetRect) -> bool {
        (from.foot_row() - to.foot_row()).abs() < 0.5
    }

    /// Whether a body at `from` can get to `to` past protected ground: on
    /// its own ground ([`Self::same_ground`]) by a walk whose box is clear
    /// ([`Self::ground_corridor_clear`]); anywhere else by a flight, which
    /// passes over the host's chrome (the console law leaves a body in the
    /// air alone: `kitty_pet::console_life`'s `place_console_body_at_home`).
    #[inline]
    #[must_use]
    /// Whether `from` can reach `to` on the same ground ([`Self::same_ground`]) without a wall.
    pub fn reaches(&self, from: PetRect, to: PetRect) -> bool {
        !Self::same_ground(from, to) || self.ground_corridor_clear(from, to)
    }

    /// Closest protected-free placement reachable from `from`
    /// ([`Self::reaches`]): walked to on its own ground without crossing
    /// protected ground, flown to on any other.
    #[inline]
    #[must_use]
    /// Closest protected-free placement to `near` reachable from `from` ([`Self::reaches`]).
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
    #[inline]
    #[must_use]
    /// Certified clear or under-text perch near attention cell `target`.
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
        self.stamp?;
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
        let cov_top = self.coverage.row as f32;
        let cov_bottom = (self.coverage.row + self.coverage.rows) as f32;
        let cov_left = self.coverage.col as f32;
        let cov_right = (self.coverage.col + self.coverage.cols) as f32;
        let prefix = if under_text {
            &self.protected_prefix
        } else {
            &self.blocked_prefix
        };
        let mut best = None;
        let mut best_distance = f32::INFINITY;
        let mut best_tie = f32::INFINITY;
        let first_col = (target.1 - max_distance - cov_left - margin)
            .ceil()
            .max(0.0) as usize;
        let end_col =
            ((target.1 + max_distance - cov_left - margin).floor() + 1.0).max(0.0) as usize;
        let end_col = end_col.min(self.coverage.cols);
        for (i, row) in rows.into_iter().enumerate() {
            let r0_f = row - margin;
            let r1_f = row + body.rows + margin;
            if rows[..i].contains(&row) || !(r0_f >= cov_top && r1_f <= cov_bottom) {
                continue;
            }
            let row_dist = (row - want_row).abs() * 2.0;
            if row_dist > max_distance || row_dist > best_distance {
                continue;
            }
            let r0 = r0_f.floor() as usize - self.coverage.row;
            let r1 = r1_f.ceil() as usize - self.coverage.row;
            let r0_row = &prefix[r0 * (MAX_WORLD_COLS + 1)..];
            let r1_row = &prefix[r1 * (MAX_WORLD_COLS + 1)..];
            let same_ground_from = if under_text {
                from.filter(|&f| Self::same_ground(f, PetRect::new(row, 0.0, body.rows, body.cols)))
            } else {
                None
            };
            let mut stop_after = |col: f32| {
                if col > target.1 && col - target.1 > max_distance.min(best_distance) {
                    return true;
                }
                let col_dist = (col - target.1).abs();
                let distance = row_dist.max(col_dist);
                let tie = row_dist + col_dist;
                if distance <= max_distance
                    && (distance < best_distance || (distance == best_distance && tie < best_tie))
                {
                    let c0_f = col - margin;
                    let c1_f = col + body.cols + margin;
                    if c0_f >= cov_left && c1_f <= cov_right {
                        let c0 = c0_f.floor() as usize - self.coverage.col;
                        let c1 = c1_f.ceil() as usize - self.coverage.col;
                        if r1_row[c1] + r0_row[c0] - r1_row[c0] - r0_row[c1] == 0 {
                            let rect = PetRect::new(row, col, body.rows, body.cols);
                            if same_ground_from
                                .is_none_or(|from| self.ground_corridor_clear(from, rect))
                            {
                                best = Some(rect);
                                best_distance = distance;
                                best_tie = tie;
                                return col_dist == 0.0;
                            }
                        }
                    }
                }
                false
            };
            if stop_after(target.1) {
                continue;
            }
            for c in first_col..end_col {
                if stop_after((self.coverage.col + c) as f32 + margin) {
                    break;
                }
            }
        }
        best
    }
}

/// One row fetch and one cursor per sidecar instead of a search per cell.
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
    #[inline]
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
    #[inline]
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
    #[inline]
    fn new(input: &'a RenderInput, row: usize) -> Self {
        let bg = match input.default_bg_spans.get(row).map(Vec::as_slice) {
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

    #[inline]
    fn default_bg(&mut self, col: usize) -> u32 {
        let input = self.input;
        match &mut self.bg {
            BgRow::Uniform(bg) => *bg,
            BgRow::Spans { spans, cursor } => {
                while spans.get(*cursor).is_some_and(|s| s.end_col <= col) {
                    *cursor += 1;
                }
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

    #[inline]
    fn line_size(&mut self, col: usize) -> LineSize {
        let input = self.input;
        match &mut self.line {
            LineRow::Uniform(size) => *size,
            LineRow::Runs { spans, cursor } => {
                while spans.get(*cursor).is_some_and(|s| s.start_col <= col) {
                    *cursor += 1;
                }
                match cursor.checked_sub(1).map(|i| &spans[i]) {
                    Some(s) if s.start_col == col || col < s.end_col => s.line_size,
                    _ => LineSize::SingleWidth,
                }
            }
            LineRow::Unordered => input.line_size_run_at(self.row, col).0,
        }
    }

    #[inline]
    fn cluster(&mut self, col: usize) -> Option<&'a str> {
        self.clusters.get(col).map(AsRef::as_ref)
    }

    #[inline]
    fn combining(&mut self, col: usize) -> Option<&'a [char]> {
        self.combining.get(col).map(AsRef::as_ref)
    }

    #[inline]
    fn image(&mut self, col: usize) -> Option<&'a ImageRef> {
        aterm_core::render::images_in_row_at(self.images, col)
            .last()
            .map(|(_, image)| image)
    }
}

#[inline]
const fn is_single_braille_dot(ch: char) -> bool {
    let code = ch as u32;
    code >= 0x2800 && code <= 0x28ff && (code - 0x2800).count_ones() == 1
}

/// The pane-local columns `[lo, hi)` of row `row` whose cells overlap
/// `rect` (the open intersection of the cell's unit square and the rect),
/// up to `limit`, or `None` when the rect misses the row.
#[inline]
const fn exclusion_span(rect: PetRect, row: usize, limit: usize) -> Option<(usize, usize)> {
    let row_f = row as f32;
    if !(rect.row < row_f + 1.0 && rect.row + rect.rows > row_f) {
        return None;
    }
    let rect_right = rect.col + rect.cols;
    let (mut a, mut b) = (0, limit);
    while a < b {
        let mid = a + (b - a) / 2;
        if rect.col < mid as f32 + 1.0 {
            b = mid;
        } else {
            a = mid + 1;
        }
    }
    let lo = a;
    let (mut a, mut b) = (0, limit);
    while a < b {
        let mid = a + (b - a) / 2;
        if rect_right > mid as f32 {
            a = mid + 1;
        } else {
            b = mid;
        }
    }
    let hi = a;
    if lo < hi { Some((lo, hi)) } else { None }
}

#[cfg(test)]
fn intersects_cell(rect: PetRect, row: usize, col: usize) -> bool {
    rect.row < row as f32 + 1.0
        && rect.row + rect.rows > row as f32
        && rect.col < col as f32 + 1.0
        && rect.col + rect.cols > col as f32
}

#[inline]
fn selection_target(input: &RenderInput, pane: PetPane) -> Option<(f32, f32)> {
    let max_col = u16::try_from(input.cols.saturating_sub(1)).unwrap_or(u16::MAX);
    let default_clip = SelectionClip::new(
        pane.row,
        pane.row + pane.rows,
        pane.col,
        pane.col + pane.cols,
    );
    let project = |selection: &TextSelection, offset: i32, clip: Option<SelectionClip>| {
        let bounds = selection.project_range(max_col)?;
        let clip = clip.unwrap_or(default_clip);
        let first = i64::from(bounds.start_row) + i64::from(offset);
        let last = i64::from(bounds.end_row) + i64::from(offset);
        let row_start = first.max(pane.row.max(clip.row_start) as i64);
        let row_end = last.min((pane.row + pane.rows).min(clip.row_end) as i64 - 1);
        let col_start = pane.col.max(clip.col_start);
        let col_end = (pane.col + pane.cols).min(clip.col_end);
        if row_start > row_end || col_start >= col_end {
            return None;
        }
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
    /// three rows and the gap is refused.
    #[test]
    fn nearest_clear_perch_fits_two_blank_rows_and_never_counts_ink() {
        let words = "x".repeat(40);
        let mut term = Terminal::new(12, 40);
        term.process(format!("\x1b[3;1H{words}\x1b[6;1H{words}\x1b[1;1H").as_bytes());
        let world = observe(&mut term);
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
    /// exclusion per cell.
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
    /// exhaustive scan finds.
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
                        let from = from.unwrap();
                        let feet = |r: PetRect| r.row + r.rows;
                        if (feet(seat) - feet(from)).abs() >= 0.5 {
                            return true;
                        }
                        let (a0, b0, a1, b1) = cells(from.union(seat), 0.0).unwrap();
                        (a0..a1).all(|r| (b0..b1).all(|c| !lasting(r, c)))
                    })
            };
            for reach in [3.0, 9.0, 64.0] {
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
    /// eight rows up ends at 31.
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
        assert!(world.clear(PetRect::new(2.3, 11.0, 1.7, 6.0), 0.2));
        assert!(!world.clear(PetRect::new(2.3, 4.0, 1.7, 6.0), 0.2));
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

    /// Outside the caret-centred coverage window is unknown: `clearance` says `None` where
    /// `clear` says `false`.
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
        assert!(!world.clear(outside, 0.0));
        assert!(world.clear(inside, 0.0));
        let mut retired = PetWorld::default();
        assert_eq!(retired.clearance(inside, 0.0), None);
        assert_eq!(retired.clearance_past_caret(inside, 0.0), None);
        retired.retire();
        assert_eq!(retired.clearance(inside, 0.0), None);
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

    /// For a body straddling the coverage window, unseen cells are not evidence but a protected
    /// cell the map covers is.
    #[test]
    fn a_protected_cell_the_map_sees_vetoes_a_body_half_outside_it() {
        let mut term = Terminal::new(100, 300);
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
        let ring = PetRect::new(1.0, 9.0, 3.0, 3.0);
        for (r, c) in [(1, 10), (3, 10), (2, 9), (2, 11), (1, 9), (3, 11)] {
            assert_eq!(world.cell(r, c), PetCell::Protected, "ring {r},{c}");
        }
        assert_eq!(world.cell(2, 13), PetCell::Clear, "outside the ring");
        assert_eq!(world.clearance(ring, 0.0), Some(false));
        assert_eq!(world.clearance_past_caret(ring, 0.0), Some(true));
        assert_eq!(world.ink_clearance(ring, 0.0), Some(true));
        assert_eq!(
            world.clearance_past_caret(PetRect::new(0.0, 8.0, 5.0, 5.0), 0.0),
            Some(true)
        );
        term.process(b"\x1b[5;21HX\x1b[5;21H");
        let world = observe(&mut term);
        let on_glyph = PetRect::new(4.0, 20.0, 1.0, 1.0);
        assert_eq!(world.clearance(on_glyph, 0.0), Some(false));
        assert_eq!(world.clearance_past_caret(on_glyph, 0.0), Some(false));
        assert_eq!(world.ink_clearance(on_glyph, 0.0), Some(false));
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
        for (row, inked) in [(2, true), (3, true), (5, true), (7, true), (9, false)] {
            assert_eq!(
                world.ink_clearance(PetRect::new(row as f32, 10.0, 1.0, 1.0), 0.0),
                Some(!inked),
                "row {row}"
            );
        }
        let blank = PetRect::new(9.0, 10.0, 1.0, 1.0);
        assert!(world.observe_with_exclusions(&input, &facts, PetPane::full(&input), &[blank]));
        assert_eq!(world.ink_clearance(blank, 0.0), Some(false));
    }

    /// The person's typed glyphs the host paints ahead of the echo (the
    /// predicted echo, the IME preedit) are ordinary ink in every plane but
    /// the glyph plane: a resident may stand under them and is drawn beneath
    /// them, nothing steps aside for them, and the strict placement reading
    /// still refuses them.
    #[test]
    fn typed_ink_reserve_is_ink_not_a_protected_surface() {
        let mut term = Terminal::new(12, 40);
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
        let mut large_term = Terminal::new(100, 300);
        large_term.process(b"\x1b[?25l");
        let large_world = observe(&mut large_term);
        assert!(large_world.coverage().row > 0 && large_world.coverage().col > 0);
        for world in [&world, &large_world] {
            let coverage = world.coverage();
            let body = PetRect::new(
                coverage.row as f32 + 3.0,
                coverage.col as f32 + 10.0,
                2.0,
                6.0,
            );
            let target = (body.foot_row(), body.col);
            assert_eq!(world.perch_near(target, body, 0.0, 8.0), Some(body));
            assert_eq!(world.nearest_under_text_perch(body, 0.0, 8.0), Some(body));
            let tall = PetRect {
                rows: f32::MAX,
                ..body
            };
            assert!(tall.valid());
            assert_eq!(
                world.perch_near((-f32::MAX, body.col), tall, 0.0, 8.0),
                None
            );
            assert_eq!(
                world.perch_near(
                    target,
                    PetRect {
                        cols: f32::MAX,
                        ..body
                    },
                    0.0,
                    8.0
                ),
                None
            );
        }
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
    /// not name it.
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
    /// 1.3 columns left in the same present.
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

    /// Sidecars that hit every branch the row cursors must reproduce: absent, empty, ordered,
    /// malformed, gapped, zero-width and unordered rows.
    fn adversarial_sidecars(input: &mut RenderInput) {
        let cols = input.cols;
        input.default_bg_spans = vec![
            Vec::new(),
            vec![
                DefaultBgSpan::new(0, 20, 0x0011_2233),
                DefaultBgSpan::new(20, cols, 0x0044_5566),
            ],
            vec![
                DefaultBgSpan::new(0, 19, 0x0077_8899),
                DefaultBgSpan::new(20, cols, 0x00AA_BBCC),
            ],
            vec![
                DefaultBgSpan::new(0, 0, 0x00AA_AAAA),
                DefaultBgSpan::new(3, 2, 0x00BB_BBBB),
                DefaultBgSpan::new(10, 20, 0x00CC_CCCC),
                DefaultBgSpan::new(30, cols + 60, 0x00DD_DDDD),
            ],
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
            Vec::new(),
            vec![
                LineSizeSpan::new(0, 20, LineSize::DoubleWidth),
                LineSizeSpan::new(20, cols, LineSize::SingleWidth),
            ],
            vec![
                LineSizeSpan::new(5, 10, LineSize::DoubleHeightTop),
                LineSizeSpan::new(20, 25, LineSize::DoubleWidth),
            ],
            vec![LineSizeSpan::new(10, 10, LineSize::DoubleWidth)],
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
            vec![(5, "x".into()), (5, "y".into())],
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

    /// Through `observe`: a pane-local default background decides Ink from Clear column by
    /// column, on both sides of the seam at 20.
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
        assert_eq!(world.cell(5, 0), PetCell::Ink);
        assert_eq!(world.cell(5, 19), PetCell::Ink);
        assert_eq!(world.cell(5, 20), PetCell::Clear);
        assert_eq!(world.cell(5, 39), PetCell::Clear);
    }

    /// The walk skips accessors (`cluster`/`combining` short-circuit, `image` is skipped when
    /// `selected || excluded`), so the cursors must survive sparse ascending query orders.
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

    /// The implicit sparse-tail cell follows a mid-row default-background seam; a stale one
    /// would paint the second pane's blank tail as Ink.
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

    #[test]
    fn block_anchor_frontier_picks_last_glyph_and_perch_near_rejects_unobserved_world() {
        let mut term = Terminal::new(10, 40);
        term.process(b"\x1b[?25l\x1b[2;4Hfirst\x1b[4;10Hsecond\x1b[4;20Htail\x1b[7;5Houtside");
        let (input, mut facts) = snapshot(&mut term);
        let top = facts.stamp.top_absolute_row;
        facts.blocks[0] = Some(PetBlock {
            id: 1,
            state: BlockState::Complete,
            start_row: top + 1,
            end_row: top + 5,
            output_row: Some(top + 1),
            exit_code: Some(0),
        });
        facts.blocks[1] = Some(PetBlock {
            id: 2,
            state: BlockState::Executing,
            start_row: top + 7,
            end_row: top + 9,
            output_row: None,
            exit_code: None,
        });
        let mut world = PetWorld::default();
        let body = PetRect::new(0.0, 2.0, 1.7, 5.0);
        assert_eq!(world.nearest_perch(body, 0.1, 8.0), None);
        assert!(world.observe(&input, &facts, PetPane::full(&input)));
        let a0 = world.anchors()[0].expect("populated block anchor");
        assert_eq!((a0.absolute_row, a0.row, a0.col), (top + 3, 3.0, 23.0));
        let a1 = world.anchors()[1].expect("blank block anchor");
        assert_eq!((a1.absolute_row, a1.row, a1.col), (top + 7, 7.0, 0.0));
        let perch = world
            .nearest_perch(body, 0.1, 8.0)
            .expect("perch inside coverage");
        assert!(world.clear(perch, 0.1));
    }

    #[test]
    fn default_world_allocates_buffers_lazily_on_demand() {
        let mut world = PetWorld::default();
        assert_eq!(world.cells.capacity(), 0);
        assert_eq!(world.glyphs.capacity(), 0);
        assert_eq!(world.letters.capacity(), 0);
        assert_eq!(world.blocked_prefix.capacity(), 0);
        assert_eq!(world.protected_prefix.capacity(), 0);
        assert_eq!(world.inked.capacity(), 0);
        assert_eq!(world.hull.capacity(), 0);
        assert_eq!(world.ground_cells.capacity(), 0);
        assert_eq!(world.ground_blocked_prefix.capacity(), 0);
        assert_eq!(world.ground_protected_prefix.capacity(), 0);

        let mut term = Terminal::new(10, 40);
        let (input, mut stale_facts) = snapshot(&mut term);
        stale_facts.stamp.content_seq = stale_facts.stamp.content_seq.wrapping_add(1);
        assert!(!world.observe(&input, &stale_facts, PetPane::full(&input)));
        assert_eq!(
            world.cells.capacity(),
            0,
            "incoherent snapshot must not allocate buffers"
        );

        let (input, facts) = snapshot(&mut term);
        assert!(world.observe(&input, &facts, PetPane::full(&input)));
        assert!(world.cells.len() == MAX_WORLD_CELLS);
        assert_eq!(
            world.ground_cells.capacity(),
            0,
            "ground plane stays unallocated without selection or transient overlay"
        );
        assert_eq!(world.ground_blocked_prefix.capacity(), 0);
        assert_eq!(world.ground_protected_prefix.capacity(), 0);

        let overlay = [PetRect::new(1.0, 2.0, 2.0, 4.0)];
        assert!(world.observe_with_overlays(&input, &facts, PetPane::full(&input), &[], &overlay));
        assert_eq!(world.ground_cells.len(), MAX_WORLD_CELLS);
        assert_eq!(
            world.ground_blocked_prefix.len(),
            (MAX_WORLD_ROWS + 1) * (MAX_WORLD_COLS + 1)
        );
        assert_eq!(
            world.ground_protected_prefix.len(),
            (MAX_WORLD_ROWS + 1) * (MAX_WORLD_COLS + 1)
        );
        let cells_ptr = world.cells.as_ptr();
        let ground_ptr = world.ground_blocked_prefix.as_ptr();
        world.retire();
        assert_eq!(world.cells.as_ptr(), cells_ptr);
        assert_eq!(world.ground_blocked_prefix.as_ptr(), ground_ptr);
    }
}
