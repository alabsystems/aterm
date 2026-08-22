// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Parser-free PHOSPHOR embedding adapter.
//!
//! [`ExternalRainOverlay`] lets a host that already owns a terminal emulator
//! drive aterm's [`MatrixRain`](crate::matrix_rain::MatrixRain) without feeding
//! PTY bytes through a second parser. The host snapshots its authoritative,
//! post-parse visible cells, then renders the returned [`SpriteQuad`] and
//! [`RainHalo`] slices with the versioned [`SceneAtlas`]. The terminal grid is
//! never mutated and no text, input, selection, scrollback, or clipboard path is
//! duplicated.
//!
//! The expensive occupancy/material scan is revision-gated. Animation frames
//! only run the bounded rain emitter and reuse resident output vectors. A host
//! must increment [`ExternalRainSnapshot::revision`] whenever any copied cell or
//! row flag changes, including selection changes; cursor, scroll, and
//! alternate-screen gates remain live in [`ExternalRainLiveState`] and require
//! no rescan.

use std::fmt;
use std::sync::Arc;

use aterm_core::grid::{LineSize, MAX_GRID_COLS, MAX_GRID_ROWS};
use aterm_core::terminal::{RenderCell, UnderlineStyle};
use aterm_render::{RainHalo, SceneAtlas, SpriteQuad};

use crate::matrix_rain::{
    EffectGeom, HIDDEN_CURSOR_BAND_ROWS, MAX_RAIN_ADD, MAX_RAIN_QUADS, MatrixRain, RainConfig,
    RainTickInput, RainVisibility,
};

/// Hard resident-cell ceiling for one external viewport snapshot.
///
/// 262,144 cells is over five 512x100 8K-monitor terminal viewports while
/// preventing a hostile embedder from asking the adapter to retain a full
/// 4096x4096 grid. The axis limits still match aterm's public grid limits.
pub const MAX_EXTERNAL_RAIN_CELLS: usize = 256 * 1024;
/// Maximum visible rows accepted by the external adapter.
pub const MAX_EXTERNAL_RAIN_ROWS: u16 = MAX_GRID_ROWS;
/// Maximum visible columns accepted by the external adapter.
pub const MAX_EXTERNAL_RAIN_COLS: u16 = MAX_GRID_COLS;
/// Maximum host-fed composer rows when the cursor is hidden.
///
/// This mirrors MatrixRain's pinned hidden-cursor band and bounds the emitter's
/// per-cell membership checks even if an untrusted host supplies the slice.
pub const MAX_EXTERNAL_HIDDEN_CURSOR_ROWS: usize = HIDDEN_CURSOR_BAND_ROWS;

/// The cell is painted on the terminal's semantic default background.
///
/// This is deliberately distinct from RGB equality. An explicit SGR
/// background that happens to equal the theme background must leave the cell
/// protected.
pub const EXTERNAL_CELL_DEFAULT_BACKGROUND: u32 = 1 << 0;
/// The cell belongs to a wide or multi-cell glyph. Hosts should set this on
/// both the lead and continuation cells so the literal sampler never turns
/// one half of a grapheme into standalone rain material.
pub const EXTERNAL_CELL_WIDE_CONTINUATION: u32 = 1 << 1;
/// The cell has any underline style.
pub const EXTERNAL_CELL_UNDERLINE: u32 = 1 << 2;
/// The cell has a strikethrough decoration.
pub const EXTERNAL_CELL_STRIKETHROUGH: u32 = 1 << 3;
/// The cell has an overline decoration.
pub const EXTERNAL_CELL_OVERLINE: u32 = 1 << 4;
/// The cell is inside the host terminal's current selection.
///
/// Selection is copied into the occupancy snapshot so blank selected cells and
/// nearby halo pixels remain protected. Changing it requires a new revision.
pub const EXTERNAL_CELL_SELECTED: u32 = 1 << 5;
/// The cell is covered by an inline image or another host-owned visual.
pub const EXTERNAL_CELL_INLINE_IMAGE: u32 = 1 << 6;

/// A scalar value that means "occupied, but not a single material glyph".
///
/// Hosts use this for grapheme clusters that cannot be represented by one
/// Unicode scalar. It protects the cell but is never sampled into the literal
/// rain alphabet.
pub const EXTERNAL_RAIN_OPAQUE_SCALAR: u32 = u32::MAX;
/// Four `u32` lanes per [`ExternalRainCell`] in ABI order:
/// `scalar, fg, bg, flags`.
pub const EXTERNAL_RAIN_CELL_WORDS: usize = 4;

const PROTECTED_CHAR: char = '\u{FFFC}';

/// One authoritative, already-resolved visible terminal cell.
///
/// `scalar == 0` is a blank cell, a valid Unicode scalar is literal terminal
/// output, and [`EXTERNAL_RAIN_OPAQUE_SCALAR`] is occupied but unsampleable.
/// Colors are `0x00RRGGBB`; upper bits are ignored. The adapter preserves
/// semantic background identity through [`EXTERNAL_CELL_DEFAULT_BACKGROUND`]
/// instead of inferring it from RGB.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ExternalRainCell {
    /// Literal scalar, zero for blank, or [`EXTERNAL_RAIN_OPAQUE_SCALAR`].
    pub scalar: u32,
    /// Resolved foreground `0x00RRGGBB`.
    pub fg: u32,
    /// Resolved background `0x00RRGGBB`.
    pub bg: u32,
    /// Bitwise OR of the `EXTERNAL_CELL_*` constants.
    pub flags: u32,
}

// Load-bearing for a wasm wrapper that exposes a persistent Uint32Array view
// over a Vec<ExternalRainCell>; no padding or per-cell object marshaling.
const _: () = assert!(
    std::mem::size_of::<ExternalRainCell>()
        == EXTERNAL_RAIN_CELL_WORDS * std::mem::size_of::<u32>()
);

impl ExternalRainCell {
    /// A plain empty cell on the semantic default background.
    #[must_use]
    pub const fn blank(fg: u32, bg: u32) -> Self {
        Self {
            scalar: 0,
            fg,
            bg,
            flags: EXTERNAL_CELL_DEFAULT_BACKGROUND,
        }
    }

    /// A single literal scalar with resolved colors.
    #[must_use]
    pub const fn glyph(ch: char, fg: u32, bg: u32, default_background: bool) -> Self {
        Self {
            scalar: ch as u32,
            fg,
            bg,
            flags: if default_background {
                EXTERNAL_CELL_DEFAULT_BACKGROUND
            } else {
                0
            },
        }
    }

    /// Add semantic/style flags.
    #[must_use]
    pub const fn with_flags(mut self, flags: u32) -> Self {
        self.flags |= flags;
        self
    }
}

impl Default for ExternalRainCell {
    fn default() -> Self {
        Self::blank(0x00D0_D0D0, 0x0011_1318)
    }
}

/// Authoritative visible-grid snapshot from the host terminal.
///
/// `cells` is exactly `rows * cols` entries in row-major order. An empty
/// `single_width_rows` means every row is single-width; otherwise it contains
/// exactly `rows` booleans. Hosts with no DEC double-width row concept use the
/// empty fast path.
#[derive(Clone, Copy, Debug)]
pub struct ExternalRainSnapshot<'a> {
    /// Visible rows.
    pub rows: u16,
    /// Visible columns.
    pub cols: u16,
    /// Monotonic host revision for cells, selection, and row attributes.
    pub revision: u64,
    /// Monotonic terminal-content sequence used by the weather machine.
    pub content_seq: u64,
    /// Theme default background `0x00RRGGBB`.
    pub default_bg: u32,
    /// Theme foreground `0x00RRGGBB`.
    pub theme_fg: u32,
    /// Full visible row-major cells.
    pub cells: &'a [ExternalRainCell],
    /// Empty means all rows are single-width.
    pub single_width_rows: &'a [bool],
}

/// Per-frame gates that do not require a cell rescan.
#[derive(Clone, Copy, Debug, Default)]
pub struct ExternalRainLiveState<'a> {
    /// Visible cursor `(row, col)`, or `None` when DECTCEM hides it.
    pub cursor: Option<(u16, u16)>,
    /// Recently damaged composer rows when the cursor is hidden.
    pub hidden_cursor_rows: &'a [u16],
    /// Host scrollback distance from the live bottom; nonzero suppresses rain.
    pub display_offset: i32,
    /// Whether the alternate screen is active.
    pub is_alt_screen: bool,
}

/// Device-pixel cell geometry for one emitted frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ExternalRainGeometry {
    /// Cell width in device pixels.
    pub cell_w: u16,
    /// Cell height in device pixels.
    pub cell_h: u16,
}

/// Result of synchronizing an authoritative host snapshot.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExternalRainSync {
    /// Cells and material sampling gates were unchanged; no scan ran.
    Unchanged,
    /// Occupancy was unchanged but cursor/composer movement resampled literals.
    Resampled,
    /// Cells/selection/theme/geometry changed and occupancy was rebuilt.
    Rescanned,
    /// The engine is disabled, reduced-motion, hidden, scrolled back, or fully drained. The
    /// host should offer a fresh snapshot when that gate lifts.
    Deferred,
}

/// Fail-closed snapshot/geometry validation error.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ExternalRainError {
    /// A terminal viewport must have at least one row and column.
    EmptyViewport,
    /// An axis exceeds aterm's public 4096-cell grid bound.
    AxisTooLarge { rows: u16, cols: u16 },
    /// The viewport product exceeds [`MAX_EXTERNAL_RAIN_CELLS`].
    TooManyCells { cells: usize, max: usize },
    /// The row-major input is not exactly `rows * cols` cells.
    CellCount { expected: usize, actual: usize },
    /// Non-empty row metadata is not exactly `rows` entries.
    RowMetadataCount { expected: usize, actual: usize },
    /// Hidden-cursor row input exceeds the engine's bounded composer band.
    HiddenCursorRows { actual: usize, max: usize },
    /// A claimed composer row lies outside the visible authoritative snapshot.
    HiddenCursorRowOutOfRange { row: u16, rows: u16 },
    /// Zero or u16-overflowing device-pixel geometry cannot be represented by
    /// the existing render-boundary quad contract.
    InvalidPixelGeometry {
        rows: u16,
        cols: u16,
        cell_w: u16,
        cell_h: u16,
    },
}

impl fmt::Display for ExternalRainError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyViewport => f.write_str("external rain viewport is empty"),
            Self::AxisTooLarge { rows, cols } => {
                write!(
                    f,
                    "external rain viewport {cols}x{rows} exceeds axis limits"
                )
            }
            Self::TooManyCells { cells, max } => {
                write!(f, "external rain viewport has {cells} cells (max {max})")
            }
            Self::CellCount { expected, actual } => write!(
                f,
                "external rain cell count is {actual}, expected {expected}"
            ),
            Self::RowMetadataCount { expected, actual } => write!(
                f,
                "external rain row metadata count is {actual}, expected {expected}"
            ),
            Self::HiddenCursorRows { actual, max } => write!(
                f,
                "external rain hidden-cursor band has {actual} rows (max {max})"
            ),
            Self::HiddenCursorRowOutOfRange { row, rows } => write!(
                f,
                "external rain hidden-cursor row {row} is outside {rows} visible rows"
            ),
            Self::InvalidPixelGeometry {
                rows,
                cols,
                cell_w,
                cell_h,
            } => write!(
                f,
                "external rain geometry {cols}x{rows} cells at {cell_w}x{cell_h}px is invalid"
            ),
        }
    }
}

impl std::error::Error for ExternalRainError {}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct SnapshotIdentity {
    revision: u64,
    rows: u16,
    cols: u16,
    default_bg: u32,
    theme_fg: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct SamplingKey {
    cursor_row: Option<u16>,
    hidden_rows_hash: u64,
    hidden_rows_len: usize,
    live_view: bool,
}

/// Effects-only Matrix rain adapter for an external authoritative terminal.
///
/// Construct this lazily when rain is enabled. [`Self::sync_snapshot`] owns the
/// only O(rows x cols) work and is revision-gated; [`Self::emit`] is the
/// animation hot path. Output slices and the atlas remain valid until the next
/// mutable call.
pub struct ExternalRainOverlay {
    rain: MatrixRain,
    config: RainConfig,
    cells: Vec<Vec<RenderCell>>,
    line_sizes: Vec<LineSize>,
    dimensions: Option<(u16, u16)>,
    identity: Option<SnapshotIdentity>,
    sampling_key: Option<SamplingKey>,
    scan_epoch: u64,
    snapshot_ready: bool,
    visibility: RainVisibility,
    reduced_motion: bool,
    quads: Vec<SpriteQuad>,
    halos: Vec<RainHalo>,
    atlas: Option<Arc<SceneAtlas>>,
    fingerprint: u64,
}

impl ExternalRainOverlay {
    /// Create a parser-free adapter. The config is defensively clamped by
    /// [`MatrixRain::new`]; an embedder normally passes `enabled = true` and
    /// constructs/drops this adapter with its feature toggle.
    #[must_use]
    pub fn new(config: RainConfig) -> Self {
        Self {
            rain: MatrixRain::new(config),
            config,
            cells: Vec::new(),
            line_sizes: Vec::new(),
            dimensions: None,
            identity: None,
            sampling_key: None,
            scan_epoch: 0,
            snapshot_ready: false,
            visibility: RainVisibility::Focused,
            reduced_motion: false,
            quads: Vec::with_capacity(MAX_RAIN_QUADS),
            halos: Vec::with_capacity(MAX_RAIN_ADD),
            atlas: None,
            fingerprint: 0,
        }
    }

    /// Replace rain configuration without replacing the adapter. A default-bg
    /// change invalidates occupancy; an output-material change invalidates the
    /// sampled alphabet. Other knobs take effect without a cell scan.
    pub fn set_config(&mut self, config: RainConfig) {
        let default_bg_changed = self.config.default_bg != config.default_bg;
        let material_changed = self.config.output_material != config.output_material;
        let disabled = !config.enabled;
        self.config = config;
        self.rain.set_config(config);
        if default_bg_changed || disabled {
            self.invalidate_snapshot();
        } else if material_changed {
            self.sampling_key = None;
        }
        if disabled {
            self.clear_output();
        }
    }

    /// Accessibility motion gate. Enabling it immediately scrubs stale output;
    /// after disabling it the host must offer a fresh snapshot.
    pub fn set_reduced_motion(&mut self, reduced: bool) {
        self.reduced_motion = reduced;
        self.rain.set_reduced_motion(reduced);
        if reduced {
            self.invalidate_snapshot();
            self.clear_output();
        }
    }

    /// Focus/visibility drain gate. Hidden output is scrubbed immediately; a
    /// snapshot offered while emission is impossible returns
    /// [`ExternalRainSync::Deferred`].
    pub fn set_visibility(&mut self, visibility: RainVisibility) {
        self.visibility = visibility;
        self.rain.set_visibility(visibility);
        if visibility == RainVisibility::Hidden {
            self.invalidate_snapshot();
            self.clear_output();
        }
    }

    /// Advance the injected host clock. Large gaps use MatrixRain's bounded
    /// arithmetic catch-up path.
    pub fn advance_ms(&mut self, dt_ms: u64) {
        self.rain.advance_ms(dt_ms);
    }

    /// Note a terminal content-sequence observation without forcing a cell
    /// rescan. [`Self::sync_snapshot`] calls this automatically.
    pub fn note_activity(&mut self, content_seq: u64) {
        self.rain.note_activity(content_seq);
    }

    /// Note one user keystroke.
    pub fn note_keystroke(&mut self) {
        self.rain.note_keystroke();
    }

    /// Note a visual bell.
    pub fn note_bell(&mut self) {
        self.rain.note_bell();
    }

    /// Note a host-observed command completion.
    pub fn note_exit_status(&mut self, failed: bool) {
        self.rain.note_exit_status(failed);
    }

    /// Note wheel/PgUp activity in an alternate-screen TUI.
    pub fn note_alt_scroll(&mut self) {
        self.rain.note_alt_scroll();
    }

    /// Copy and scan an authoritative post-parse visible frame.
    ///
    /// Validation and every gate are fail-closed. When this returns
    /// [`ExternalRainSync::Deferred`], no O(rows x cols) copy/scan ran and the
    /// host should call again after focus/reduced-motion/enablement changes.
    pub fn sync_snapshot(
        &mut self,
        snapshot: ExternalRainSnapshot<'_>,
        live: ExternalRainLiveState<'_>,
    ) -> Result<ExternalRainSync, ExternalRainError> {
        self.validate_snapshot(snapshot)?;
        self.validate_live(live, snapshot.rows)?;
        self.rain.note_activity(snapshot.content_seq);

        let default_bg = snapshot.default_bg & 0x00FF_FFFF;
        let theme_fg = snapshot.theme_fg & 0x00FF_FFFF;
        if self.config.default_bg != default_bg || self.config.theme_fg != theme_fg {
            let mut config = self.config;
            config.default_bg = default_bg;
            config.theme_fg = theme_fg;
            self.set_config(config);
        }

        if live.display_offset != 0 {
            // A translated scrollback viewport cannot share live cursor-band
            // coordinates. Skip the guaranteed-empty O(rows*cols) copy/scan
            // and require a fresh authoritative snapshot on return to live.
            self.rain.defer_reading();
            self.invalidate_snapshot();
            self.clear_output();
            return Ok(ExternalRainSync::Deferred);
        }

        let identity = SnapshotIdentity {
            revision: snapshot.revision,
            rows: snapshot.rows,
            cols: snapshot.cols,
            default_bg,
            theme_fg,
        };
        let sampling_key = sampling_key(live, snapshot.rows, snapshot.cols);
        let needs_rescan = self.identity != Some(identity) || !self.snapshot_ready;
        let needs_resample = self.sampling_key != Some(sampling_key);
        if !needs_rescan && !needs_resample {
            return Ok(ExternalRainSync::Unchanged);
        }

        if !self.rain.can_emit() {
            self.invalidate_snapshot();
            self.clear_output();
            return Ok(ExternalRainSync::Deferred);
        }

        // A new occupancy or material-generation contract cannot present the
        // previous frame. The host emits immediately after sync; until then the
        // public channels stay empty rather than pairing stale quads/atlas with
        // fresh xterm cells.
        self.clear_output();
        let mut result = ExternalRainSync::Resampled;
        if needs_rescan {
            self.prepare_storage(snapshot.rows, snapshot.cols);
            let cols = usize::from(snapshot.cols);
            let default_bg_rgb = rgb(default_bg);
            for (row_index, row) in self
                .cells
                .iter_mut()
                .enumerate()
                .take(usize::from(snapshot.rows))
            {
                row.clear();
                let start = row_index * cols;
                for cell in &snapshot.cells[start..start + cols] {
                    row.push(to_render_cell(*cell, default_bg_rgb));
                }
                self.line_sizes[row_index] = if snapshot.single_width_rows.is_empty()
                    || snapshot.single_width_rows[row_index]
                {
                    LineSize::SingleWidth
                } else {
                    LineSize::DoubleWidth
                };
            }

            self.scan_epoch = self.scan_epoch.wrapping_add(1);
            self.rain.rescan_from_cells(
                &self.cells[..usize::from(snapshot.rows)],
                &self.line_sizes[..usize::from(snapshot.rows)],
                &[],
                usize::from(snapshot.rows),
                usize::from(snapshot.cols),
                default_bg,
                self.scan_epoch,
            );
            self.identity = Some(identity);
            self.snapshot_ready = true;
            result = ExternalRainSync::Rescanned;
        }

        if live.display_offset == 0 {
            self.rain.sample_material(
                &self.cells[..usize::from(snapshot.rows)],
                usize::from(snapshot.rows),
                valid_cursor(live.cursor, snapshot.rows, snapshot.cols),
                live.hidden_cursor_rows,
            );
        }
        self.sampling_key = Some(sampling_key);
        Ok(result)
    }

    /// Emit one effects-only frame into resident, bounded vectors.
    ///
    /// The returned fingerprint is zero for an empty/gated frame and stable for
    /// unchanged output. Read the frame through [`Self::quads`],
    /// [`Self::halos`], and [`Self::atlas`].
    pub fn emit(
        &mut self,
        geometry: ExternalRainGeometry,
        live: ExternalRainLiveState<'_>,
    ) -> Result<u64, ExternalRainError> {
        let Some((rows, cols)) = self.dimensions else {
            self.clear_output();
            return Ok(0);
        };
        self.validate_live(live, rows)?;
        self.validate_geometry(rows, cols, geometry)?;
        if !self.snapshot_ready {
            self.clear_output();
            return Ok(0);
        }

        let input = RainTickInput {
            cursor: valid_cursor(live.cursor, rows, cols),
            hidden_band: live.hidden_cursor_rows,
            // Selection is encoded into the authoritative occupancy snapshot.
            sel: None,
            display_offset: live.display_offset,
            is_alt_screen: live.is_alt_screen,
        };
        self.fingerprint = self.rain.emit(
            EffectGeom {
                cell_w: geometry.cell_w,
                cell_h: geometry.cell_h,
                rows,
                cols,
            },
            &input,
            &mut self.quads,
            &mut self.halos,
        );
        self.atlas = if self.quads.is_empty() && self.halos.is_empty() {
            None
        } else {
            self.rain.rain_atlas()
        };
        Ok(self.fingerprint)
    }

    /// Current source-over rain glyph quads.
    #[must_use]
    pub fn quads(&self) -> &[SpriteQuad] {
        &self.quads
    }

    /// Current additive bright-head halos.
    #[must_use]
    pub fn halos(&self) -> &[RainHalo] {
        &self.halos
    }

    /// Current versioned glyph atlas, present only with visible output.
    #[must_use]
    pub fn atlas(&self) -> Option<&SceneAtlas> {
        self.atlas.as_deref()
    }

    /// Current frame fingerprint.
    #[must_use]
    pub fn fingerprint(&self) -> u64 {
        self.fingerprint
    }

    /// Whether the host should keep its shared animation ticker armed.
    #[must_use]
    pub fn is_active(&self) -> bool {
        self.config.enabled
            && !self.reduced_motion
            && self.visibility != RainVisibility::Hidden
            && self.rain.is_active()
    }

    /// Atlas generation for upload caching.
    #[must_use]
    pub fn atlas_version(&self) -> u64 {
        self.rain.atlas_version()
    }

    fn validate_snapshot(
        &mut self,
        snapshot: ExternalRainSnapshot<'_>,
    ) -> Result<(), ExternalRainError> {
        let fail = |this: &mut Self, error| {
            this.invalidate_snapshot();
            this.clear_output();
            Err(error)
        };
        if snapshot.rows == 0 || snapshot.cols == 0 {
            return fail(self, ExternalRainError::EmptyViewport);
        }
        if snapshot.rows > MAX_EXTERNAL_RAIN_ROWS || snapshot.cols > MAX_EXTERNAL_RAIN_COLS {
            return fail(
                self,
                ExternalRainError::AxisTooLarge {
                    rows: snapshot.rows,
                    cols: snapshot.cols,
                },
            );
        }
        let cell_count = usize::from(snapshot.rows) * usize::from(snapshot.cols);
        if cell_count > MAX_EXTERNAL_RAIN_CELLS {
            return fail(
                self,
                ExternalRainError::TooManyCells {
                    cells: cell_count,
                    max: MAX_EXTERNAL_RAIN_CELLS,
                },
            );
        }
        if snapshot.cells.len() != cell_count {
            return fail(
                self,
                ExternalRainError::CellCount {
                    expected: cell_count,
                    actual: snapshot.cells.len(),
                },
            );
        }
        if !snapshot.single_width_rows.is_empty()
            && snapshot.single_width_rows.len() != usize::from(snapshot.rows)
        {
            return fail(
                self,
                ExternalRainError::RowMetadataCount {
                    expected: usize::from(snapshot.rows),
                    actual: snapshot.single_width_rows.len(),
                },
            );
        }
        Ok(())
    }

    fn validate_geometry(
        &mut self,
        rows: u16,
        cols: u16,
        geometry: ExternalRainGeometry,
    ) -> Result<(), ExternalRainError> {
        if geometry.cell_w == 0
            || geometry.cell_h == 0
            || u32::from(cols) * u32::from(geometry.cell_w) > u32::from(u16::MAX)
            || u32::from(rows) * u32::from(geometry.cell_h) > u32::from(u16::MAX)
        {
            self.clear_output();
            return Err(ExternalRainError::InvalidPixelGeometry {
                rows,
                cols,
                cell_w: geometry.cell_w,
                cell_h: geometry.cell_h,
            });
        }
        Ok(())
    }

    fn validate_live(
        &mut self,
        live: ExternalRainLiveState<'_>,
        rows: u16,
    ) -> Result<(), ExternalRainError> {
        if live.hidden_cursor_rows.len() > MAX_EXTERNAL_HIDDEN_CURSOR_ROWS {
            self.invalidate_snapshot();
            self.clear_output();
            return Err(ExternalRainError::HiddenCursorRows {
                actual: live.hidden_cursor_rows.len(),
                max: MAX_EXTERNAL_HIDDEN_CURSOR_ROWS,
            });
        }
        if let Some(&row) = live.hidden_cursor_rows.iter().find(|&&row| row >= rows) {
            self.invalidate_snapshot();
            self.clear_output();
            return Err(ExternalRainError::HiddenCursorRowOutOfRange { row, rows });
        }
        Ok(())
    }

    fn prepare_storage(&mut self, rows: u16, cols: u16) {
        if self.dimensions != Some((rows, cols)) {
            // A resize is infrequent and starts a fresh bounded allocation.
            // Reusing inner vectors across incompatible shapes can accumulate
            // capacity from several <=MAX_CELLS aspect ratios.
            self.cells.clear();
            self.cells
                .resize_with(usize::from(rows), || Vec::with_capacity(usize::from(cols)));
            self.line_sizes.clear();
            self.line_sizes
                .resize(usize::from(rows), LineSize::SingleWidth);
            self.dimensions = Some((rows, cols));
        }
    }

    fn invalidate_snapshot(&mut self) {
        self.identity = None;
        self.sampling_key = None;
        self.snapshot_ready = false;
    }

    fn clear_output(&mut self) {
        self.quads.clear();
        self.halos.clear();
        self.atlas = None;
        self.fingerprint = 0;
    }

    #[cfg(test)]
    fn material_chars(&self) -> &[char] {
        self.rain.literal_material_chars_for_test()
    }
}

fn to_render_cell(cell: ExternalRainCell, default_bg: [u8; 3]) -> RenderCell {
    let protected = cell.flags & (EXTERNAL_CELL_SELECTED | EXTERNAL_CELL_INLINE_IMAGE) != 0;
    let mut ch = if cell.scalar == 0 {
        ' '
    } else {
        char::from_u32(cell.scalar).unwrap_or(PROTECTED_CHAR)
    };
    if protected {
        ch = PROTECTED_CHAR;
    }

    let mut bg = rgb(cell.bg);
    let semantic_default = cell.flags & EXTERNAL_CELL_DEFAULT_BACKGROUND != 0;
    if protected || !semantic_default {
        // Preserve explicit-background semantics even when its resolved RGB is
        // byte-equal to the theme default. One bit is enough because this color
        // exists only in the private occupancy snapshot.
        if bg == default_bg {
            bg[2] ^= 1;
        }
    }
    RenderCell {
        ch,
        fg: rgb(cell.fg),
        bg,
        wide: cell.flags & EXTERNAL_CELL_WIDE_CONTINUATION != 0,
        emoji_presentation: false,
        text_presentation: false,
        bold: false,
        italic: false,
        underline: if cell.flags & EXTERNAL_CELL_UNDERLINE != 0 {
            UnderlineStyle::Single
        } else {
            UnderlineStyle::None
        },
        strikethrough: cell.flags & EXTERNAL_CELL_STRIKETHROUGH != 0,
        overline: cell.flags & EXTERNAL_CELL_OVERLINE != 0,
        underline_color: None,
    }
}

const fn rgb(color: u32) -> [u8; 3] {
    [
        ((color & 0x00FF_FFFF) >> 16) as u8,
        ((color & 0x00FF_FFFF) >> 8) as u8,
        (color & 0x00FF_FFFF) as u8,
    ]
}

fn valid_cursor(cursor: Option<(u16, u16)>, rows: u16, cols: u16) -> Option<(u16, u16)> {
    cursor.filter(|(row, col)| *row < rows && *col < cols)
}

fn sampling_key(live: ExternalRainLiveState<'_>, rows: u16, cols: u16) -> SamplingKey {
    // Stable FNV-1a over a tiny (normally <=5 row) host slice; no allocation.
    let mut hash = 0xCBF2_9CE4_8422_2325u64;
    for &row in live.hidden_cursor_rows {
        hash ^= u64::from(row);
        hash = hash.wrapping_mul(0x0000_0100_0000_01B3);
    }
    SamplingKey {
        cursor_row: valid_cursor(live.cursor, rows, cols).map(|(row, _)| row),
        hidden_rows_hash: hash,
        hidden_rows_len: live.hidden_cursor_rows.len(),
        live_view: live.display_offset == 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const BG: u32 = 0x0011_1318;
    const FG: u32 = 0x00D0_D0D0;
    const GEOM: ExternalRainGeometry = ExternalRainGeometry {
        cell_w: 8,
        cell_h: 16,
    };

    fn config() -> RainConfig {
        RainConfig {
            enabled: true,
            density: 12,
            seed: 0xA7E2_11D3,
            default_bg: BG,
            theme_fg: FG,
            ..RainConfig::default()
        }
    }

    fn blank_cells(rows: u16, cols: u16) -> Vec<ExternalRainCell> {
        vec![ExternalRainCell::blank(FG, BG); usize::from(rows) * usize::from(cols)]
    }

    fn snapshot<'a>(
        rows: u16,
        cols: u16,
        revision: u64,
        content_seq: u64,
        cells: &'a [ExternalRainCell],
    ) -> ExternalRainSnapshot<'a> {
        ExternalRainSnapshot {
            rows,
            cols,
            revision,
            content_seq,
            default_bg: BG,
            theme_fg: FG,
            cells,
            single_width_rows: &[],
        }
    }

    fn wake(overlay: &mut ExternalRainOverlay, live: ExternalRainLiveState<'_>) {
        for _ in 0..12 {
            overlay.note_keystroke();
            overlay.advance_ms(83);
            overlay.emit(GEOM, live).unwrap();
        }
        assert!(
            !overlay.quads().is_empty(),
            "fixture must produce visible rain"
        );
    }

    /// Hide creates a freshness obligation: refocus alone cannot authorize an
    /// old snapshot. `Buggy=1` models the exact missing-invalidation defect.
    fn external_rain_freshness_model() -> aterm_spec::derive::Model {
        use aterm_spec::ty_model;
        ty_model! {
            ExternalRainFreshness {
                const Buggy = 0;
                var visible = 1;
                var fresh = 0;
                var must_sync = 1;
                var output = 0;
                action Sync when (visible == 1) {
                    fresh = 1;
                    must_sync = 0;
                    output = 0;
                }
                action Emit when (visible == 1) {
                    output = if fresh == 1 { 1 } else { 0 };
                }
                action Hide when (visible == 1) {
                    visible = 0;
                    fresh = if Buggy == 1 { fresh } else { 0 };
                    must_sync = 1;
                    output = 0;
                }
                action Focus when (visible == 0) {
                    visible = 1;
                    output = 0;
                }
                invariant FreshOnly: output <= 1 - must_sync;
                invariant HiddenEmpty: output <= visible;
            }
        }
    }

    #[test]
    fn external_rain_freshness_model_proves_and_catches_stale_refocus() {
        let model = external_rain_freshness_model();
        aterm_spec::verify::prove_and_catch_scalar(&model, model.name);
    }

    /// Tier-1 bind: drive the shipping adapter through the model's exact
    /// Sync -> Emit -> Hide -> Focus -> Emit -> Sync -> Emit trace.
    #[test]
    fn real_overlay_hide_refocus_trace_conforms_to_freshness_model() {
        let model = external_rain_freshness_model();
        let mut state = model.init_state();
        let (rows, cols) = (40, 60);
        let mut cells = blank_cells(rows, cols);
        cells[0] = ExternalRainCell::glyph('A', FG, BG, true);
        let live = ExternalRainLiveState {
            cursor: Some((30, 0)),
            ..ExternalRainLiveState::default()
        };
        let mut overlay = ExternalRainOverlay::new(config());

        assert!(model.fire("Sync", &mut state));
        assert_eq!(
            overlay
                .sync_snapshot(snapshot(rows, cols, 1, 1, &cells), live)
                .unwrap(),
            ExternalRainSync::Rescanned
        );
        assert!(overlay.quads().is_empty());
        wake(&mut overlay, live);
        assert!(model.fire("Emit", &mut state));
        assert_eq!(state["output"], 1);

        assert!(model.fire("Hide", &mut state));
        overlay.set_visibility(RainVisibility::Hidden);
        assert_eq!(state["fresh"], 0);
        assert!(!overlay.snapshot_ready);
        assert!(overlay.quads().is_empty() && overlay.atlas().is_none());

        assert!(model.fire("Focus", &mut state));
        overlay.set_visibility(RainVisibility::Focused);
        assert!(model.fire("Emit", &mut state));
        overlay.advance_ms(83);
        assert_eq!(overlay.emit(GEOM, live).unwrap(), 0);
        assert_eq!(state["output"], 0);
        assert!(model.check_invariant("FreshOnly", &state));

        assert!(model.fire("Sync", &mut state));
        assert_eq!(
            overlay
                .sync_snapshot(snapshot(rows, cols, 2, 2, &cells), live)
                .unwrap(),
            ExternalRainSync::Rescanned
        );
        wake(&mut overlay, live);
        assert!(model.fire("Emit", &mut state));
        assert_eq!(state["output"], 1);
        assert!(model.check_invariant("FreshOnly", &state));
        assert!(!overlay.quads().is_empty());
    }

    #[test]
    fn samples_literal_output_without_a_second_terminal_parser() {
        let (rows, cols) = (24, 80);
        let mut cells = blank_cells(rows, cols);
        for (col, ch) in "REAL42!".chars().enumerate() {
            cells[2 * usize::from(cols) + col] = ExternalRainCell::glyph(ch, FG, BG, true);
        }
        cells[3 * usize::from(cols)] =
            ExternalRainCell::glyph('Z', FG, BG, true).with_flags(EXTERNAL_CELL_SELECTED);
        let live = ExternalRainLiveState {
            cursor: Some((22, 4)),
            ..ExternalRainLiveState::default()
        };
        let mut overlay = ExternalRainOverlay::new(config());
        assert_eq!(
            overlay
                .sync_snapshot(snapshot(rows, cols, 1, 1, &cells), live)
                .unwrap(),
            ExternalRainSync::Rescanned
        );
        let material = overlay.material_chars();
        for ch in "REAL42!".chars() {
            assert!(
                material.contains(&ch),
                "missing literal {ch:?} from {material:?}"
            );
        }
        assert!(
            !material.contains(&'Z'),
            "selected text must not enter material"
        );
        assert_eq!(
            overlay
                .sync_snapshot(snapshot(rows, cols, 1, 1, &cells), live)
                .unwrap(),
            ExternalRainSync::Unchanged,
            "same authoritative revision must not rescan"
        );
    }

    #[test]
    fn masks_text_explicit_background_selection_and_cursor_band() {
        let (rows, cols) = (40, 60);
        let mut cells = blank_cells(rows, cols);
        let text_index = 10 * usize::from(cols) + 20;
        cells[text_index] = ExternalRainCell::glyph('X', FG, BG, true);
        // Explicit SGR background equal to default RGB must still be protected.
        cells[12 * usize::from(cols) + 22] = ExternalRainCell {
            flags: 0,
            ..ExternalRainCell::blank(FG, BG)
        };
        let live = ExternalRainLiveState {
            cursor: Some((30, 5)),
            ..ExternalRainLiveState::default()
        };
        let mut overlay = ExternalRainOverlay::new(config());
        overlay
            .sync_snapshot(snapshot(rows, cols, 1, 1, &cells), live)
            .unwrap();
        wake(&mut overlay, live);

        assert!(overlay.quads().iter().all(|quad| {
            let col = quad.x / GEOM.cell_w;
            !((28..=32).contains(&quad.row)
                || quad.row == 10 && col == 20
                || quad.row == 12 && col == 22)
        }));
        let text_x = 20 * GEOM.cell_w;
        let text_y = 10 * GEOM.cell_h;
        assert!(overlay.halos().iter().all(|halo| {
            halo.x.saturating_add(halo.w) <= text_x
                || halo.x >= text_x + GEOM.cell_w
                || halo.y.saturating_add(halo.h) <= text_y
                || halo.y >= text_y + GEOM.cell_h
        }));

        for cell in &mut cells {
            cell.flags |= EXTERNAL_CELL_SELECTED;
        }
        overlay
            .sync_snapshot(snapshot(rows, cols, 2, 2, &cells), live)
            .unwrap();
        overlay.note_keystroke();
        overlay.advance_ms(83);
        assert_eq!(overlay.emit(GEOM, live).unwrap(), 0);
        assert!(overlay.quads().is_empty() && overlay.halos().is_empty());
    }

    #[test]
    fn display_alt_and_visibility_gates_scrub_output() {
        let (rows, cols) = (40, 60);
        let mut cells = blank_cells(rows, cols);
        cells[0] = ExternalRainCell::glyph('A', FG, BG, true);
        let live = ExternalRainLiveState {
            cursor: Some((30, 0)),
            ..ExternalRainLiveState::default()
        };
        let mut cfg = config();
        cfg.suppress_in_alt_screen = true;
        let mut overlay = ExternalRainOverlay::new(cfg);
        overlay
            .sync_snapshot(snapshot(rows, cols, 1, 1, &cells), live)
            .unwrap();
        wake(&mut overlay, live);

        let scrolled = ExternalRainLiveState {
            display_offset: 1,
            ..live
        };
        overlay.advance_ms(83);
        assert_eq!(overlay.emit(GEOM, scrolled).unwrap(), 0);
        assert!(overlay.quads().is_empty());
        assert!(
            !overlay.is_active(),
            "reading gate must disarm empty frames"
        );
        wake(&mut overlay, live);

        let alternate = ExternalRainLiveState {
            is_alt_screen: true,
            ..live
        };
        overlay.advance_ms(83);
        assert_eq!(overlay.emit(GEOM, alternate).unwrap(), 0);
        assert!(overlay.quads().is_empty());
        wake(&mut overlay, live);

        assert!(overlay.is_active());
        overlay.set_visibility(RainVisibility::Hidden);
        assert!(overlay.quads().is_empty() && overlay.atlas().is_none());
        assert!(
            !overlay.is_active(),
            "hidden must disarm before another frame"
        );
        overlay.advance_ms(83);
        assert_eq!(overlay.emit(GEOM, live).unwrap(), 0);
        assert!(!overlay.is_active());
        assert_eq!(
            overlay
                .sync_snapshot(snapshot(rows, cols, 2, 2, &cells), live)
                .unwrap(),
            ExternalRainSync::Deferred,
            "hidden panes do no grid scan"
        );
        overlay.set_visibility(RainVisibility::Focused);
        overlay.advance_ms(83);
        assert_eq!(
            overlay.emit(GEOM, live).unwrap(),
            0,
            "refocus cannot replay stale pre-hidden occupancy"
        );
        assert!(overlay.quads().is_empty() && overlay.atlas().is_none());
        assert_eq!(
            overlay
                .sync_snapshot(snapshot(rows, cols, 2, 2, &cells), live)
                .unwrap(),
            ExternalRainSync::Rescanned,
            "a fresh authoritative snapshot rearms the refocused pane"
        );
    }

    #[test]
    fn scrolled_snapshot_defers_without_copy_or_scan_and_live_return_rescans() {
        let (rows, cols) = (40, 60);
        let mut cells = blank_cells(rows, cols);
        cells[0] = ExternalRainCell::glyph('A', FG, BG, true);
        let live = ExternalRainLiveState {
            cursor: Some((30, 0)),
            ..ExternalRainLiveState::default()
        };
        let mut overlay = ExternalRainOverlay::new(config());
        assert_eq!(
            overlay
                .sync_snapshot(snapshot(rows, cols, 1, 1, &cells), live)
                .unwrap(),
            ExternalRainSync::Rescanned
        );
        let scan_epoch = overlay.scan_epoch;
        cells[1] = ExternalRainCell::glyph('B', FG, BG, true);
        let scrolled = ExternalRainLiveState {
            display_offset: 7,
            ..live
        };
        assert_eq!(
            overlay
                .sync_snapshot(snapshot(rows, cols, 2, 2, &cells), scrolled)
                .unwrap(),
            ExternalRainSync::Deferred
        );
        assert_eq!(overlay.scan_epoch, scan_epoch, "reading mode ran no scan");
        assert!(!overlay.snapshot_ready);
        assert_eq!(
            overlay
                .sync_snapshot(snapshot(rows, cols, 2, 2, &cells), live)
                .unwrap(),
            ExternalRainSync::Rescanned,
            "returning live requires a fresh authoritative scan"
        );
        assert_eq!(overlay.scan_epoch, scan_epoch.wrapping_add(1));
    }

    #[test]
    fn storage_is_reused_and_malformed_input_fails_closed() {
        let (rows, cols) = (32, 100);
        let cells = blank_cells(rows, cols);
        let live = ExternalRainLiveState {
            cursor: Some((20, 0)),
            ..ExternalRainLiveState::default()
        };
        let mut overlay = ExternalRainOverlay::new(config());
        overlay
            .sync_snapshot(snapshot(rows, cols, 1, 1, &cells), live)
            .unwrap();
        let row_ptrs: Vec<*const RenderCell> = overlay.cells.iter().map(Vec::as_ptr).collect();
        let row_caps: Vec<usize> = overlay.cells.iter().map(Vec::capacity).collect();
        overlay
            .sync_snapshot(snapshot(rows, cols, 2, 2, &cells), live)
            .unwrap();
        assert_eq!(
            row_ptrs,
            overlay.cells.iter().map(Vec::as_ptr).collect::<Vec<_>>()
        );
        assert_eq!(
            row_caps,
            overlay.cells.iter().map(Vec::capacity).collect::<Vec<_>>()
        );
        assert!(overlay.quads.capacity() >= MAX_RAIN_QUADS);
        assert!(overlay.halos.capacity() >= MAX_RAIN_ADD);

        let oversized_hidden_band = [0, 1, 2, 3, 4, 5];
        let bad_live = ExternalRainLiveState {
            hidden_cursor_rows: &oversized_hidden_band,
            ..ExternalRainLiveState::default()
        };
        assert!(matches!(
            overlay.sync_snapshot(snapshot(rows, cols, 3, 3, &cells), bad_live),
            Err(ExternalRainError::HiddenCursorRows { .. })
        ));

        let out_of_range = [rows];
        let bad_live = ExternalRainLiveState {
            hidden_cursor_rows: &out_of_range,
            ..ExternalRainLiveState::default()
        };
        assert!(matches!(
            overlay.sync_snapshot(snapshot(rows, cols, 3, 3, &cells), bad_live),
            Err(ExternalRainError::HiddenCursorRowOutOfRange { .. })
        ));
        assert!(!overlay.snapshot_ready);
        overlay
            .sync_snapshot(snapshot(rows, cols, 4, 4, &cells), live)
            .unwrap();
        assert!(matches!(
            overlay.emit(GEOM, bad_live),
            Err(ExternalRainError::HiddenCursorRowOutOfRange { .. })
        ));
        assert!(!overlay.snapshot_ready);

        let bad = ExternalRainSnapshot {
            rows: MAX_GRID_ROWS,
            cols: MAX_GRID_COLS,
            revision: 3,
            content_seq: 3,
            default_bg: BG,
            theme_fg: FG,
            cells: &[],
            single_width_rows: &[],
        };
        assert!(matches!(
            overlay.sync_snapshot(bad, live),
            Err(ExternalRainError::TooManyCells { .. })
        ));
        assert!(!overlay.snapshot_ready);
        assert!(overlay.quads().is_empty() && overlay.halos().is_empty());
    }
}
