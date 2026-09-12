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
use aterm_core::render::{RenderInput, SelectionClip};
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

/// One map-local cell the LIVE CARET'S 3x3 RING protects and nothing else
/// does.
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

/// A fixed-budget occupancy window and small, text-free observation set.
#[derive(Clone, Debug)]
pub struct PetWorld {
    cells: Vec<PetCell>,
    glyphs: Vec<Option<bool>>,
    blocked_prefix: Vec<u16>,
    protected_prefix: Vec<u16>,
    coverage: PetPane,
    /// Map-local cells the LIVE CARET'S 3x3 RING protects and nothing else
    /// does — at most nine. Kept apart so the two visibility readings can
    /// forgive exactly the right subset and nothing more:
    /// [`PetWorld::clearance_past_caret`] forgives only the blank ones (a
    /// ring cell that also carries a glyph, a selection, an image or a
    /// non-default background is not forgiven and stays blocked), while
    /// [`PetWorld::under_text_clearance_past_caret`] — the reading that
    /// already admits ordinary ink — may forgive the whole ring.
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
        self.caret_cells = [None; 9];
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
        self.retire();
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
            || input.scroll_frac_px != 0
            || input.selections.len() > MAX_REGIONS
            || exclusions.len() > MAX_REGIONS
            || exclusions.iter().any(|r| !r.valid())
        {
            return false;
        }
        self.stamp = Some(facts.stamp);
        self.progress = facts.progress;
        self.selection_target = selection_target(input, pane);
        let center = if input.display_offset != 0 || !input.cursor_visible {
            self.selection_target
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
        self.cells.fill(PetCell::Unknown);
        self.glyphs.fill(None);
        for r in 0..rows {
            let local_r = self.coverage.row + r;
            let frame_r = pane.row + local_r;
            let Some(row) = input.cells.get(frame_r) else {
                continue;
            };
            for c in 0..cols {
                let local_c = self.coverage.col + c;
                let frame_c = pane.col + local_c;
                // A present RenderInput row is authoritative even when its
                // materialized prefix is empty: its absent tail is Cell::EMPTY
                // under the live default colors, by the renderer's sparse-row
                // contract. Images and selection can still cover that tail.
                // A missing ROW above remains unknown, as does map overflow.
                let default_bg = input.default_bg_at(frame_r, frame_c);
                let implicit = RenderCell {
                    bg: [
                        (default_bg >> 16) as u8,
                        (default_bg >> 8) as u8,
                        default_bg as u8,
                    ],
                    ..RenderCell::default()
                };
                let cell = row.get(frame_c).unwrap_or(&implicit);
                self.examined_cells += 1;
                let selected = input.selection_contains_cell(
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
                // THE CARET'S 3x3 PROTECTION RING. It stays exactly as
                // shipped for PLACEMENT — measured, it is what keeps the pet
                // from parking in the first columns of the line, where every
                // line of output lands — but see `caret_cells` below: it is
                // a ring around the only place the escort is ever seated, so
                // for VISIBILITY it may not stand in for ink.
                let cursor = input.cursor_visible
                    && input.display_offset == 0
                    && frame_r.abs_diff(input.cursor_row) <= 1
                    && frame_c.abs_diff(input.cursor_col) <= 1;
                let excluded = exclusions
                    .iter()
                    .any(|rect| intersects_cell(*rect, local_r, local_c));
                let bg = (u32::from(cell.bg[0]) << 16)
                    | (u32::from(cell.bg[1]) << 8)
                    | u32::from(cell.bg[2]);
                let decorated =
                    cell.underline != UnderlineStyle::None || cell.strikethrough || cell.overline;
                let compound = input.cluster_at(frame_r, frame_c).is_some()
                    || input.combining_at(frame_r, frame_c).is_some();
                // Nonstandard line geometry is conservatively all protected.
                // A logical single cell is not a proof about its expanded pixels.
                let line = input.line_size_run_at(frame_r, frame_c).0;
                let glyph = cell.ch != ' ' || cell.wide || decorated || compound;
                // Occupancy and protection are independent facts. Moving the
                // caret/selection off a glyph must not look like arriving ink.
                // Expanded rows lack a certified cell-to-pixel projection.
                self.glyphs[r * MAX_WORLD_COLS + c] =
                    (line == LineSize::SingleWidth).then_some(glyph);
                let others = selected
                    || excluded
                    || input.image_at(frame_r, frame_c).is_some()
                    || line != LineSize::SingleWidth;
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
                        blank: !glyph && bg == default_bg,
                    });
                }
                let class = if others || cursor {
                    PetCell::Protected
                } else if glyph || bg != default_bg {
                    PetCell::Ink
                } else {
                    PetCell::Clear
                };
                self.cells[r * MAX_WORLD_COLS + c] = class;
            }
        }
        // A rectangle costs four reads regardless of sprite size. Counts fit
        // u16 because the complete inspected window has at most 16,384 cells.
        let stride = MAX_WORLD_COLS + 1;
        // Only the top and left borders need resetting. Every interior cell
        // in the current scan is overwritten below; rectangle queries cannot
        // reach the retained cells outside coverage. A small pane should not
        // clear two maximum-sized tables on every presentation.
        self.blocked_prefix[..=cols].fill(0);
        self.protected_prefix[..=cols].fill(0);
        for r in 0..rows {
            self.blocked_prefix[(r + 1) * stride] = 0;
            self.protected_prefix[(r + 1) * stride] = 0;
            for c in 0..cols {
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
                        if self.glyphs[r * MAX_WORLD_COLS + c] == Some(true) {
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
    /// FAIL-CLOSED, and deliberately two-valued: a rectangle this map never
    /// examined answers `false`, the same byte as a genuinely obstructed one.
    /// That is the correct answer for PLACEMENT — never stand where you
    /// cannot see — and the WRONG one for VISIBILITY. [`Self::clearance`] is
    /// the three-valued answer, and v0.81.0's blinking pet was exactly this
    /// `false` being read as "there is ink there" by a per-frame alpha veto:
    /// a bound on what the code knows, returned as a fact about the world.
    #[must_use]
    pub fn clear(&self, rect: PetRect, margin: f32) -> bool {
        self.clearance(rect, margin) == Some(true)
    }

    /// The full body may stand behind ordinary text and colored backgrounds.
    /// Selection, cursor protection, images and unknown geometry still exclude
    /// it. The renderer must put an admitted occupied body under the text.
    ///
    /// FAIL-CLOSED and two-valued for the same reason [`Self::clear`] is;
    /// [`Self::under_text_clearance`] is the three-valued answer.
    #[must_use]
    pub fn under_text_clear(&self, rect: PetRect, margin: f32) -> bool {
        self.under_text_clearance(rect, margin) == Some(true)
    }

    /// THREE-VALUED CLEARANCE — the distinction [`Self::clear`] cannot make.
    ///
    /// * `Some(true)`  — every cell the rectangle and its margin touch was
    ///   examined by this snapshot and is free.
    /// * `Some(false)` — an examined cell is occupied or protected, or the
    ///   query itself is degenerate (a non-finite rectangle or margin is a
    ///   refusal to answer, not an observation of ink).
    /// * `None`        — I CANNOT SEE THERE. There is no coherent snapshot at
    ///   all, or the rectangle leaves the observed coverage window, which is
    ///   centred on the caret and therefore SLIDES AS THE USER TYPES. This is
    ///   a bound on this map, never a claim about the terminal; a caller that
    ///   collapses it into `false` has invented ink.
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

    /// As [`Self::clearance`], except that the LIVE CARET'S OWN CELLS — and
    /// only those, and only where nothing else already protects them — do not
    /// count as obstruction.
    ///
    /// THE CARET IS NOT AN OBSTRUCTION TO ITS OWN ESCORT, but the two layers
    /// want different things from that sentence and both are right:
    ///
    ///  * PLACEMENT keeps the caret protected ([`Self::clearance`]), so the
    ///    pet never comes to REST on top of the cursor and hides the one cell
    ///    the user is watching;
    ///  * VISIBILITY uses this, so a pet merely walking PAST the caret is not
    ///    deleted for the two frames it overlaps it. Blanking a whole cat
    ///    because of a cell that carries no glyph is how v0.81.0 came to
    ///    flash with no new text under the pet at all.
    ///
    /// Only the ring cells that would be `Clear` without the caret are
    /// forgiven here: a ring cell that also carries a glyph, a selection, an
    /// image or a non-default background is NOT, and stays blocked.
    #[must_use]
    pub fn clearance_past_caret(&self, rect: PetRect, margin: f32) -> Option<bool> {
        self.clearance_in(&self.blocked_prefix, Forgive::BlankCaret, rect, margin)
    }

    /// [`Self::clearance_past_caret`] as a two-valued, fail-closed answer:
    /// occupancy for a body that is standing beside the live caret, with the
    /// caret's blank ring cells discounted and nothing else.
    #[must_use]
    pub fn clear_past_caret(&self, rect: PetRect, margin: f32) -> bool {
        self.clearance_past_caret(rect, margin) == Some(true)
    }

    /// THE VISIBILITY VERDICT the emitted sprite is judged by: a resident may
    /// stand under ordinary ink, and the caret's own ring is not an
    /// obstruction to its escort. Every caret ring cell is forgivable here,
    /// because absent the caret each one is `Ink` or `Clear` and this reading
    /// admits both; selections, images and unknown geometry still veto.
    #[must_use]
    pub fn under_text_clearance_past_caret(&self, rect: PetRect, margin: f32) -> Option<bool> {
        self.clearance_in(&self.protected_prefix, Forgive::AnyCaret, rect, margin)
    }

    fn clearance_in(
        &self,
        prefix: &[u16],
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
                    .caret_cells
                    .iter()
                    .flatten()
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

    /// Conservative swept rectangle: it can refuse a diagonal whose corners
    /// happen to be free, but cannot skip ink between sampled waypoints.
    #[must_use]
    pub fn corridor_clear(&self, from: PetRect, to: PetRect, margin: f32) -> bool {
        from.valid() && to.valid() && self.clear(from.union(to), margin)
    }

    /// The same swept-body test for a resident travelling behind text.
    #[must_use]
    pub fn under_text_corridor_clear(&self, from: PetRect, to: PetRect, margin: f32) -> bool {
        from.valid() && to.valid() && self.under_text_clear(from.union(to), margin)
    }

    /// [`Self::under_text_corridor_clear`] for a body LEAVING the caret's
    /// escort seat.
    ///
    /// A swept corridor always contains the body it departs from, and
    /// [`STATION_LEAD`] seats the escort one cell past the caret — inside
    /// the caret's 3x3 keep-off ring by construction. Read strictly, the
    /// corridor out of that seat is refused for containing the seat itself:
    /// the pet could never leave the caret to take a perch, which is a bound
    /// on the reading returned as a fact about the route. The caret is not
    /// an obstruction to its own escort here either, and a body merely
    /// passing over it is already admitted by
    /// [`Self::under_text_clearance_past_caret`], which is the reading the
    /// emitted sprite is judged by.
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

    /// Closest protected-free placement, without preferring distant blank ink.
    #[must_use]
    pub fn nearest_under_text_perch(
        &self,
        near: PetRect,
        margin: f32,
        max_distance: f32,
    ) -> Option<PetRect> {
        self.perch_near_with(
            (near.foot_row(), near.col),
            near,
            margin,
            max_distance,
            true,
        )
    }

    /// `target` is an attention location `(row, col)`, not a fabricated caret.
    /// The returned destination is clear; travel must separately pass
    /// `corridor_clear` from the current body.
    #[must_use]
    pub fn perch_near(
        &self,
        target: (f32, f32),
        body: PetRect,
        margin: f32,
        max_distance: f32,
    ) -> Option<PetRect> {
        self.perch_near_with(target, body, margin, max_distance, false)
    }

    fn perch_near_with(
        &self,
        target: (f32, f32),
        body: PetRect,
        margin: f32,
        max_distance: f32,
        under_text: bool,
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
        // The allowed neighborhood bounds the loop, not merely its answers.
        // The previous full-width pass tested up to 256 columns even for an
        // eight-column home search on a dense coding console.
        let first_col = (target.1 - max_distance - self.coverage.col as f32 - margin)
            .ceil()
            .max(0.0) as usize;
        let end_col = ((target.1 + max_distance - self.coverage.col as f32 - margin).floor() + 1.0)
            .max(0.0) as usize;
        for row in rows {
            for c in first_col..end_col.min(self.coverage.cols) {
                let col = (self.coverage.col + c) as f32 + margin;
                let rect = PetRect::new(row, col, body.rows, body.cols);
                let distance = ((row - want_row).abs() * 2.0).max((col - target.1).abs());
                if distance <= max_distance
                    && distance < best_distance
                    && if under_text {
                        self.under_text_clear(rect, margin)
                    } else {
                        self.clear(rect, margin)
                    }
                {
                    best = Some(rect);
                    best_distance = distance;
                }
            }
        }
        best
    }
}

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
    use aterm_core::grid::extra::{ImageData, ImageFormat, ImageRef};
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

    /// A BOUND ON WHAT THIS MAP KNOWS IS NOT A FACT ABOUT THE WORLD. The
    /// coverage window is centred on the caret and slides as the user types;
    /// `clear` answers the same `false` for "there is ink there" and "that is
    /// outside my window", and a visibility veto reading the second as the
    /// first is what made the pet strobe at the keystroke rate in v0.81.0.
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

    /// The caret protects its own cell from PLACEMENT and not from
    /// VISIBILITY, and it forgives exactly one cell — never a glyph, never a
    /// selection, and never a neighbour.
    #[test]
    fn the_caret_is_not_an_obstruction_to_its_own_escort() {
        let mut term = Terminal::new(10, 40);
        term.process(b"\x1b[3;11H");
        let world = observe(&mut term);
        let on_caret = PetRect::new(2.0, 10.0, 1.0, 1.0);
        assert_eq!(world.cell(2, 10), PetCell::Protected);
        assert_eq!(world.clearance(on_caret, 0.0), Some(false));
        assert_eq!(world.clearance_past_caret(on_caret, 0.0), Some(true));
        // THE WHOLE 3x3 RING: still protected, so PLACEMENT keeps its
        // distance and the pet never parks over the cursor; still forgiven,
        // so a sprite seated beside the caret is not deleted for standing in
        // the caret's own keep-off margin. The ring is nine blank cells.
        let ring = PetRect::new(1.0, 9.0, 3.0, 3.0);
        for (r, c) in [(1, 10), (3, 10), (2, 9), (2, 11), (1, 9), (3, 11)] {
            assert_eq!(world.cell(r, c), PetCell::Protected, "ring {r},{c}");
        }
        assert_eq!(world.cell(2, 13), PetCell::Clear, "outside the ring");
        assert_eq!(world.clearance(ring, 0.0), Some(false));
        assert_eq!(world.clearance_past_caret(ring, 0.0), Some(true));
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
    fn safe_endpoints_do_not_license_crossing_text() {
        let mut term = Terminal::new(10, 40);
        term.process(b"\x1b[?25l\x1b[5;17Hwall");
        let world = observe(&mut term);
        let from = PetRect::new(3.5, 3.0, 1.7, 5.0);
        let to = PetRect::new(3.5, 25.0, 1.7, 5.0);
        assert!(world.clear(from, 0.2) && world.clear(to, 0.2));
        assert!(!world.corridor_clear(from, to, 0.2));
        assert!(world.corridor_clear(from, PetRect { col: 5.0, ..from }, 0.2));
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
                }),
                cell_row: 0,
                cell_col: 0,
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
    }

    #[test]
    fn both_prefix_planes_reset_correctly_when_the_scan_shrinks_and_grows() {
        let mut world = PetWorld::default();
        for (rows, cols, ink) in [(57, 151, true), (8, 20, false), (57, 151, false)] {
            let mut term = Terminal::new(rows, cols);
            term.process(b"\x1b[?25l");
            if ink {
                term.process(&vec![b'x'; usize::from(rows) * usize::from(cols)]);
                term.text_selection_mut().start_selection(
                    0,
                    0,
                    SelectionSide::Left,
                    SelectionType::Simple,
                );
                term.text_selection_mut().update_selection(
                    i32::from(rows - 1),
                    cols - 1,
                    SelectionSide::Right,
                );
            }
            let (input, facts) = snapshot(&mut term);
            assert!(world.observe(&input, &facts, PetPane::full(&input)));
            let rect = PetRect::new(1.0, 1.0, f32::from(rows) - 2.0, f32::from(cols) - 2.0);
            assert_eq!(world.clear(rect, 0.0), !ink);
            assert_eq!(world.under_text_clear(rect, 0.0), !ink);
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
        input.cells.truncate(50);
        assert!(world.observe(&input, &facts, PetPane::full(&input)));
        assert_eq!(world.cell(50, 60), PetCell::Unknown);
        assert!(!world.clear(PetRect::new(50.0, 60.0, 2.0, 5.0), 0.0));
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
}
