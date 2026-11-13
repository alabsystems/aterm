// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! `TerminalCheckpoint`: a scoped, round-trippable projection of a live [`Terminal`].

use aterm_parser::{HookedDcsCarry, ParserCarry};
use aterm_types::charset::CharacterSetState;
use aterm_types::{KittyKeyboardStateSnapshot, TaskbarProgress, XtermKeyboardState};

use super::Terminal;
use super::checkpoint_holder::HolderRepr;
use super::checkpoint_output::OutputStateRepr;
use super::checkpoint_state::{ColorRepr, ShellRepr, TitleRepr};
use super::types::{CurrentStyle, TerminalModes};
use crate::grid::{CellFlags, Cursor, Grid, PackedColor, SavedCursorState, VisibleCells};
use crate::scrollback::{Scrollback, deserialize_lines, serialize_line_views};

/// Per-grid cursor + region + wrap + tab-stop projection, captured for the main grid and (when present) the alt grid.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct GridCursorRepr {
    /// Cursor row (0-based, grid-relative).
    pub cursor_row: u16,
    /// Cursor column (0-based, grid-relative).
    pub cursor_col: u16,
    /// Deferred-wrap (pending-wrap / wrap-next) flag.
    pub pending_wrap: bool,
    /// The glyph before the cursor is still the one a mark continues (`Grid::cursor_follows_glyph`).
    #[cfg_attr(
        feature = "serde",
        serde(default, skip_serializing_if = "std::ops::Not::not")
    )]
    pub follows_glyph: bool,
    /// DECSTBM scroll-region top (inclusive).
    pub scroll_top: u16,
    /// DECSTBM scroll-region bottom (inclusive).
    pub scroll_bottom: u16,
    /// DECSLRM horizontal-margin left (inclusive).
    pub margin_left: u16,
    /// DECSLRM horizontal-margin right (inclusive).
    pub margin_right: u16,
    /// Per-column tab stops (`true` = stop set at that column).
    pub tab_stops: Vec<bool>,
    /// Whether TBC 3 suppressed the every-8 defaults.
    #[cfg_attr(feature = "serde", serde(default))]
    pub tab_defaults_suppressed: bool,
    /// What the grid's rows-only resizes owe back at one edge (`Grid::handback_rows`).
    #[cfg_attr(
        feature = "serde",
        serde(default, skip_serializing_if = "is_zero_rows")
    )]
    pub handback_rows: i32,
    /// The natural moves made after what `handback_rows` owes (`Grid::handback_above`).
    #[cfg_attr(
        feature = "serde",
        serde(default, skip_serializing_if = "is_zero_above")
    )]
    pub handback_above: u16,
}

impl GridCursorRepr {
    fn capture(grid: &Grid) -> Self {
        let region = grid.scroll_region();
        let margins = grid.horizontal_margins();
        Self {
            cursor_row: grid.cursor_row(),
            cursor_col: grid.cursor_col(),
            pending_wrap: grid.pending_wrap(),
            follows_glyph: grid.cursor_follows_glyph(),
            scroll_top: region.top,
            scroll_bottom: region.bottom,
            margin_left: margins.left,
            margin_right: margins.right,
            tab_stops: grid.tab_stops().to_vec(),
            tab_defaults_suppressed: grid.tab_defaults_suppressed(),
            handback_rows: grid.handback_rows(),
            handback_above: grid.handback_above(),
        }
    }

    fn apply(&self, grid: &mut Grid) {
        grid.set_scroll_region(self.scroll_top, self.scroll_bottom);
        grid.set_horizontal_margins(self.margin_left, self.margin_right);
        grid.set_cursor(self.cursor_row, self.cursor_col);
        grid.set_pending_wrap(self.pending_wrap);
        grid.set_cursor_follows_glyph(self.follows_glyph);
        grid.restore_tab_stops(&self.tab_stops, self.tab_defaults_suppressed);
        grid.seed_handback(self.handback_rows, self.handback_above);
    }
}

/// Minimal, by-value style projection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StyleRepr {
    /// Foreground color.
    pub fg: PackedColor,
    /// Background color.
    pub bg: PackedColor,
    /// SGR cell flags (bold, italic, underline, …).
    pub flags: CellFlags,
    /// DECSCA selective-erase protection.
    pub protected: bool,
}

impl StyleRepr {
    #[inline]
    const fn capture(style: &CurrentStyle) -> Self {
        Self {
            fg: style.fg,
            bg: style.bg,
            flags: style.flags,
            protected: style.protected,
        }
    }

    #[inline]
    const fn into_style(self) -> CurrentStyle {
        CurrentStyle::new(self.fg, self.bg, self.flags, self.protected)
    }
}

/// A DECSC slot's deferred-wrap flag (a bare `bool` on the wire).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(transparent))]
pub struct SavedCursorPendingWrap(bool);

impl SavedCursorPendingWrap {
    #[inline]
    pub(super) const fn new(value: bool) -> Self {
        Self(value)
    }

    #[inline]
    pub(super) const fn get(self) -> bool {
        self.0
    }
}

/// Whether a resize changed the screen's size since a DECSC slot was saved
/// (`SavedCursorState::resized`): a bare `bool` on the wire, absent when false, which is all an
/// older producer wrote.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(transparent))]
pub struct SavedCursorResized(bool);

impl SavedCursorResized {
    #[inline]
    pub(super) const fn new(value: bool) -> Self {
        Self(value)
    }

    #[inline]
    pub(super) const fn get(self) -> bool {
        self.0
    }
}

/// Wire-stable DECSC/DECRC slot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct SavedCursorRepr {
    pub cursor_row: u16,
    pub cursor_col: u16,
    pub style_fg_bits: u32,
    pub style_bg_bits: u32,
    pub style_flag_bits: u16,
    pub style_protected: bool,
    pub origin_mode: bool,
    pub auto_wrap: bool,
    pub charset: CharacterSetState,
    pub pending_wrap: SavedCursorPendingWrap,
    pub underline_color: Option<u32>,
    #[cfg_attr(
        feature = "serde",
        serde(default, skip_serializing_if = "is_unresized")
    )]
    pub resized: SavedCursorResized,
}

impl SavedCursorRepr {
    #[inline]
    const fn capture(saved: SavedCursorState) -> Self {
        Self {
            cursor_row: saved.cursor.row,
            cursor_col: saved.cursor.col,
            style_fg_bits: saved.style.fg.0,
            style_bg_bits: saved.style.bg.0,
            style_flag_bits: saved.style.flags.0,
            style_protected: saved.style.protected,
            origin_mode: saved.origin_mode,
            auto_wrap: saved.auto_wrap,
            charset: saved.charset,
            pending_wrap: SavedCursorPendingWrap::new(saved.pending_wrap),
            underline_color: saved.underline_color,
            resized: SavedCursorResized::new(saved.resized),
        }
    }

    #[inline]
    const fn into_saved(self) -> SavedCursorState {
        SavedCursorState {
            cursor: Cursor::new(self.cursor_row, self.cursor_col),
            style: CurrentStyle::new(
                PackedColor(self.style_fg_bits),
                PackedColor(self.style_bg_bits),
                CellFlags(self.style_flag_bits),
                self.style_protected,
            ),
            origin_mode: self.origin_mode,
            auto_wrap: self.auto_wrap,
            charset: self.charset,
            pending_wrap: self.pending_wrap.get(),
            underline_color: self.underline_color,
            resized: self.resized.get(),
        }
    }
}

/// The shell-integration capability nonce an adopted shell keeps emitting (`ATERM_SHELL_NONCE`, injected once at its first spawn).
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct ShellIntegrationNonce(pub [u8; 32]);

impl std::fmt::Debug for ShellIntegrationNonce {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ShellIntegrationNonce(..)")
    }
}

impl ShellIntegrationNonce {
    /// Lowercase hex, the wire form the shell's `id=` parameter uses.
    #[must_use]
    pub fn to_hex(&self) -> String {
        use std::fmt::Write as _;
        self.0.iter().fold(String::with_capacity(64), |mut s, b| {
            let _ = write!(s, "{b:02x}");
            s
        })
    }

    /// Parse exactly 64 hex digits; anything else is `None` (a malformed carry degrades to "no nonce", never to a partial one).
    #[must_use]
    pub fn from_hex(hex: &str) -> Option<Self> {
        let bytes = hex.as_bytes();
        if bytes.len() != 64 {
            return None;
        }
        let mut out = [0u8; 32];
        for (i, pair) in bytes.as_chunks::<2>().0.iter().enumerate() {
            let hi = char::from(pair[0]).to_digit(16)?;
            let lo = char::from(pair[1]).to_digit(16)?;
            out[i] = u8::try_from(hi * 16 + lo).ok()?;
        }
        Some(Self(out))
    }
}

/// Whether a terminal's OSC 133/633 marks can reach it, as the host reports it (`status integration=`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ShellIntegrationPosture {
    /// Disabled by configuration or unsupported shell.
    Off,
    /// Enabled and receiving OSC 133/633 marks.
    On,
    /// Enabled in configuration, but marks are not reaching the terminal.
    Degraded,
}

impl ShellIntegrationPosture {
    /// The wire word.
    #[inline]
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::On => "on",
            Self::Degraded => "degraded",
        }
    }
}

/// A scoped, round-trippable projection of a live [`Terminal`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TerminalCheckpoint {
    /// Grid rows (visible).
    pub rows: u16,
    /// Grid cols.
    pub cols: u16,
    /// Main-grid body: `serialize_lines(grid.checkpoint_lines_bounded(..))` (scrollback-then-visible).
    pub grid: Vec<u8>,
    /// How many of `grid`'s leading line records are scrollback (`grid` holds `history_lines + rows` records), carried so the consumer can bound its allocation from the authenticated meta before decoding.
    pub history_lines: u32,
    /// Main-grid cursor/region/wrap/tab projection.
    pub cursor: GridCursorRepr,
    /// Alt-grid body, if an alt grid exists.
    pub alt_grid: Option<Vec<u8>>,
    /// Alt-grid cursor/region/wrap/tab projection, if an alt grid exists.
    pub alt_cursor: Option<GridCursorRepr>,
    /// What `grid`'s visible rows hold that their line records leave out ([`VisibleCells`]): each row's DEC line size and its DECSCA-protected columns. Empty from a producer that carried none.
    pub visible_cells: VisibleCells,
    /// The same for `alt_grid`'s visible rows (empty when there is none).
    pub alt_visible_cells: VisibleCells,
    /// DECSC/DECRC save slot for the main screen.
    pub saved_cursor_main: Option<SavedCursorRepr>,
    /// DECSC/DECRC save slot for the alternate screen.
    pub saved_cursor_alt: Option<SavedCursorRepr>,
    /// Terminal modes (Copy; ~40 boolean/enum fields).
    pub modes: TerminalModes,
    /// Current SGR style (minimal by-value projection).
    pub style: StyleRepr,
    /// Character set state (G0-G3, GL, GR, single-shift).
    pub charset: CharacterSetState,
    /// Kitty keyboard protocol state (via snapshot/restore).
    pub kitty_keyboard: KittyKeyboardStateSnapshot,
    /// xterm keyboard modifier/format options (XTMODKEYS/XTFMTKEYS).
    pub xterm_keyboard: XtermKeyboardState,
    /// Taskbar progress (ConEmu OSC 9;4).
    pub taskbar_progress: Option<TaskbarProgress>,
    /// Secure keyboard entry mode.
    pub secure_keyboard_entry: bool,
    /// Current working directory (OSC 7).
    pub current_working_directory: Option<String>,
    /// The parser this checkpoint restores into is in Ground.
    pub parser_ground: bool,
    /// The parser's partial state ([`ParserCarry`]).
    pub parser: Option<ParserCarry>,
    /// The authorized shell-integration nonce.
    pub shell_integration_nonce: Option<ShellIntegrationNonce>,
    /// The absolute row counter of the grid in `grid`.
    pub absolute_row_counter: u64,
    /// The same for the grid in `alt_grid` (`0` when there is none).
    pub alt_absolute_row_counter: u64,
    /// Application-set colours, as a diff from the configured baseline.
    pub color: ColorRepr,
    /// Shell-integration (OSC 133/633) state.
    pub shell: ShellRepr,
    /// The integration body the shell last signed that it runs ([`Terminal::shell_integration_rev`]).
    pub shell_integration_rev: Option<String>,
    /// The titles a program set ([`TitleRepr`]).
    pub title: Option<TitleRepr>,
    /// The engine state output still to come relies on ([`OutputStateRepr`]); a restore installs it whole.
    pub output_state: OutputStateRepr,
    /// The holder fence and the main grid's span anchors ([`HolderRepr`]), keyed like `shell`'s rows.
    pub holder: HolderRepr,
}

impl TerminalCheckpoint {
    /// Whether the parser carry holds part of an OSC 133/633 shell-integration mark past its command number, the payload a signed mark carries the session's nonce in (`Terminal::partial_sequence_is_shell_mark` at the capture).
    #[must_use]
    pub fn parser_holds_shell_mark(&self) -> bool {
        self.parser.as_ref().is_some_and(|carry| {
            carry.state == aterm_parser::State::OscString && osc_payload_is_shell_mark(&carry.osc)
        })
    }

    /// This checkpoint less its lineage's seed uncertainty ([`HolderRepr::seed_untrusted`]).
    #[cfg(any(test, debug_assertions))]
    #[must_use]
    pub const fn without_seed_lineage(mut self) -> Self {
        self.holder.seed_untrusted = false;
        self
    }
}

/// In-process transient engine state paired with a [`TerminalCheckpoint`] so a replay keyframe continues with the same open mode-2026 frame start, alternate-screen leave notes, Kitty ids and serials, rate-limit buckets, shell-integration authorization and pending restore lineage as the engine it was taken from.
///
/// `ReplayCarry::default()` is empty, leaving [`Terminal::replay_from_checkpoint`]'s defaults untouched when a caller replays from a bare [`TerminalCheckpoint`].
#[derive(Clone, Debug, Default)]
pub struct ReplayCarry(Option<CapturedCarry>);

impl ReplayCarry {
    /// Earliest live timestamp held by this carry (`sync_start`, fallback response limiter, or policy buckets).
    /// Shown-since timestamps of carried Kitty animations are included as well.
    #[must_use]
    pub fn earliest_instant(&self) -> Option<aterm_time::Instant> {
        let c = self.0.as_ref()?;
        [
            c.sync_start,
            c.response_rate_limiter.snapshot().3,
            c.policy.earliest_refill(),
        ]
        .into_iter()
        .flatten()
        .chain(c.kitty_frames.values().map(|animation| animation.shown_at))
        .min()
    }

    /// The bytes this carry holds besides its image payloads: its own size and every allocation it owns, its Kitty tables, placements and span lists, its policy and its hyperlink schemes among them. The image payloads an `Arc` shares with the engine it was taken from, and an in-flight transfer's chunks, are [`Self::retained_image_bytes`].
    #[must_use]
    pub fn memory_used(&self) -> usize {
        use super::checkpoint_heap::Heap as _;
        std::mem::size_of::<Self>().saturating_add(self.0.as_ref().map_or(0, CapturedCarry::heap))
    }

    /// The bytes of the in-flight Kitty transfer's chunks this carry copied (an `m=1` command's payload so far), which it owns alone.
    #[must_use]
    pub fn pending_transfer_bytes(&self) -> usize {
        self.0
            .as_ref()
            .and_then(|c| c.kitty_pending.as_ref())
            .map_or(0, |cmd| cmd.payload.capacity())
    }

    /// Distinct Kitty image and in-flight chunk payload bytes retained by this carry: [`Self::pending_transfer_bytes`] and the payload of each image [`Self::for_each_retained_image`] visits.
    #[must_use]
    pub fn retained_image_bytes(&self) -> usize {
        let mut total = self.pending_transfer_bytes();
        self.for_each_retained_image(|img| total = total.saturating_add(img.payload_bytes()));
        total
    }

    /// Visit once each distinct image this carry keeps alive (one `Arc` several of its tables share is one image): the Kitty store's images, animation frames, variants and their sources, virtual and floating placements, and the inline and Kitty spans on both grids' visible rows and carried history. Each `Arc` is shared with the engine the carry was taken from, and with every other carry taken while that engine held it.
    pub fn for_each_retained_image(
        &self,
        mut f: impl FnMut(&std::sync::Arc<aterm_types::ImageData>),
    ) {
        use aterm_types::ImageData;
        use std::sync::Arc;
        let Some(c) = self.0.as_ref() else {
            return;
        };
        let mut seen: Vec<usize> = Vec::new();
        let mut visit = |img: &Arc<ImageData>| {
            let ptr = Arc::as_ptr(img) as usize;
            if let Err(pos) = seen.binary_search(&ptr) {
                seen.insert(pos, ptr);
                f(img);
            }
        };
        for img in c.kitty_images.values() {
            visit(img);
        }
        for anim in c.kitty_frames.values() {
            for frame in &anim.frames {
                visit(&frame.image);
            }
        }
        for variants in c.kitty_variants.values() {
            for variant in variants {
                visit(&variant.source);
                visit(&variant.image);
            }
        }
        for placements in c.kitty_virtual.values() {
            for placement in placements {
                visit(&placement.image);
            }
        }
        for floating in &c.kitty_floating {
            visit(&floating.image);
        }
        for (_, spans) in &c.active_images.visible {
            for span in spans {
                visit(&span.image);
            }
        }
        for (_, spans) in &c.active_images.history_rev {
            for span in spans {
                visit(&span.image);
            }
        }
        if let Some(inactive) = &c.inactive_images {
            for (_, spans) in &inactive.visible {
                for span in spans {
                    visit(&span.image);
                }
            }
            for (_, spans) in &inactive.history_rev {
                for span in spans {
                    visit(&span.image);
                }
            }
        }
    }

    /// Host alternate-scroll baseline captured with this keyframe, if non-empty.
    #[must_use]
    pub const fn host_alternate_scroll(&self) -> Option<bool> {
        match &self.0 {
            Some(c) => Some(c.host_alternate_scroll),
            None => None,
        }
    }

    /// Host-measured cell box `(width_px, height_px)` captured with this keyframe, if set by the host.
    #[must_use]
    pub const fn host_cell_px(&self) -> Option<(u16, u16)> {
        match &self.0 {
            Some(c) if c.cell_px_from_host && c.cell_px.0 > 0 && c.cell_px.1 > 0 => Some(c.cell_px),
            _ => None,
        }
    }

    /// Security policy document installed when this keyframe was captured, if any.
    #[must_use]
    pub fn security_policy(&self) -> Option<aterm_policy::Policy> {
        self.0.as_ref().and_then(|c| c.policy.policy_doc().cloned())
    }

    /// Host-authorized extra OSC 8 URI schemes captured with this keyframe.
    #[must_use]
    pub fn hyperlink_schemes(&self) -> Vec<String> {
        self.0.as_ref().map_or_else(Vec::new, |c| {
            c.hyperlink_auth
                .extra_schemes()
                .iter()
                .map(|s| (**s).to_owned())
                .collect()
        })
    }

    /// Whether non-direct Kitty graphics media (`t=f`, `t=t`, `t=s`) were admitted when this keyframe was captured.
    #[must_use]
    pub const fn kitty_media_admitted(&self) -> bool {
        matches!(&self.0, Some(c) if c.kitty_media_admitted)
    }
}

/// Kind of hooked DCS query carried across pane replay slices ([`DcsQueryCarry`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DcsQueryKind {
    /// DECRQSS (`ESC P $ q … ST`), whose payload buffer caps at 256 bytes.
    Decrqss,
    /// XTGETTCAP (`ESC P + q … ST`), whose payload buffer caps at 1024 bytes.
    Xtgettcap,
}

impl DcsQueryKind {
    /// Maximum bytes retained in [`DcsQueryCarry::data`] for this query kind.
    #[inline]
    #[must_use]
    pub const fn data_cap(self) -> usize {
        match self {
            Self::Decrqss => 256,
            Self::Xtgettcap => 1024,
        }
    }
}

/// Canonicalizable state of an open hooked `DECRQSS` or `XTGETTCAP` query across pane replay slices.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DcsQueryCarry {
    /// Which query (`DECRQSS` or `XTGETTCAP`) is hooked.
    pub kind: DcsQueryKind,
    /// Retained query payload bytes (capped at [`DcsQueryKind::data_cap`]).
    pub data: Vec<u8>,
    /// Total bytes charged to this DCS sequence (`<= MAX_DCS_GLOBAL_BUDGET`).
    pub sequence_bytes: u32,
    /// Parser state inside the hooked DCS string (`DcsPassthrough`, or `Escape` with ST pending).
    pub parser: HookedDcsCarry,
}

/// Playback state of a carried Kitty animation (`s=`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KittyPlayStateCarry {
    /// `s=1` (and a fresh image): frames change only when `c=` selects one.
    Stopped,
    /// `s=2`: runs, and at the last frame waits for more instead of looping.
    Loading,
    /// `s=3`: runs, looping back to the first frame after the last.
    Running,
}

impl KittyPlayStateCarry {
    const fn from_play_state(state: super::transient_state::KittyPlayState) -> Self {
        match state {
            super::transient_state::KittyPlayState::Stopped => Self::Stopped,
            super::transient_state::KittyPlayState::Loading => Self::Loading,
            super::transient_state::KittyPlayState::Running => Self::Running,
        }
    }

    const fn into_play_state(self) -> super::transient_state::KittyPlayState {
        match self {
            Self::Stopped => super::transient_state::KittyPlayState::Stopped,
            Self::Loading => super::transient_state::KittyPlayState::Loading,
            Self::Running => super::transient_state::KittyPlayState::Running,
        }
    }
}

/// Footprint, stacking and crop parameters of a stored Kitty image or variant.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct KittyLayoutCarry {
    /// Footprint width in cells (`1..=MAX_GRID_COLS`).
    pub cols: u16,
    /// Footprint height in cells (`1..=MAX_GRID_ROWS`).
    pub rows: u16,
    /// Stacking order (`< 0` behind cell text, `>= 0` over it).
    pub z_index: i32,
    /// Sub-cell top-left offset `(x, y)` in device pixels inside the first cell.
    pub cell_offset_px: (u16, u16),
    /// How the source raster maps onto the cell footprint.
    pub scaling: aterm_types::ImageScaling,
    /// Optional non-empty sub-rectangle of the source raster to display.
    pub source_rect: Option<aterm_types::SourceRect>,
}

impl KittyLayoutCarry {
    #[inline]
    pub(super) const fn of(image: &aterm_types::ImageData) -> Self {
        Self {
            cols: image.cols,
            rows: image.rows,
            z_index: image.z_index,
            cell_offset_px: image.cell_offset_px,
            scaling: image.scaling,
            source_rect: image.source_rect,
        }
    }

    #[inline]
    pub(super) fn into_image(
        self,
        mut bytes: Vec<u8>,
        format: aterm_types::ImageFormat,
    ) -> aterm_types::ImageData {
        bytes.shrink_to_fit();
        aterm_types::ImageData {
            bytes,
            format,
            cols: self.cols,
            rows: self.rows,
            z_index: self.z_index,
            band_lift_px: 0,
            cell_offset_px: self.cell_offset_px,
            scaling: self.scaling,
            source_rect: self.source_rect,
        }
    }
}

/// One frame of a carried Kitty image or animation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KittyFrameCarry {
    /// Raw payload bytes (`Png` or packed `RawRgba8`, `<= MAX_KITTY_IMAGE_BYTES`).
    pub bytes: Vec<u8>,
    /// Encoding format (`Png` or `RawRgba8`).
    pub format: aterm_types::ImageFormat,
    /// Root layout parameters of this frame.
    pub layout: KittyLayoutCarry,
    /// Frame display gap in milliseconds (`0` = gapless).
    pub gap_ms: u32,
}

/// Source raster of a cached Kitty layout variant ([`KittyVariantCarry`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum KittyVariantSourceCarry {
    /// Derived from `frames[idx]` (`idx < frames.len()`).
    Frame(u8),
    /// Orphaned when its source frame was later edited or deleted.
    Orphan {
        /// Retained variant payload bytes.
        bytes: Vec<u8>,
        /// Encoding format of `bytes`.
        format: aterm_types::ImageFormat,
    },
}

/// One cached placement layout variant of a carried Kitty image.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KittyVariantCarry {
    /// Source frame index or orphaned raster payload.
    pub source: KittyVariantSourceCarry,
    /// Variant placement layout.
    pub layout: KittyLayoutCarry,
}

/// Target image slot displayed by a carried Kitty virtual placement ([`KittyVirtualCarry`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KittyVirtualImageCarry {
    /// Displays `frames[idx]` directly (`idx < frames.len()`).
    Frame(u8),
    /// Displays `variants[idx]` (`idx < variants.len()`).
    Variant(u8),
}

/// One carried Kitty virtual placement (`U=1`) referenced by Unicode placeholders.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct KittyVirtualCarry {
    /// Client placement ID (`p=`), or `0` when unnamed.
    pub placement_id: u32,
    /// Transmission serial carried on this virtual placement's tag.
    pub image_serial: core::num::NonZeroU32,
    /// Unique placement serial of this virtual placement.
    pub serial: core::num::NonZeroU32,
    /// Frame or variant slot displayed by this virtual placement.
    pub image: KittyVirtualImageCarry,
}

/// Canonicalizable state of one stored Kitty image (`kitty_images`, `kitty_frames`, `kitty_variants`, `kitty_virtual`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KittyImageCarry {
    /// Non-zero Kitty image ID (`>= 1`).
    pub id: u32,
    /// Monotonic transmission serial assigned when this ID was first stored.
    pub image_serial: core::num::NonZeroU32,
    /// Zero-based index into `frames` currently installed in `kitty_images` (`< frames.len()`).
    pub shown_frame: u8,
    /// Animation playback state (`s=`).
    pub state: KittyPlayStateCarry,
    /// Loop limit (`v=` minus 1; `0` = infinite).
    pub max_loops: u32,
    /// Completed loop count since `state` was last set.
    pub loops: u32,
    /// Nanosecond offset from the schema origin when `shown_frame` went up.
    pub shown_at_ns: u64,
    /// Animation frames (`1..=MAX_KITTY_FRAMES`), root transmit first.
    pub frames: Vec<KittyFrameCarry>,
    /// Cached layout variants (`<= MAX_KITTY_VARIANTS`), oldest first.
    pub variants: Vec<KittyVariantCarry>,
    /// Active virtual placements (`<= MAX_KITTY_VIRTUALS`), oldest first.
    pub virtuals: Vec<KittyVirtualCarry>,
}

/// Uncompressed primary-grid scrollback horizon whose inline-image spans [`PaneStepCarry`] carries (`restore_grid`'s 1000-line ring plus `Scrollback::with_defaults()`'s 1000-line hot tier, matching `image-retention-horizon`).
pub const PANE_HISTORY_IMAGE_LINES: usize = 2000;

/// Source raster displayed by a carried inline or floating placement.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PlacedImageSourceCarry {
    /// Displays `frames[frame]` of stored Kitty image `image_id`.
    KittyFrame {
        /// Stored Kitty image ID (`>= 1`).
        image_id: u32,
        /// Zero-based frame index (`< frames.len()`).
        frame: u8,
    },
    /// Displays `variants[variant]` of stored Kitty image `image_id`.
    KittyVariant {
        /// Stored Kitty image ID (`>= 1`).
        image_id: u32,
        /// Zero-based variant index (`< variants.len()`).
        variant: u8,
    },
    /// Displays `loose_images[idx]` (iTerm2 OSC 1337, Sixel, `i=0` Kitty, or a placement whose store entry was freed).
    Loose(u32),
}

/// One loose raster payload referenced by inline or floating placements outside the Kitty store.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LooseImageCarry {
    /// Raw payload bytes (`Png` `<= MAX_IMAGE_BYTES` or packed `RawRgba8`).
    pub bytes: Vec<u8>,
    /// Encoding format (`Png` or `RawRgba8`).
    pub format: aterm_types::ImageFormat,
    /// Placement layout parameters.
    pub layout: KittyLayoutCarry,
}

/// Parent placement identity referenced by a carried Kitty relative or floating placement.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KittyParentCarry {
    /// Named parent `(image_id, placement_id)` (`placement_id >= 1`).
    Named {
        /// Parent image ID (`0` when unnamed).
        image_id: u32,
        /// Non-zero parent placement ID (`p=`).
        placement_id: core::num::NonZeroU32,
    },
    /// Unnamed one-off parent `(image_id, serial)`.
    Serial {
        /// Parent image ID (`0` when unnamed).
        image_id: u32,
        /// Parent placement serial.
        serial: core::num::NonZeroU32,
    },
}

impl KittyParentCarry {
    pub(super) fn from_parent(p: super::transient_state::KittyParent) -> Option<Self> {
        match p {
            super::transient_state::KittyParent::Named {
                image_id,
                placement_id,
            } => Some(Self::Named {
                image_id,
                placement_id: core::num::NonZeroU32::new(placement_id)?,
            }),
            super::transient_state::KittyParent::Serial { image_id, serial } => {
                Some(Self::Serial { image_id, serial })
            }
        }
    }

    pub(super) const fn into_parent(self) -> super::transient_state::KittyParent {
        match self {
            Self::Named {
                image_id,
                placement_id,
            } => super::transient_state::KittyParent::Named {
                image_id,
                placement_id: placement_id.get(),
            },
            Self::Serial { image_id, serial } => {
                super::transient_state::KittyParent::Serial { image_id, serial }
            }
        }
    }
}

/// Ancestor kind of a carried Kitty floating placement ([`KittyFloatingCarry`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KittyFloatParentCarry {
    /// Relative to a virtual placement (`U=1`).
    Virtual(KittyParentCarry),
    /// Relative to another floating placement.
    Floating(KittyParentCarry),
}

impl KittyFloatParentCarry {
    pub(super) fn from_float_parent(p: super::transient_state::KittyFloatParent) -> Option<Self> {
        match p {
            super::transient_state::KittyFloatParent::Virtual(parent) => {
                Some(Self::Virtual(KittyParentCarry::from_parent(parent)?))
            }
            super::transient_state::KittyFloatParent::Floating(parent) => {
                Some(Self::Floating(KittyParentCarry::from_parent(parent)?))
            }
        }
    }

    pub(super) const fn into_float_parent(self) -> super::transient_state::KittyFloatParent {
        match self {
            Self::Virtual(p) => super::transient_state::KittyFloatParent::Virtual(p.into_parent()),
            Self::Floating(p) => {
                super::transient_state::KittyFloatParent::Floating(p.into_parent())
            }
        }
    }
}

/// One carried Kitty placement relative to a virtual or floating placement (`kitty_floating`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct KittyFloatingCarry {
    /// Placement identity (`image_id >= 1`).
    pub tag: aterm_grid::KittyPlacementTag,
    /// Displayed frame, variant, or loose raster slot.
    pub image: PlacedImageSourceCarry,
    /// Virtual or floating parent placement.
    pub parent: KittyFloatParentCarry,
    /// Horizontal offset in columns (`H=`).
    pub dx: i32,
    /// Vertical offset in rows (`V=`).
    pub dy: i32,
}

/// One carried Kitty cell-relative placement relation (`kitty_relations`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct KittyRelationCarry {
    /// Child placement identity (`image_id >= 1`).
    pub child: aterm_grid::KittyPlacementTag,
    /// Parent placement identity.
    pub parent: KittyParentCarry,
    /// Horizontal offset in columns (`H=`).
    pub dx: i32,
    /// Vertical offset in rows (`V=`).
    pub dy: i32,
}

/// One coalesced inline-image span on a visible or history row.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PlacedSpanCarry {
    /// Inclusive start column (`< end_col`).
    pub start_col: u16,
    /// Exclusive end column (`<= MAX_GRID_COLS`).
    pub end_col: u16,
    /// Zero-based footprint row (`< layout.rows`).
    pub image_row: u16,
    /// Zero-based footprint column painted at `start_col`.
    pub first_cell_col: u16,
    /// Displayed frame, variant, or loose raster slot.
    pub image: PlacedImageSourceCarry,
    /// Kitty placement tag for layer ordering, or `None` for non-Kitty formats.
    pub kitty: Option<aterm_grid::KittyPlacementTag>,
}

/// Carried inline-image spans of one grid (visible screen plus newest uncompressed history rows).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PlacedGridCarry {
    /// Non-empty visible-row span lists `(row, spans)` sorted ascending by `row`.
    pub visible: Vec<(u16, Vec<PlacedSpanCarry>)>,
    /// Non-empty history-row span lists `(rev_idx, spans)` sorted ascending by `rev_idx` (`0` = newest).
    pub history_rev: Vec<(u32, Vec<PlacedSpanCarry>)>,
}

/// Canonicalizable per-slice transient engine state paired with a [`TerminalCheckpoint`] in a pane replay state (`aterm-aos-pane`).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "independent transient terminal flags carried across pane replay slices"
)]
pub struct PaneStepCarry {
    /// Open DEC mode-2026 frame start as nanoseconds past the schema origin, or `None`.
    pub sync_start_ns: Option<u64>,
    /// Synchronized-output timeout duration in nanoseconds.
    pub sync_timeout_ns: u64,
    /// Fallback response rate limiter: `(tokens_bits, capacity_bytes, refill_bytes_per_sec, last_refill_ns)`.
    pub response_limiter: (u64, u64, u64, Option<u64>),
    /// Policy rate-limit buckets in first-declaration order: `(tokens_bits, last_refill_ns)`.
    pub policy_buckets: Vec<(u64, Option<u64>)>,
    /// Whether the active alternate screen was entered via `?47h` / `?1047h` without saving the cursor.
    pub alt_entered_without_cursor_save: bool,
    /// Whether ConPTY left an alternate screen up across a mode change.
    pub conhost_alt_screen_left_up: bool,
    /// `screen_replaced` generation of an alternate screen proved dead by a shell prompt.
    pub dead_alt_screen: Option<u64>,
    /// `(screen_replaced, row, col)` where a shell prompt arrived on a dead alternate screen.
    pub dead_alt_prompt_at: Option<(u64, u16, u16)>,
    /// Count of full-screen replacements since engine creation.
    pub screen_replaced: u64,
    /// Whether an in-band size report was held pending a prompt or leave.
    pub size_report_held: bool,
    /// Whether bracketed paste was enabled while at a shell prompt.
    pub bracketed_paste_from_prompt: bool,
    /// Last Kitty graphics image ID assigned by the terminal.
    pub kitty_last_assigned_id: u32,
    /// Kitty graphics monotonic placement serial.
    pub kitty_serial: u32,
    /// Live Kitty image byte charge `[primary, alternate]`.
    pub live_image_charge: [u64; 2],
    /// Payload bytes the primary screen may still place before its history's images are measured again (`0` measures at the next placement).
    pub history_image_room: u64,
    /// Whether the most recent OSC sequence was terminated by BEL (`0x07`) rather than ST.
    pub last_osc_bel_terminated: bool,
    /// Pending restore holder-lineage tag (`0 = None`, `1 = Untouched`, `2 = Touched`).
    pub pending_restore: u8,
    /// Host foreground-job state (`set_foreground_job`).
    pub foreground_job: bool,
    /// Last `program_probe` active state while `foreground_job` is `false`.
    pub probe_job_active: bool,
    /// Capability nonce authorized pending the shell's first mark (`awaiting_first_mark`), when any.
    pub awaiting_shell_nonce: Option<ShellIntegrationNonce>,
    /// Prior authorization replaced by an unsettled typed re-key (`(nonce, awaiting_first_mark)`), when any.
    pub typed_rekey_prior: Option<(Option<ShellIntegrationNonce>, bool)>,
    /// Full owed resize hand-back state of the active grid.
    pub active_handback: aterm_grid::GridHandbackCarry,
    /// Full owed resize hand-back state of the inactive grid, when present.
    pub inactive_handback: Option<aterm_grid::GridHandbackCarry>,
    /// Open hooked `DECRQSS` or `XTGETTCAP` query state, when the slice ended mid-query.
    pub dcs_query: Option<DcsQueryCarry>,
    /// Stored Kitty images, animation frames, variants and virtual placements, sorted ascending by `id`.
    pub kitty_store: Vec<KittyImageCarry>,
    /// Kitty image number table `(number, id)`, sorted ascending by `number`.
    pub kitty_numbers: Vec<(u32, u32)>,
    /// In-flight chunked Kitty transmission (`m=1`), when any.
    pub kitty_pending: Option<super::kitty_graphics::KittyCommand>,
    /// Loose inline/floating raster payloads outside `kitty_store`, in canonical first-appearance order.
    pub loose_images: Vec<LooseImageCarry>,
    /// Kitty placements relative to a virtual or floating placement (`kitty_floating`), oldest first.
    pub kitty_floating: Vec<KittyFloatingCarry>,
    /// Kitty cell-relative placement relations (`kitty_relations`), oldest first.
    pub kitty_relations: Vec<KittyRelationCarry>,
    /// Active-grid inline-image spans (visible screen plus up to [`PANE_HISTORY_IMAGE_LINES`] primary history rows).
    pub active_images: PlacedGridCarry,
    /// Inactive-grid inline-image spans (`Some` iff an inactive grid exists).
    pub inactive_images: Option<PlacedGridCarry>,
}

fn canonical_f64_bits(v: f64) -> u64 {
    if !v.is_finite() || v <= 0.0 {
        0.0f64.to_bits()
    } else {
        v.to_bits()
    }
}

fn offset_ns_from(origin: aterm_time::Instant, instant: aterm_time::Instant) -> u64 {
    u64::try_from(instant.saturating_duration_since(origin).as_nanos()).unwrap_or(u64::MAX)
}

fn instant_at_offset(origin: aterm_time::Instant, offset_ns: u64) -> aterm_time::Instant {
    origin
        .checked_add(std::time::Duration::from_nanos(offset_ns))
        .unwrap_or(origin)
}

const fn duration_to_ns_saturating(d: std::time::Duration) -> u64 {
    let nanos = d.as_nanos();
    if nanos > u64::MAX as u128 {
        u64::MAX
    } else {
        #[allow(
            clippy::cast_possible_truncation,
            reason = "explicitly clamped to u64::MAX above"
        )]
        {
            nanos as u64
        }
    }
}

impl PaneStepCarry {
    /// Derive the pure baseline carry for a bare [`TerminalCheckpoint`] under `config` and `policy` without reading a clock.
    #[must_use]
    pub fn initial(
        checkpoint: &TerminalCheckpoint,
        config: &crate::config::TerminalConfig,
        policy: Option<&aterm_policy::Policy>,
    ) -> Self {
        use super::response_rate_limiter::{DEFAULT_BURST_BYTES, DEFAULT_REFILL_BYTES_PER_SEC};
        #[allow(
            clippy::cast_precision_loss,
            reason = "burst is bytes; f64 represents DEFAULT_BURST_BYTES (96 KiB) with zero loss"
        )]
        let default_burst = DEFAULT_BURST_BYTES as f64;
        let policy_buckets = policy.map_or_else(Vec::new, |p| {
            aterm_policy::limits::RateLimiterSet::from_policy(p)
                .snapshots()
                .into_iter()
                .map(|b| (canonical_f64_bits(b.tokens), None))
                .collect()
        });
        Self {
            sync_start_ns: checkpoint.modes.synchronized_output.then_some(0),
            sync_timeout_ns: duration_to_ns_saturating(std::time::Duration::from_millis(
                config.sync_timeout_ms,
            )),
            response_limiter: (
                canonical_f64_bits(default_burst),
                DEFAULT_BURST_BYTES,
                DEFAULT_REFILL_BYTES_PER_SEC,
                None,
            ),
            policy_buckets,
            alt_entered_without_cursor_save: false,
            conhost_alt_screen_left_up: false,
            dead_alt_screen: None,
            dead_alt_prompt_at: None,
            screen_replaced: 0,
            size_report_held: false,
            bracketed_paste_from_prompt: false,
            kitty_last_assigned_id: 0,
            kitty_serial: 0,
            live_image_charge: [0, 0],
            history_image_room: 0,
            last_osc_bel_terminated: false,
            pending_restore: 0,
            foreground_job: false,
            probe_job_active: false,
            awaiting_shell_nonce: None,
            typed_rekey_prior: None,
            active_handback: aterm_grid::GridHandbackCarry::from_cursor(
                checkpoint.cursor.handback_rows,
                checkpoint.cursor.handback_above,
                aterm_grid::ResizeLaw::default(),
            ),
            inactive_handback: checkpoint.alt_cursor.as_ref().map(|c| {
                aterm_grid::GridHandbackCarry::from_cursor(
                    c.handback_rows,
                    c.handback_above,
                    aterm_grid::ResizeLaw::default(),
                )
            }),
            dcs_query: None,
            kitty_store: Vec::new(),
            kitty_numbers: Vec::new(),
            kitty_pending: None,
            loose_images: Vec::new(),
            kitty_floating: Vec::new(),
            kitty_relations: Vec::new(),
            active_images: PlacedGridCarry::default(),
            inactive_images: checkpoint
                .alt_cursor
                .as_ref()
                .map(|_| PlacedGridCarry::default()),
        }
    }

    /// Capture the live transient step carry relative to `origin`, bounding owed pushed-row handback by `max_history`.
    #[must_use]
    pub fn capture(term: &Terminal, origin: aterm_time::Instant, max_history: usize) -> Self {
        let t = &term.transient;
        let (rl_tokens, rl_cap, rl_refill, rl_last) = t.response_rate_limiter.snapshot();
        let pending_restore = match term.holder_carried_at {
            None => 0,
            Some(at) if at == t.screen_mutations => 1,
            Some(_) => 2,
        };
        let active_cap = term.grid.scrollback_lines().min(max_history);
        let inactive_handback = term.alt_grid.as_ref().map(|g| {
            g.capture_handback_carry()
                .bounded(g.scrollback_lines().min(max_history))
        });
        let (awaiting_shell_nonce, typed_rekey_prior) =
            term.shell_integration_auth.awaiting_carry();
        let dcs_query = capture_dcs_query(term);
        let (kitty_store, kitty_numbers) = capture_kitty_carry(
            &t.kitty_images,
            &t.kitty_frames,
            &t.kitty_variants,
            &t.kitty_virtual,
            &t.kitty_numbers,
            origin,
        );
        let on_alt = term.modes.alternate_screen;
        let active_img_cap = if on_alt {
            0
        } else {
            max_history.min(PANE_HISTORY_IMAGE_LINES)
        };
        let inactive_img_cap = if on_alt {
            max_history.min(PANE_HISTORY_IMAGE_LINES)
        } else {
            0
        };
        let raw_active_images = term.grid.capture_image_carry(active_img_cap);
        let raw_inactive_images = term
            .alt_grid
            .as_ref()
            .map(|g| g.capture_image_carry(inactive_img_cap));
        let (loose_images, kitty_floating, kitty_relations, active_images, inactive_images) =
            capture_placed_carry(
                &t.kitty_images,
                &t.kitty_frames,
                &t.kitty_variants,
                &t.kitty_floating,
                &t.kitty_relations,
                &raw_active_images,
                raw_inactive_images.as_ref(),
            );
        Self {
            sync_start_ns: t.sync_start.map(|at| offset_ns_from(origin, at)),
            sync_timeout_ns: duration_to_ns_saturating(term.sync_timeout_duration),
            response_limiter: (
                canonical_f64_bits(rl_tokens),
                rl_cap,
                rl_refill,
                rl_last.map(|at| offset_ns_from(origin, at)),
            ),
            policy_buckets: term
                .policy
                .bucket_snapshots()
                .into_iter()
                .map(|b| {
                    (
                        canonical_f64_bits(b.tokens),
                        b.last_refill.map(|at| offset_ns_from(origin, at)),
                    )
                })
                .collect(),
            alt_entered_without_cursor_save: t.alt_entered_without_cursor_save,
            conhost_alt_screen_left_up: t.conhost_alt_screen_left_up,
            dead_alt_screen: t.dead_alt_screen,
            dead_alt_prompt_at: t.dead_alt_prompt_at,
            screen_replaced: t.screen_replaced,
            size_report_held: t.size_report_held,
            bracketed_paste_from_prompt: t.bracketed_paste_from_prompt,
            kitty_last_assigned_id: t.kitty_last_assigned_id,
            kitty_serial: t.kitty_serial,
            live_image_charge: t
                .live_image_charge
                .map(|n| u64::try_from(n).unwrap_or(u64::MAX)),
            history_image_room: u64::try_from(t.history_image_room).unwrap_or(u64::MAX),
            last_osc_bel_terminated: t.last_osc_bel_terminated,
            pending_restore,
            foreground_job: term.shell.foreground_job,
            probe_job_active: !term.shell.foreground_job && term.shell.probe_job_active,
            awaiting_shell_nonce,
            typed_rekey_prior,
            active_handback: term.grid.capture_handback_carry().bounded(active_cap),
            inactive_handback,
            dcs_query,
            kitty_store,
            kitty_numbers,
            kitty_pending: t.kitty_pending.clone(),
            loose_images,
            kitty_floating,
            kitty_relations,
            active_images,
            inactive_images,
        }
    }

    /// Project an in-process [`ReplayCarry`] paired with `checkpoint` onto `origin`, falling back to [`Self::initial`] when `carry` is empty.
    #[must_use]
    pub fn from_replay_carry(
        carry: &ReplayCarry,
        checkpoint: &TerminalCheckpoint,
        config: &crate::config::TerminalConfig,
        policy: Option<&aterm_policy::Policy>,
        origin: aterm_time::Instant,
    ) -> Self {
        let Some(c) = &carry.0 else {
            return Self::initial(checkpoint, config, policy);
        };
        let (rl_tokens, rl_cap, rl_refill, rl_last) = c.response_rate_limiter.snapshot();
        let pending_restore = match c.pending_restore {
            PendingRestore::None => 0,
            PendingRestore::Untouched => 1,
            PendingRestore::Touched => 2,
        };
        let (awaiting_shell_nonce, typed_rekey_prior) = c.shell_integration_auth.awaiting_carry();
        let active_cap = checkpoint.history_lines as usize;
        let inactive_cap = if checkpoint.modes.alternate_screen {
            checkpoint.alt_grid.as_deref().map_or(0, |bytes| {
                deserialize_lines(bytes)
                    .len()
                    .saturating_sub(usize::from(checkpoint.rows))
            })
        } else {
            0
        };
        let (kitty_store, kitty_numbers) = capture_kitty_carry(
            &c.kitty_images,
            &c.kitty_frames,
            &c.kitty_variants,
            &c.kitty_virtual,
            &c.kitty_numbers,
            origin,
        );
        let (active_img_cap, inactive_img_cap) = if checkpoint.modes.alternate_screen {
            (0, inactive_cap.min(PANE_HISTORY_IMAGE_LINES))
        } else {
            (active_cap.min(PANE_HISTORY_IMAGE_LINES), 0)
        };
        let bounded_active = c.active_images.bounded(active_img_cap);
        let bounded_inactive = c
            .inactive_images
            .as_ref()
            .map(|g| g.bounded(inactive_img_cap));
        let (loose_images, kitty_floating, kitty_relations, active_images, inactive_images) =
            capture_placed_carry(
                &c.kitty_images,
                &c.kitty_frames,
                &c.kitty_variants,
                &c.kitty_floating,
                &c.kitty_relations,
                &bounded_active,
                bounded_inactive.as_ref(),
            );
        Self {
            sync_start_ns: c.sync_start.map(|at| offset_ns_from(origin, at)),
            sync_timeout_ns: duration_to_ns_saturating(c.sync_timeout_duration),
            response_limiter: (
                canonical_f64_bits(rl_tokens),
                rl_cap,
                rl_refill,
                rl_last.map(|at| offset_ns_from(origin, at)),
            ),
            policy_buckets: c
                .policy
                .bucket_snapshots()
                .into_iter()
                .map(|b| {
                    (
                        canonical_f64_bits(b.tokens),
                        b.last_refill.map(|at| offset_ns_from(origin, at)),
                    )
                })
                .collect(),
            alt_entered_without_cursor_save: c.alt_entered_without_cursor_save,
            conhost_alt_screen_left_up: c.conhost_alt_screen_left_up,
            dead_alt_screen: c.dead_alt_screen,
            dead_alt_prompt_at: c.dead_alt_prompt_at,
            screen_replaced: c.screen_replaced,
            size_report_held: c.size_report_held,
            bracketed_paste_from_prompt: c.bracketed_paste_from_prompt,
            kitty_last_assigned_id: c.kitty_last_assigned_id,
            kitty_serial: c.kitty_serial,
            live_image_charge: c
                .live_image_charge
                .map(|n| u64::try_from(n).unwrap_or(u64::MAX)),
            history_image_room: u64::try_from(c.history_image_room).unwrap_or(u64::MAX),
            last_osc_bel_terminated: c.last_osc_bel_terminated,
            pending_restore,
            foreground_job: c.foreground_job,
            probe_job_active: !c.foreground_job && c.probe_job_active,
            awaiting_shell_nonce,
            typed_rekey_prior,
            active_handback: c.active_handback.bounded(active_cap),
            inactive_handback: c.inactive_handback.map(|h| h.bounded(inactive_cap)),
            dcs_query: None,
            kitty_store,
            kitty_numbers,
            kitty_pending: c.kitty_pending.clone(),
            loose_images,
            kitty_floating,
            kitty_relations,
            active_images,
            inactive_images,
        }
    }
}

fn capture_dcs_query(term: &Terminal) -> Option<DcsQueryCarry> {
    match (term.hooked_dcs(), term.parser.hooked_dcs_carry()) {
        (Some(HookedDcs::Query), Some(parser)) => {
            let kind = match term.dcs.dcs_type {
                super::DcsType::Xtgettcap => DcsQueryKind::Xtgettcap,
                super::DcsType::Decrqss
                | super::DcsType::None
                | super::DcsType::Sixel
                | super::DcsType::Decrsps { .. }
                | super::DcsType::Unknown => DcsQueryKind::Decrqss,
            };
            Some(DcsQueryCarry {
                kind,
                data: term.dcs.data.clone(),
                sequence_bytes: u32::try_from(term.dcs.sequence_bytes).unwrap_or(u32::MAX),
                parser,
            })
        }
        _ => None,
    }
}

fn capture_kitty_carry(
    kitty_images: &std::collections::HashMap<u32, std::sync::Arc<aterm_types::ImageData>>,
    kitty_frames: &std::collections::HashMap<u32, super::transient_state::KittyAnimation>,
    kitty_variants: &std::collections::HashMap<u32, Vec<super::transient_state::KittyVariant>>,
    kitty_virtual: &std::collections::HashMap<u32, Vec<super::transient_state::KittyVirtual>>,
    kitty_numbers: &std::collections::HashMap<u32, u32>,
    origin: aterm_time::Instant,
) -> (Vec<KittyImageCarry>, Vec<(u32, u32)>) {
    let mut ids: Vec<u32> = kitty_frames
        .keys()
        .copied()
        .filter(|&id| id != 0 && kitty_images.contains_key(&id))
        .collect();
    ids.sort_unstable();
    let store = ids
        .into_iter()
        .filter_map(|id| {
            capture_kitty_entry(
                id,
                kitty_images.get(&id)?,
                kitty_frames.get(&id)?,
                kitty_variants.get(&id).map_or(&[][..], Vec::as_slice),
                kitty_virtual.get(&id).map_or(&[][..], Vec::as_slice),
                origin,
            )
        })
        .collect();
    let mut numbers: Vec<(u32, u32)> = kitty_numbers
        .iter()
        .filter(|&(&num, &id)| {
            num != 0 && id != 0 && kitty_images.contains_key(&id) && kitty_frames.contains_key(&id)
        })
        .map(|(&num, &id)| (num, id))
        .collect();
    numbers.sort_unstable_by_key(|&(num, _)| num);
    (store, numbers)
}

fn capture_kitty_entry(
    id: u32,
    shown_img: &std::sync::Arc<aterm_types::ImageData>,
    anim: &super::transient_state::KittyAnimation,
    raw_variants: &[super::transient_state::KittyVariant],
    raw_virtuals: &[super::transient_state::KittyVirtual],
    origin: aterm_time::Instant,
) -> Option<KittyImageCarry> {
    use std::sync::Arc;
    if anim.frames.is_empty() {
        return None;
    }
    let shown_idx = anim
        .frames
        .iter()
        .position(|f| Arc::ptr_eq(&f.image, shown_img))
        .unwrap_or(0);
    let frames: Vec<KittyFrameCarry> = anim
        .frames
        .iter()
        .map(|f| KittyFrameCarry {
            bytes: f.image.bytes.clone(),
            format: f.image.format,
            layout: KittyLayoutCarry::of(&f.image),
            gap_ms: f.gap_ms,
        })
        .collect();
    let variants: Vec<KittyVariantCarry> = raw_variants
        .iter()
        .map(|v| {
            let source = match anim
                .frames
                .iter()
                .position(|f| Arc::ptr_eq(&f.image, &v.source))
            {
                Some(idx) => KittyVariantSourceCarry::Frame(u8::try_from(idx).unwrap_or(0)),
                None => KittyVariantSourceCarry::Orphan {
                    bytes: v.image.bytes.clone(),
                    format: v.image.format,
                },
            };
            KittyVariantCarry {
                source,
                layout: KittyLayoutCarry::of(&v.image),
            }
        })
        .collect();
    let virtuals: Vec<KittyVirtualCarry> = raw_virtuals
        .iter()
        .map(|virt| {
            let image = if let Some(idx) = anim
                .frames
                .iter()
                .position(|f| Arc::ptr_eq(&f.image, &virt.image))
            {
                KittyVirtualImageCarry::Frame(u8::try_from(idx).unwrap_or(0))
            } else if let Some(idx) = raw_variants
                .iter()
                .position(|v| Arc::ptr_eq(&v.image, &virt.image))
            {
                KittyVirtualImageCarry::Variant(u8::try_from(idx).unwrap_or(0))
            } else {
                KittyVirtualImageCarry::Frame(u8::try_from(shown_idx).unwrap_or(0))
            };
            KittyVirtualCarry {
                placement_id: virt.tag.placement_id,
                image_serial: virt.tag.image_serial,
                serial: virt.tag.serial,
                image,
            }
        })
        .collect();
    Some(KittyImageCarry {
        id,
        image_serial: anim.image_serial,
        shown_frame: u8::try_from(shown_idx).unwrap_or(0),
        state: KittyPlayStateCarry::from_play_state(anim.state),
        max_loops: anim.max_loops,
        loops: anim.loops,
        shown_at_ns: offset_ns_from(origin, anim.shown_at),
        frames,
        variants,
        virtuals,
    })
}

fn apply_kitty_carry(
    to: &mut super::TransientState,
    carry: &PaneStepCarry,
    origin: aterm_time::Instant,
) {
    use super::transient_state::{KittyAnimation, KittyFrame, KittyVariant, KittyVirtual};
    use std::sync::Arc;
    to.kitty_images.clear();
    to.kitty_frames.clear();
    to.kitty_variants.clear();
    to.kitty_virtual.clear();
    to.kitty_relations.clear();
    to.kitty_floating.clear();
    to.kitty_numbers.clear();
    let mut total_bytes = 0usize;
    for entry in &carry.kitty_store {
        if entry.id == 0 || entry.frames.is_empty() {
            continue;
        }
        let mut frames = Vec::with_capacity(entry.frames.len());
        for fc in &entry.frames {
            let img = Arc::new(fc.layout.into_image(fc.bytes.clone(), fc.format));
            total_bytes = total_bytes.saturating_add(img.bytes.len());
            frames.push(KittyFrame {
                image: img,
                gap_ms: fc.gap_ms,
                undecodable: false,
            });
        }
        let shown_idx = usize::from(entry.shown_frame).min(frames.len() - 1);
        let shown_arc = Arc::clone(&frames[shown_idx].image);
        total_bytes = total_bytes.saturating_add(shown_arc.bytes.len());
        to.kitty_images.insert(entry.id, shown_arc);

        let mut restored_variants = Vec::with_capacity(entry.variants.len());
        for vc in &entry.variants {
            let variant = match &vc.source {
                KittyVariantSourceCarry::Frame(idx) => {
                    let src_idx = usize::from(*idx).min(frames.len() - 1);
                    let src_arc = Arc::clone(&frames[src_idx].image);
                    let img = Arc::new(vc.layout.into_image(src_arc.bytes.clone(), src_arc.format));
                    total_bytes = total_bytes.saturating_add(img.bytes.len());
                    KittyVariant {
                        source: src_arc,
                        image: img,
                    }
                }
                KittyVariantSourceCarry::Orphan { bytes, format } => {
                    let dummy_source = Arc::new(vc.layout.into_image(Vec::new(), *format));
                    let img = Arc::new(vc.layout.into_image(bytes.clone(), *format));
                    total_bytes = total_bytes.saturating_add(img.bytes.len());
                    KittyVariant {
                        source: dummy_source,
                        image: img,
                    }
                }
            };
            restored_variants.push(variant);
        }

        let mut restored_virtuals = Vec::with_capacity(entry.virtuals.len());
        for virt in &entry.virtuals {
            let maybe_img = match virt.image {
                KittyVirtualImageCarry::Frame(idx) => {
                    frames.get(usize::from(idx)).map(|f| Arc::clone(&f.image))
                }
                KittyVirtualImageCarry::Variant(idx) => restored_variants
                    .get(usize::from(idx))
                    .map(|v| Arc::clone(&v.image)),
            };
            if let Some(image) = maybe_img {
                restored_virtuals.push(KittyVirtual {
                    tag: aterm_grid::KittyPlacementTag {
                        image_id: entry.id,
                        placement_id: virt.placement_id,
                        image_serial: virt.image_serial,
                        serial: virt.serial,
                    },
                    image,
                });
            }
        }
        if !restored_variants.is_empty() {
            to.kitty_variants.insert(entry.id, restored_variants);
        }
        if !restored_virtuals.is_empty() {
            to.kitty_virtual.insert(entry.id, restored_virtuals);
        }
        to.kitty_frames.insert(
            entry.id,
            KittyAnimation {
                frames,
                state: entry.state.into_play_state(),
                max_loops: entry.max_loops,
                loops: entry.loops,
                shown_at: instant_at_offset(origin, entry.shown_at_ns),
                image_serial: entry.image_serial,
            },
        );
    }
    for &(num, id) in &carry.kitty_numbers {
        if num != 0 && to.kitty_images.contains_key(&id) {
            to.kitty_numbers.insert(num, id);
        }
    }
    to.kitty_total_bytes = total_bytes;
    to.kitty_pending.clone_from(&carry.kitty_pending);
}

fn capture_placed_carry(
    kitty_images: &std::collections::HashMap<u32, std::sync::Arc<aterm_types::ImageData>>,
    kitty_frames: &std::collections::HashMap<u32, super::transient_state::KittyAnimation>,
    kitty_variants: &std::collections::HashMap<u32, Vec<super::transient_state::KittyVariant>>,
    kitty_floating: &[super::transient_state::KittyFloating],
    kitty_relations: &[super::transient_state::KittyRelation],
    active_images: &aterm_grid::GridImageCarry,
    inactive_images: Option<&aterm_grid::GridImageCarry>,
) -> (
    Vec<LooseImageCarry>,
    Vec<KittyFloatingCarry>,
    Vec<KittyRelationCarry>,
    PlacedGridCarry,
    Option<PlacedGridCarry>,
) {
    use std::sync::Arc;
    if kitty_floating.is_empty()
        && kitty_relations.is_empty()
        && active_images.is_empty()
        && inactive_images.is_none_or(aterm_grid::GridImageCarry::is_empty)
    {
        return (
            Vec::new(),
            Vec::new(),
            Vec::new(),
            PlacedGridCarry::default(),
            inactive_images.map(|_| PlacedGridCarry::default()),
        );
    }

    let store_by_ptr = build_kitty_source_index(kitty_images, kitty_frames, kitty_variants);
    let mut loose_by_ptr: std::collections::HashMap<usize, u32> = std::collections::HashMap::new();
    let mut loose_images: Vec<LooseImageCarry> = Vec::new();
    let mut intern_source = |img: &Arc<aterm_types::ImageData>| -> PlacedImageSourceCarry {
        let ptr = Arc::as_ptr(img) as usize;
        if let Some(&src) = store_by_ptr.get(&ptr) {
            return src;
        }
        if let Some(&idx) = loose_by_ptr.get(&ptr) {
            return PlacedImageSourceCarry::Loose(idx);
        }
        let idx = u32::try_from(loose_images.len()).unwrap_or(u32::MAX);
        loose_images.push(LooseImageCarry {
            bytes: img.bytes.clone(),
            format: img.format,
            layout: KittyLayoutCarry::of(img),
        });
        loose_by_ptr.insert(ptr, idx);
        PlacedImageSourceCarry::Loose(idx)
    };

    let floating: Vec<KittyFloatingCarry> = kitty_floating
        .iter()
        .filter_map(|fl| {
            let parent = KittyFloatParentCarry::from_float_parent(fl.parent)?;
            let image = intern_source(&fl.image);
            Some(KittyFloatingCarry {
                tag: fl.tag,
                image,
                parent,
                dx: fl.dx,
                dy: fl.dy,
            })
        })
        .collect();

    let relations: Vec<KittyRelationCarry> = kitty_relations
        .iter()
        .filter_map(|rel| {
            let parent = KittyParentCarry::from_parent(rel.parent)?;
            Some(KittyRelationCarry {
                child: rel.child,
                parent,
                dx: rel.dx,
                dy: rel.dy,
            })
        })
        .collect();

    let active_placed = map_placed_grid(active_images, &mut intern_source);
    let inactive_placed = inactive_images.map(|g| map_placed_grid(g, &mut intern_source));

    (
        loose_images,
        floating,
        relations,
        active_placed,
        inactive_placed,
    )
}

fn build_kitty_source_index(
    kitty_images: &std::collections::HashMap<u32, std::sync::Arc<aterm_types::ImageData>>,
    kitty_frames: &std::collections::HashMap<u32, super::transient_state::KittyAnimation>,
    kitty_variants: &std::collections::HashMap<u32, Vec<super::transient_state::KittyVariant>>,
) -> std::collections::HashMap<usize, PlacedImageSourceCarry> {
    use std::sync::Arc;
    let mut ids: Vec<u32> = kitty_frames
        .keys()
        .copied()
        .filter(|&id| id != 0 && kitty_images.contains_key(&id))
        .collect();
    ids.sort_unstable();
    let mut store_by_ptr = std::collections::HashMap::new();
    for id in ids {
        let Some(anim) = kitty_frames.get(&id) else {
            continue;
        };
        if anim.frames.is_empty() {
            continue;
        }
        for (f_idx, f) in anim.frames.iter().enumerate() {
            if let Ok(frame) = u8::try_from(f_idx) {
                store_by_ptr
                    .entry(Arc::as_ptr(&f.image) as usize)
                    .or_insert(PlacedImageSourceCarry::KittyFrame {
                        image_id: id,
                        frame,
                    });
            }
        }
        if let Some(variants) = kitty_variants.get(&id) {
            for (v_idx, v) in variants.iter().enumerate() {
                if let Ok(variant) = u8::try_from(v_idx) {
                    store_by_ptr
                        .entry(Arc::as_ptr(&v.image) as usize)
                        .or_insert(PlacedImageSourceCarry::KittyVariant {
                            image_id: id,
                            variant,
                        });
                }
            }
        }
    }
    store_by_ptr
}

fn map_placed_grid(
    grid: &aterm_grid::GridImageCarry,
    intern: &mut dyn FnMut(&std::sync::Arc<aterm_types::ImageData>) -> PlacedImageSourceCarry,
) -> PlacedGridCarry {
    let mut map_spans = |spans: &[aterm_scrollback::ImageSpan]| -> Vec<PlacedSpanCarry> {
        spans
            .iter()
            .map(|sp| PlacedSpanCarry {
                start_col: sp.start_col,
                end_col: sp.end_col,
                image_row: sp.image_row,
                first_cell_col: sp.first_cell_col,
                image: intern(&sp.image),
                kitty: sp.kitty,
            })
            .collect()
    };
    let visible = grid
        .visible
        .iter()
        .map(|&(row, ref spans)| (row, map_spans(spans)))
        .collect();
    let history_rev = grid
        .history_rev
        .iter()
        .map(|&(rev, ref spans)| (rev, map_spans(spans)))
        .collect();
    PlacedGridCarry {
        visible,
        history_rev,
    }
}

fn resolve_placed_grid_carry(
    grid: &PlacedGridCarry,
    resolve: &dyn Fn(PlacedImageSourceCarry) -> Option<std::sync::Arc<aterm_types::ImageData>>,
) -> aterm_grid::GridImageCarry {
    let map_spans = |spans: &[PlacedSpanCarry]| -> Vec<aterm_scrollback::ImageSpan> {
        spans
            .iter()
            .filter_map(|sp| {
                let image = resolve(sp.image)?;
                Some(aterm_scrollback::ImageSpan {
                    image,
                    start_col: sp.start_col,
                    end_col: sp.end_col,
                    image_row: sp.image_row,
                    first_cell_col: sp.first_cell_col,
                    kitty: sp.kitty,
                })
            })
            .collect()
    };
    let visible = grid
        .visible
        .iter()
        .filter_map(|&(row, ref spans)| {
            let resolved = map_spans(spans);
            (!resolved.is_empty()).then_some((row, resolved))
        })
        .collect();
    let history_rev = grid
        .history_rev
        .iter()
        .filter_map(|&(rev, ref spans)| {
            let resolved = map_spans(spans);
            (!resolved.is_empty()).then_some((rev, resolved))
        })
        .collect();
    aterm_grid::GridImageCarry {
        visible,
        history_rev,
    }
}

fn kitty_maps_total_bytes(
    kitty_images: &std::collections::HashMap<u32, std::sync::Arc<aterm_types::ImageData>>,
    kitty_frames: &std::collections::HashMap<u32, super::transient_state::KittyAnimation>,
    kitty_variants: &std::collections::HashMap<u32, Vec<super::transient_state::KittyVariant>>,
) -> usize {
    let imgs = kitty_images
        .values()
        .fold(0usize, |sum, img| sum.saturating_add(img.bytes.len()));
    let frms = kitty_frames.values().fold(0usize, |sum, anim| {
        anim.frames
            .iter()
            .fold(sum, |acc, f| acc.saturating_add(f.image.bytes.len()))
    });
    let vars = kitty_variants.values().fold(0usize, |sum, vs| {
        vs.iter()
            .fold(sum, |acc, v| acc.saturating_add(v.image.bytes.len()))
    });
    imgs.saturating_add(frms).saturating_add(vars)
}

#[derive(Clone, Copy, Debug)]
enum PendingRestore {
    None,
    Untouched,
    Touched,
}

super::checkpoint_heap::no_heap!(PendingRestore);

#[derive(Clone, Debug)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "independent transient terminal flags carried across an in-process replay keyframe"
)]
struct CapturedCarry {
    program_state: super::program_state::ProgramStateReport,
    shell_integration_auth: super::shell_integration_auth::ShellIntegrationAuth,
    foreground_job: bool,
    probe_job_active: bool,
    policy: super::policy_gates::PolicyState,
    hyperlink_auth: super::hyperlink_auth::HyperlinkAuth,
    clipboard_auth: super::clipboard_auth::ClipboardAuth,
    kitty_media_admitted: bool,
    configured_modes: super::state::ConfiguredModes,
    default_cursor_style: aterm_types::CursorStyle,
    host_alternate_scroll: bool,
    sync_timeout_duration: std::time::Duration,
    last_bell_time: Option<aterm_time::Instant>,
    bell_total: u64,
    notify_total: u64,
    cell_px: (u16, u16),
    cell_px_from_host: bool,
    bidi_config: crate::config::BiDiConfig,
    color_baseline: super::config_api::ColorBaseline,
    color_overrides: super::checkpoint_state::ColorOverridesRepr,
    bold_is_bright: bool,
    faint_opacity: f32,
    indexed_minimum_contrast: f32,
    palette_set_refused: bool,
    frame_foreground_authoritative: bool,
    frame_background_authoritative: bool,
    frame_cursor_authoritative: bool,
    scrollback_line_limit: Option<usize>,
    pending_restore: PendingRestore,
    response_rate_limiter: super::response_rate_limiter::ResponseRateLimiter,
    sync_start: Option<aterm_time::Instant>,
    alt_entered_without_cursor_save: bool,
    conhost_alt_screen_left_up: bool,
    dead_alt_screen: Option<u64>,
    dead_alt_prompt_at: Option<(u64, u16, u16)>,
    screen_replaced: u64,
    size_report_held: bool,
    bracketed_paste_from_prompt: bool,
    kitty_last_assigned_id: u32,
    kitty_serial: u32,
    live_image_charge: [usize; 2],
    history_image_room: usize,
    last_osc_bel_terminated: bool,
    kitty_images: std::collections::HashMap<u32, std::sync::Arc<aterm_types::ImageData>>,
    kitty_frames: std::collections::HashMap<u32, super::transient_state::KittyAnimation>,
    kitty_variants: std::collections::HashMap<u32, Vec<super::transient_state::KittyVariant>>,
    kitty_virtual: std::collections::HashMap<u32, Vec<super::transient_state::KittyVirtual>>,
    kitty_numbers: std::collections::HashMap<u32, u32>,
    kitty_pending: Option<super::kitty_graphics::KittyCommand>,
    kitty_floating: Vec<super::transient_state::KittyFloating>,
    kitty_relations: Vec<super::transient_state::KittyRelation>,
    active_handback: aterm_grid::GridHandbackCarry,
    inactive_handback: Option<aterm_grid::GridHandbackCarry>,
    active_images: aterm_grid::GridImageCarry,
    inactive_images: Option<aterm_grid::GridImageCarry>,
}

impl super::checkpoint_heap::Heap for CapturedCarry {
    #[expect(
        clippy::too_many_lines,
        reason = "one exhaustive destructure and one term per field of the carry, so a field added \
                  later fails to compile until it is counted here"
    )]
    fn heap(&self) -> usize {
        let Self {
            program_state,
            shell_integration_auth,
            foreground_job,
            probe_job_active,
            policy,
            hyperlink_auth,
            clipboard_auth,
            kitty_media_admitted,
            configured_modes,
            default_cursor_style,
            host_alternate_scroll,
            sync_timeout_duration,
            last_bell_time,
            bell_total,
            notify_total,
            cell_px,
            cell_px_from_host,
            bidi_config,
            color_baseline,
            color_overrides,
            bold_is_bright,
            faint_opacity,
            indexed_minimum_contrast,
            palette_set_refused,
            frame_foreground_authoritative,
            frame_background_authoritative,
            frame_cursor_authoritative,
            scrollback_line_limit,
            pending_restore,
            response_rate_limiter,
            sync_start,
            alt_entered_without_cursor_save,
            conhost_alt_screen_left_up,
            dead_alt_screen,
            dead_alt_prompt_at,
            screen_replaced,
            size_report_held,
            bracketed_paste_from_prompt,
            kitty_last_assigned_id,
            kitty_serial,
            live_image_charge,
            history_image_room,
            last_osc_bel_terminated,
            kitty_images,
            kitty_frames,
            kitty_variants,
            kitty_virtual,
            kitty_numbers,
            kitty_pending,
            kitty_floating,
            kitty_relations,
            active_handback,
            inactive_handback,
            active_images,
            inactive_images,
        } = self;
        let super::config_api::ColorBaseline {
            foreground,
            background,
            cursor,
            selection_background,
            selection_foreground,
            palette,
            color_scheme,
        } = color_baseline;
        [
            program_state.heap(),
            shell_integration_auth.heap(),
            foreground_job.heap(),
            probe_job_active.heap(),
            policy.heap(),
            hyperlink_auth.heap(),
            clipboard_auth.heap(),
            kitty_media_admitted.heap(),
            configured_modes.heap(),
            default_cursor_style.heap(),
            host_alternate_scroll.heap(),
            sync_timeout_duration.heap(),
            last_bell_time.heap(),
            bell_total.heap(),
            notify_total.heap(),
            cell_px.heap(),
            cell_px_from_host.heap(),
            bidi_config.heap(),
            foreground.heap(),
            background.heap(),
            cursor.heap(),
            selection_background.heap(),
            selection_foreground.heap(),
            palette.heap(),
            color_scheme.heap(),
            color_overrides.heap(),
            bold_is_bright.heap(),
            faint_opacity.heap(),
            indexed_minimum_contrast.heap(),
            palette_set_refused.heap(),
            frame_foreground_authoritative.heap(),
            frame_background_authoritative.heap(),
            frame_cursor_authoritative.heap(),
            scrollback_line_limit.heap(),
            pending_restore.heap(),
            response_rate_limiter.heap(),
            sync_start.heap(),
            alt_entered_without_cursor_save.heap(),
            conhost_alt_screen_left_up.heap(),
            dead_alt_screen.heap(),
            dead_alt_prompt_at.heap(),
            screen_replaced.heap(),
            size_report_held.heap(),
            bracketed_paste_from_prompt.heap(),
            kitty_last_assigned_id.heap(),
            kitty_serial.heap(),
            live_image_charge.heap(),
            history_image_room.heap(),
            last_osc_bel_terminated.heap(),
            kitty_images.heap(),
            kitty_frames.heap(),
            kitty_variants.heap(),
            kitty_virtual.heap(),
            kitty_numbers.heap(),
            kitty_pending.heap(),
            kitty_floating.heap(),
            kitty_relations.heap(),
            active_handback.heap(),
            inactive_handback.heap(),
            active_images.heap(),
            inactive_images.heap(),
        ]
        .into_iter()
        .fold(0, usize::saturating_add)
    }
}

fn osc_payload_is_shell_mark(osc: &[u8]) -> bool {
    let Some(command) = osc
        .iter()
        .position(|b| *b == b';')
        .and_then(|end| osc.get(..end))
    else {
        return false;
    };
    std::str::from_utf8(command)
        .ok()
        .and_then(|s| s.parse::<u32>().ok())
        .is_some_and(|n| n == 133 || n == 633)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum HookedDcs {
    Nothing,
    Picture,
    Query,
    Restore,
}

#[derive(Clone, Copy, Debug)]
enum MarksFrom {
    Carried,
    Within(usize),
    All,
}

impl Terminal {
    /// Whether OSC 133/633 marks can reach this terminal ([`ShellIntegrationPosture`]).
    #[inline]
    #[must_use]
    pub const fn shell_integration_posture(&self) -> ShellIntegrationPosture {
        match (
            self.modes.require_shell_integration_nonce,
            self.shell_integration_auth.nonce_in_use().is_some(),
        ) {
            (false, _) => ShellIntegrationPosture::Off,
            (true, true) => ShellIntegrationPosture::On,
            (true, false) => ShellIntegrationPosture::Degraded,
        }
    }

    /// Capture a [`TerminalCheckpoint`]: a pure read, no host effects, no fs.
    #[inline]
    #[must_use]
    pub fn checkpoint(&self) -> TerminalCheckpoint {
        self.checkpoint_with_scrollback(true)
    }

    /// [`Self::checkpoint`] for an engine that may be parked inside a partial sequence.
    #[inline]
    #[must_use]
    pub fn checkpoint_continuing(&self) -> Option<TerminalCheckpoint> {
        self.checkpoint_continuing_bounded(usize::MAX)
    }

    /// [`Self::checkpoint_continuing`] with each grid's history bounded to its most recent `max_history` lines, at a capture cost of `O((rows + max_history) × cols)`.
    #[must_use]
    pub fn checkpoint_continuing_bounded(&self, max_history: usize) -> Option<TerminalCheckpoint> {
        self.partial_sequence_uncarried()
            .is_none()
            .then(|| self.project_bounded(max_history, max_history, self.parser_is_ground()))
    }

    /// [`Self::checkpoint_continuing_bounded`] for a pane step paired with a [`PaneStepCarry`] (`aterm-aos-pane`), which carries a hooked DCS query in [`PaneStepCarry::dcs_query`].
    #[must_use]
    pub fn checkpoint_pane_step(&self, max_history: usize) -> Option<TerminalCheckpoint> {
        (self.partial_sequence_uncarried().is_none() || self.hooked_dcs() == Some(HookedDcs::Query))
            .then(|| self.project_bounded(max_history, max_history, self.parser_is_ground()))
    }

    /// [`Self::checkpoint_continuing`] that never refuses.
    #[must_use]
    pub fn checkpoint_continuing_swallowing(&self) -> TerminalCheckpoint {
        let mut c = self.project(
            usize::MAX,
            usize::MAX,
            self.parser_is_ground(),
            true,
            MarksFrom::All,
        );
        if matches!(
            self.hooked_dcs(),
            Some(HookedDcs::Query | HookedDcs::Restore)
        ) {
            c.parser = Some(self.parser.carry_unhooking());
            c.parser_ground = false;
        }
        c
    }

    /// Capture the exact visible terminal state without scrollback.
    #[inline]
    #[must_use]
    pub fn checkpoint_visible(&self) -> Option<TerminalCheckpoint> {
        self.checkpoint_carry(0)
    }

    /// The seamless-handoff projection, carrying up to `max_history` lines of the history of the
    /// grid that holds it ([`Self::history_budgets`]).
    #[must_use]
    pub fn checkpoint_carry(&self, max_history: usize) -> Option<TerminalCheckpoint> {
        self.partial_sequence_uncarried().is_none().then(|| {
            let (active, inactive) = self.history_budgets(max_history);
            let mut c = self.project_bounded(active, inactive, true);
            c.shell_integration_nonce = self.carried_shell_integration_nonce();
            c.shell_integration_rev = self.shell_integration_rev().map(str::to_owned);
            c
        })
    }

    /// The crash-restore journal's keyframe projection (`aterm-restore`).
    #[must_use]
    pub fn checkpoint_journal(&self, bridge: usize) -> Option<TerminalCheckpoint> {
        (self.partial_sequence_uncarried().is_none()
            && self.partial_sequence_swallowed().is_none()
            && !self.partial_sequence_is_shell_mark())
        .then(|| {
            let (active, inactive) = self.history_budgets(bridge);
            self.project_bounded(active, inactive, self.parser_is_ground())
        })
    }

    /// `lines` of history as (active, inactive) budgets: all of them to the grid that holds the
    /// history, the active grid on the main screen and the saved primary (the inactive grid)
    /// while the alternate screen is up, which keeps none.
    const fn history_budgets(&self, lines: usize) -> (usize, usize) {
        if self.modes.alternate_screen {
            (0, lines)
        } else {
            (lines, 0)
        }
    }

    /// [`Self::checkpoint_journal`] whose completed shell marks and blocks reach `marks` history lines above the screen, whether or not the bridge carries those lines.
    #[must_use]
    pub fn checkpoint_journal_marked(
        &self,
        bridge: usize,
        marks: usize,
    ) -> Option<TerminalCheckpoint> {
        (self.partial_sequence_uncarried().is_none()
            && self.partial_sequence_swallowed().is_none()
            && !self.partial_sequence_is_shell_mark())
        .then(|| {
            let (active, inactive) = self.history_budgets(bridge);
            self.project(
                active,
                inactive,
                self.parser_is_ground(),
                true,
                MarksFrom::Within(marks),
            )
        })
    }

    /// [`Self::checkpoint_carry`] for a parser that may hold a partial sequence no carry can.
    #[must_use]
    pub fn checkpoint_carry_abandoning_partial(
        &self,
        max_history: usize,
    ) -> (TerminalCheckpoint, Option<&'static str>) {
        assert!(
            !crate::fault::triggered("checkpoint.carry_projection"),
            "fault injected: the carry projection failed"
        );
        let (active, inactive) = self.history_budgets(max_history);
        let mut carry = self.project_bounded(active, inactive, true);
        carry.shell_integration_nonce = self.carried_shell_integration_nonce();
        carry.shell_integration_rev = self.shell_integration_rev().map(str::to_owned);
        (carry, self.partial_sequence_uncarried())
    }

    /// The scalar half of [`Self::checkpoint_carry_abandoning_partial`]`(0)` without projecting either grid: that carry's [`CheckpointMeta`] with `visible_cells` and `alt_visible_cells`, which describe the grids, left empty.
    #[cfg(feature = "serde")]
    #[must_use]
    pub fn carry_meta_abandoning_partial(&self) -> CheckpointMeta {
        let mut scalars = self.project(0, 0, true, false, MarksFrom::Carried);
        scalars.shell_integration_nonce = self.carried_shell_integration_nonce();
        scalars.shell_integration_rev = self.shell_integration_rev().map(str::to_owned);
        CheckpointMeta::from_checkpoint(&scalars)
    }

    #[inline]
    fn carried_shell_integration_nonce(&self) -> Option<ShellIntegrationNonce> {
        self.shell_integration_auth
            .nonce_in_use()
            .map(ShellIntegrationNonce)
    }

    /// The parser state a partial escape sequence is sitting in (`CsiParam`, `OscString`, `SosPmApcString`, …; `TmuxPassthrough` for a tmux passthrough wrapper open at a payload's Ground), or `None` at Ground.
    #[inline]
    #[must_use]
    pub fn partial_sequence_state(&self) -> Option<&'static str> {
        let state = self.parser.state();
        if state.is_ground() && self.parser.tmux_passthrough().is_open() {
            return Some("TmuxPassthrough");
        }
        (!state.is_ground()).then(|| state.name())
    }

    /// The parser state of a partial sequence no carry can hold ([`Self::parser_carry`]), or `None` when carried ([`TerminalCheckpoint::parser`]).
    #[inline]
    #[must_use]
    pub fn partial_sequence_uncarried(&self) -> Option<&'static str> {
        self.parser_carry()
            .is_none()
            .then(|| self.parser.state().name())
    }

    #[inline]
    const fn hooked_dcs(&self) -> Option<HookedDcs> {
        if !self.parser.in_hooked_dcs() {
            return None;
        }
        Some(match self.dcs.dcs_type {
            super::DcsType::None | super::DcsType::Unknown => HookedDcs::Nothing,
            super::DcsType::Sixel => HookedDcs::Picture,
            super::DcsType::Decrqss | super::DcsType::Xtgettcap => HookedDcs::Query,
            super::DcsType::Decrsps { .. } => HookedDcs::Restore,
        })
    }

    #[inline]
    fn parser_carry(&self) -> Option<ParserCarry> {
        match self.hooked_dcs() {
            None => self.parser.carry(),
            Some(HookedDcs::Nothing | HookedDcs::Picture) => Some(self.parser.carry_unhooking()),
            Some(HookedDcs::Query | HookedDcs::Restore) => None,
        }
    }

    /// The parser state of a partial sequence the carry would only swallow where this engine would deliver it ([`aterm_parser::Parser::carry_swallows_sequence`]), or `None`.
    #[inline]
    #[must_use]
    pub fn partial_sequence_swallowed(&self) -> Option<&'static str> {
        (self.parser.carry_swallows_sequence() || self.hooked_dcs() == Some(HookedDcs::Picture))
            .then(|| self.parser.state().name())
    }

    /// Whether the parser sits inside an OSC 133/633 shell-integration mark past its command number.
    #[inline]
    #[must_use]
    pub fn partial_sequence_is_shell_mark(&self) -> bool {
        self.parser.state() == aterm_parser::State::OscString
            && self
                .parser_carry()
                .is_some_and(|carry| osc_payload_is_shell_mark(&carry.osc))
    }

    /// Whether the sequence [`Self::partial_sequence_swallowed`] names is a picture (a kitty-graphics APC, an OSC 1337 image, a hooked sixel string), which a graphics stream's next frame replaces ([`aterm_parser::Parser::carry_swallows_picture`]).
    #[inline]
    #[must_use]
    pub fn partial_sequence_swallows_picture(&self) -> bool {
        self.parser.carry_swallows_picture() || self.hooked_dcs() == Some(HookedDcs::Picture)
    }

    /// The engine state (not the parser's) that output still to come relies on and that is in flight, or `None`.
    #[inline]
    #[must_use]
    pub const fn output_state_in_flight(&self) -> Option<&'static str> {
        if !matches!(
            self.transient.vt52_cursor_state,
            super::Vt52CursorState::None
        ) {
            Some("Vt52CursorAddress")
        } else if self.transient.current_hyperlink.is_some() {
            Some("Osc8HyperlinkOpen")
        } else {
            None
        }
    }

    #[inline]
    fn arm_carried_sync_window(&mut self) {
        self.transient.sync_start = self
            .modes
            .synchronized_output
            .then_some(self.transient.process_now);
    }

    #[inline]
    fn carried_parser(&self) -> Option<ParserCarry> {
        self.parser_carry().filter(|carry| !carry.is_ground())
    }

    fn restore_parser_carry(&mut self, carry: Option<&ParserCarry>) {
        let Some(carry) = carry else {
            return;
        };
        if let Err(refusal) = self.parser.restore_carry(carry) {
            self.parser.reset();
            aterm_log::warn!(
                "checkpoint restore: the carried parser state was refused ({refusal}); the \
                 parser resumes at Ground"
            );
        }
    }

    /// Whether the engine holds an inactive grid, the one a checkpoint carries as `alt_grid` (the saved primary while the alternate screen is up, or the alternate grid kept after it was left).
    #[inline]
    #[must_use]
    pub const fn has_inactive_grid(&self) -> bool {
        self.alt_grid.is_some()
    }

    /// The inactive grid [`Self::has_inactive_grid`] names, so a caller can count the memory of both grids.
    #[inline]
    #[must_use]
    pub const fn inactive_grid(&self) -> Option<&Grid> {
        self.alt_grid.as_ref()
    }

    fn checkpoint_with_scrollback(&self, include_scrollback: bool) -> TerminalCheckpoint {
        let bound = if include_scrollback { usize::MAX } else { 0 };
        self.checkpoint_bounded(bound, bound)
    }

    fn checkpoint_bounded(
        &self,
        max_history: usize,
        inactive_max_history: usize,
    ) -> TerminalCheckpoint {
        debug_assert!(
            self.parser_is_ground(),
            "checkpoint() requires parser_is_ground() (B.3.3)"
        );
        self.project_bounded(max_history, inactive_max_history, self.parser_is_ground())
    }

    fn project_bounded(
        &self,
        max_history: usize,
        inactive_max_history: usize,
        parser_ground: bool,
    ) -> TerminalCheckpoint {
        self.project(
            max_history,
            inactive_max_history,
            parser_ground,
            true,
            MarksFrom::Carried,
        )
    }

    fn project(
        &self,
        max_history: usize,
        inactive_max_history: usize,
        parser_ground: bool,
        grids: bool,
        marks: MarksFrom,
    ) -> TerminalCheckpoint {
        let rows = self.grid.rows();
        let cols = self.grid.cols();
        let parser = self.carried_parser();
        let (grid_bytes, history_lines, visible_cells) = if grids {
            let grid_lines = self.grid.checkpoint_line_views_bounded(max_history);
            let history_lines = u32::try_from(
                grid_lines
                    .len()
                    .saturating_sub(usize::from(self.grid.rows())),
            )
            .unwrap_or(u32::MAX);
            (
                serialize_line_views(&grid_lines),
                history_lines,
                self.grid.checkpoint_visible_cells(),
            )
        } else {
            (Vec::new(), 0, VisibleCells::default())
        };
        let cursor = GridCursorRepr::capture(&self.grid);

        let (primary, primary_bound) = if self.modes.alternate_screen {
            (self.alt_grid.as_ref(), inactive_max_history)
        } else {
            (Some(&self.grid), max_history)
        };
        let oldest_carried_row = primary.map_or(0, |g| {
            let carried = g.scrollback_lines().min(primary_bound);
            let top = g.visible_to_absolute(0);
            match marks {
                MarksFrom::Carried => top.saturating_sub(carried as u64),
                MarksFrom::Within(lines) => top.saturating_sub(carried.max(lines) as u64),
                MarksFrom::All => 0,
            }
        });

        let (alt_grid, alt_cursor, alt_visible_cells) = match &self.alt_grid {
            Some(inactive) => {
                let (blob, cells) = if grids {
                    (
                        serialize_line_views(
                            &inactive.checkpoint_line_views_bounded(inactive_max_history),
                        ),
                        inactive.checkpoint_visible_cells(),
                    )
                } else {
                    (Vec::new(), VisibleCells::default())
                };
                (Some(blob), Some(GridCursorRepr::capture(inactive)), cells)
            }
            None => (None, None, VisibleCells::default()),
        };

        TerminalCheckpoint {
            rows,
            cols,
            grid: grid_bytes,
            history_lines,
            cursor,
            alt_grid,
            alt_cursor,
            visible_cells,
            alt_visible_cells,
            saved_cursor_main: self.cursor_save.main.map(SavedCursorRepr::capture),
            saved_cursor_alt: self.cursor_save.alt.map(SavedCursorRepr::capture),
            modes: self.modes,
            style: StyleRepr::capture(&self.style),
            charset: self.charset,
            kitty_keyboard: self.kitty_keyboard.snapshot(),
            xterm_keyboard: self.xterm_keyboard,
            taskbar_progress: self.taskbar_progress,
            secure_keyboard_entry: self.secure_keyboard_entry,
            current_working_directory: self.current_working_directory.clone(),
            parser_ground: parser_ground && parser.is_none(),
            parser,
            shell_integration_nonce: None,
            absolute_row_counter: self.grid.absolute_row_counter(),
            alt_absolute_row_counter: self.alt_grid.as_ref().map_or(0, Grid::absolute_row_counter),
            color: self.capture_color_repr(),
            shell: self.capture_shell_repr(oldest_carried_row),
            shell_integration_rev: None,
            title: Some(self.capture_title_repr()),
            output_state: self.capture_output_state_repr(),
            holder: self.capture_holder_repr(oldest_carried_row),
        }
    }

    /// Rebuild a fully-living [`Terminal`] from a checkpoint.
    #[must_use]
    pub fn from_checkpoint(c: &TerminalCheckpoint) -> Terminal {
        Self::from_checkpoint_onto(c, None)
    }

    /// [`Self::from_checkpoint`] with the main grid's tiered store unbounded (`store_lines` `None`) or kept to `store_lines` lines.
    fn from_checkpoint_onto(c: &TerminalCheckpoint, store_lines: Option<usize>) -> Terminal {
        let on_alt = c.modes.alternate_screen;
        let mut active_grid = restore_grid(
            c.rows,
            c.cols,
            &c.grid,
            &c.visible_cells,
            &c.cursor,
            on_alt,
            store_lines,
        );
        active_grid.continue_absolute_numbering(c.absolute_row_counter);
        let saved_grid = match (&c.alt_grid, &c.alt_cursor) {
            (Some(alt_bytes), Some(alt_cursor)) => {
                let mut saved = restore_grid(
                    c.rows,
                    c.cols,
                    alt_bytes,
                    &c.alt_visible_cells,
                    alt_cursor,
                    !on_alt,
                    store_lines,
                );
                saved.continue_absolute_numbering(c.alt_absolute_row_counter);
                Some(saved)
            }
            _ => None,
        };
        Self::restored(c, active_grid, saved_grid)
    }

    /// [`Self::from_checkpoint`] for replaying, in the process that took `c`, the bytes its engine parsed next while no restore of its own was pending: the restore adds no holder lineage (`c`'s seed uncertainty and tainted marks stay, and a later adoption is never untouched).
    #[must_use]
    pub fn replay_from_checkpoint(c: &TerminalCheckpoint) -> Terminal {
        let mut terminal = Self::from_checkpoint(c);
        terminal.holder_carried_at = None;
        terminal
    }

    /// [`Self::replay_from_checkpoint`] with the main grid's tiered store kept to `store_lines` lines past its ring, so [`Self::replay_fork`] copies it in bounded time.
    #[must_use]
    pub fn replay_from_checkpoint_bounded(c: &TerminalCheckpoint, store_lines: usize) -> Terminal {
        let mut terminal = Self::from_checkpoint_onto(c, Some(store_lines));
        terminal.holder_carried_at = None;
        terminal
    }

    /// Show this engine's screen `lines` rows higher, for a view whose bottom `lines` rows chrome covers: its top `lines` screen lines join its history (the alternate screen keeps just those, and a smaller history limit is raised to hold them), as many blank lines join its bottom, and the cursor moves up with its line, hidden once that line is history.
    pub fn lift_screen(&mut self, lines: u16) {
        let lines = lines.min(self.grid.rows().saturating_sub(1));
        if lines == 0 {
            return;
        }
        let keep = usize::from(lines);
        if self.modes.alternate_screen
            || self.grid.scrollback_line_limit().is_some_and(|n| n < keep)
        {
            self.grid.set_scrollback_line_limit(Some(keep));
        }
        let cursor = self.grid.cursor();
        self.grid.scroll_up(usize::from(lines));
        if cursor.row < lines {
            self.modes.cursor_visible = false;
        }
        self.grid
            .set_cursor(cursor.row.saturating_sub(lines), cursor.col);
    }

    /// Capture the in-process transient state a [`TerminalCheckpoint`] leaves out ([`ReplayCarry`]).
    #[must_use]
    pub fn capture_replay_carry(&self) -> ReplayCarry {
        ReplayCarry(Some(self.captured_carry()))
    }

    /// Install a [`ReplayCarry`] captured by [`Self::capture_replay_carry`] onto an engine restored by [`Self::replay_from_checkpoint`] or [`Self::replay_from_checkpoint_bounded`].
    pub fn apply_replay_carry(&mut self, carry: &ReplayCarry) {
        let Some(captured) = &carry.0 else {
            return;
        };
        self.apply_captured_scalars(captured);
        self.grid.apply_handback_carry(captured.active_handback);
        if let (Some(inactive), Some(handback)) = (&mut self.alt_grid, captured.inactive_handback) {
            inactive.apply_handback_carry(handback);
        }
        self.grid.apply_image_carry(&captured.active_images);
        if let (Some(inactive), Some(images)) = (&mut self.alt_grid, &captured.inactive_images) {
            inactive.apply_image_carry(images);
        }
    }

    /// Install a [`PaneStepCarry`] onto a checkpoint-restored engine whose host configuration and policy have already been applied, resolving offsets from `origin`.
    pub fn apply_pane_step_carry(&mut self, carry: &PaneStepCarry, origin: aterm_time::Instant) {
        self.sync_timeout_duration = std::time::Duration::from_nanos(carry.sync_timeout_ns);
        let snaps: Vec<aterm_policy::limits::BucketSnapshot> = carry
            .policy_buckets
            .iter()
            .map(|&(bits, last)| aterm_policy::limits::BucketSnapshot {
                tokens: f64::from_bits(bits),
                last_refill: last.map(|ns| instant_at_offset(origin, ns)),
            })
            .collect();
        self.policy.restore_bucket_snapshots(&snaps);
        let mutations = self.transient.screen_mutations;
        self.holder_carried_at = match carry.pending_restore {
            1 => Some(mutations),
            2 => Some(mutations.wrapping_sub(1)),
            _ => None,
        };
        self.shell.foreground_job = carry.foreground_job;
        self.shell.probe_job_active = !carry.foreground_job && carry.probe_job_active;
        self.shell_integration_auth
            .apply_awaiting_carry(carry.awaiting_shell_nonce, carry.typed_rekey_prior);
        let (rl_bits, rl_cap, rl_refill, rl_last) = carry.response_limiter;
        let to = &mut self.transient;
        to.response_rate_limiter = super::response_rate_limiter::ResponseRateLimiter::from_snapshot(
            f64::from_bits(rl_bits),
            rl_cap,
            rl_refill,
            rl_last.map(|ns| instant_at_offset(origin, ns)),
        );
        to.sync_start = carry.sync_start_ns.map(|ns| instant_at_offset(origin, ns));
        to.alt_entered_without_cursor_save = carry.alt_entered_without_cursor_save;
        to.conhost_alt_screen_left_up = carry.conhost_alt_screen_left_up;
        to.dead_alt_screen = carry.dead_alt_screen;
        to.dead_alt_prompt_at = carry.dead_alt_prompt_at;
        to.screen_replaced = carry.screen_replaced;
        to.size_report_held = carry.size_report_held;
        to.bracketed_paste_from_prompt = carry.bracketed_paste_from_prompt;
        to.kitty_last_assigned_id = carry.kitty_last_assigned_id;
        to.kitty_serial = carry.kitty_serial;
        to.live_image_charge = carry
            .live_image_charge
            .map(|n| usize::try_from(n).unwrap_or(usize::MAX));
        to.history_image_room = usize::try_from(carry.history_image_room).unwrap_or(usize::MAX);
        to.last_osc_bel_terminated = carry.last_osc_bel_terminated;
        apply_kitty_carry(to, carry, origin);
        self.apply_placed_carry(carry);
        self.grid.apply_handback_carry(carry.active_handback);
        if let (Some(inactive), Some(handback)) = (&mut self.alt_grid, carry.inactive_handback) {
            inactive.apply_handback_carry(handback);
        }
        if let Some(q) = &carry.dcs_query {
            if let Err(refusal) = self.parser.restore_hooked_dcs(&q.parser) {
                self.dcs.reset();
                self.parser.reset();
                aterm_log::warn!(
                    "pane step carry: the carried hooked DCS state was refused ({refusal}); \
                     the parser resumes at Ground"
                );
            } else {
                self.dcs.dcs_type = match q.kind {
                    DcsQueryKind::Decrqss => super::DcsType::Decrqss,
                    DcsQueryKind::Xtgettcap => super::DcsType::Xtgettcap,
                };
                self.dcs.data.clone_from(&q.data);
                let seq = usize::try_from(q.sequence_bytes).unwrap_or(usize::MAX);
                self.dcs.sequence_bytes = seq;
                self.dcs.total_bytes = seq;
            }
        } else if self.hooked_dcs() == Some(HookedDcs::Query) {
            self.dcs.reset();
            self.parser.reset();
        }
        self.refresh_mode_mirror();
    }

    fn apply_placed_carry(&mut self, carry: &PaneStepCarry) {
        use super::transient_state::{KittyFloating, KittyRelation};
        use std::sync::Arc;
        if carry.loose_images.is_empty()
            && carry.kitty_floating.is_empty()
            && carry.kitty_relations.is_empty()
            && carry.active_images.visible.is_empty()
            && carry.active_images.history_rev.is_empty()
            && carry
                .inactive_images
                .as_ref()
                .is_none_or(|g| g.visible.is_empty() && g.history_rev.is_empty())
        {
            return;
        }
        let loose_arcs: Vec<Arc<aterm_types::ImageData>> = carry
            .loose_images
            .iter()
            .map(|lc| Arc::new(lc.layout.into_image(lc.bytes.clone(), lc.format)))
            .collect();
        let active_resolved;
        let inactive_resolved;
        {
            let frames = &self.transient.kitty_frames;
            let variants = &self.transient.kitty_variants;
            let resolve = |src: PlacedImageSourceCarry| -> Option<Arc<aterm_types::ImageData>> {
                match src {
                    PlacedImageSourceCarry::KittyFrame { image_id, frame } => frames
                        .get(&image_id)?
                        .frames
                        .get(usize::from(frame))
                        .map(|f| Arc::clone(&f.image)),
                    PlacedImageSourceCarry::KittyVariant { image_id, variant } => variants
                        .get(&image_id)?
                        .get(usize::from(variant))
                        .map(|v| Arc::clone(&v.image)),
                    PlacedImageSourceCarry::Loose(idx) => {
                        loose_arcs.get(usize::try_from(idx).ok()?).cloned()
                    }
                }
            };
            let floating: Vec<KittyFloating> = carry
                .kitty_floating
                .iter()
                .filter_map(|fc| {
                    let image = resolve(fc.image)?;
                    Some(KittyFloating {
                        tag: fc.tag,
                        image,
                        parent: fc.parent.into_float_parent(),
                        dx: fc.dx,
                        dy: fc.dy,
                    })
                })
                .collect();
            let relations: Vec<KittyRelation> = carry
                .kitty_relations
                .iter()
                .map(|rc| KittyRelation {
                    child: rc.child,
                    parent: rc.parent.into_parent(),
                    dx: rc.dx,
                    dy: rc.dy,
                })
                .collect();
            active_resolved = resolve_placed_grid_carry(&carry.active_images, &resolve);
            inactive_resolved = carry
                .inactive_images
                .as_ref()
                .map(|g| resolve_placed_grid_carry(g, &resolve));
            self.transient.kitty_floating = floating;
            self.transient.kitty_relations = relations;
        }
        self.grid.apply_image_carry(&active_resolved);
        if let (Some(inactive), Some(images)) = (&mut self.alt_grid, &inactive_resolved) {
            inactive.apply_image_carry(images);
        }
    }

    fn captured_carry(&self) -> CapturedCarry {
        let t = &self.transient;
        let pending_restore = match self.holder_carried_at {
            None => PendingRestore::None,
            Some(at) if at == t.screen_mutations => PendingRestore::Untouched,
            Some(_) => PendingRestore::Touched,
        };
        CapturedCarry {
            program_state: self.program_state,
            shell_integration_auth: self.shell_integration_auth.clone(),
            foreground_job: self.shell.foreground_job,
            probe_job_active: !self.shell.foreground_job && self.shell.probe_job_active,
            policy: self.policy.clone(),
            hyperlink_auth: self.hyperlink_auth.clone(),
            clipboard_auth: self.clipboard_auth.clone(),
            kitty_media_admitted: self.kitty_file_resolver_installed(),
            configured_modes: self.configured_modes,
            default_cursor_style: self.default_cursor_style,
            host_alternate_scroll: self.host_alternate_scroll,
            sync_timeout_duration: self.sync_timeout_duration,
            last_bell_time: self.last_bell_time,
            bell_total: self.bell_total,
            notify_total: self.notify_total,
            cell_px: self.iterm2.cell_px,
            cell_px_from_host: self.iterm2.cell_px_from_host,
            bidi_config: self.bidi_state.config.clone(),
            color_baseline: self.color_baseline(),
            color_overrides: self.color.capture_overrides(),
            bold_is_bright: self.color.bold_is_bright,
            faint_opacity: self.color.faint_opacity,
            indexed_minimum_contrast: self.color.indexed_minimum_contrast,
            palette_set_refused: self.color.palette_set_refused,
            frame_foreground_authoritative: self.color.frame_foreground_authoritative,
            frame_background_authoritative: self.color.frame_background_authoritative,
            frame_cursor_authoritative: self.color.frame_cursor_authoritative,
            scrollback_line_limit: self.scrollback_line_limit(),
            pending_restore,
            response_rate_limiter: t.response_rate_limiter.clone(),
            sync_start: t.sync_start,
            alt_entered_without_cursor_save: t.alt_entered_without_cursor_save,
            conhost_alt_screen_left_up: t.conhost_alt_screen_left_up,
            dead_alt_screen: t.dead_alt_screen,
            dead_alt_prompt_at: t.dead_alt_prompt_at,
            screen_replaced: t.screen_replaced,
            size_report_held: t.size_report_held,
            bracketed_paste_from_prompt: t.bracketed_paste_from_prompt,
            kitty_last_assigned_id: t.kitty_last_assigned_id,
            kitty_serial: t.kitty_serial,
            live_image_charge: t.live_image_charge,
            history_image_room: t.history_image_room,
            last_osc_bel_terminated: t.last_osc_bel_terminated,
            kitty_images: t.kitty_images.clone(),
            kitty_frames: t.kitty_frames.clone(),
            kitty_variants: t.kitty_variants.clone(),
            kitty_virtual: t.kitty_virtual.clone(),
            kitty_numbers: t.kitty_numbers.clone(),
            kitty_pending: t.kitty_pending.clone(),
            kitty_floating: t.kitty_floating.clone(),
            kitty_relations: t.kitty_relations.clone(),
            active_handback: self.grid.capture_handback_carry(),
            inactive_handback: self.alt_grid.as_ref().map(Grid::capture_handback_carry),
            active_images: self.grid.capture_image_carry(usize::MAX),
            inactive_images: self
                .alt_grid
                .as_ref()
                .map(|g| g.capture_image_carry(usize::MAX)),
        }
    }

    fn apply_captured_scalars(&mut self, captured: &CapturedCarry) {
        self.program_state = captured.program_state;
        self.shell_integration_auth
            .clone_from(&captured.shell_integration_auth);
        self.shell.foreground_job = captured.foreground_job;
        self.shell.probe_job_active = !captured.foreground_job && captured.probe_job_active;
        self.policy.clone_from(&captured.policy);
        self.hyperlink_auth.clone_from(&captured.hyperlink_auth);
        self.clipboard_auth.clone_from(&captured.clipboard_auth);
        if captured.kitty_media_admitted {
            self.set_kitty_file_resolver(|_, _| None);
        } else {
            self.clear_kitty_file_resolver();
        }
        self.configured_modes = captured.configured_modes;
        self.default_cursor_style = captured.default_cursor_style;
        self.host_alternate_scroll = captured.host_alternate_scroll;
        self.sync_timeout_duration = captured.sync_timeout_duration;
        self.last_bell_time = captured.last_bell_time;
        self.bell_total = captured.bell_total;
        self.notify_total = captured.notify_total;
        self.iterm2.cell_px = captured.cell_px;
        self.iterm2.cell_px_from_host = captured.cell_px_from_host;
        self.bidi_state.config.clone_from(&captured.bidi_config);
        {
            let b = &captured.color_baseline;
            self.color.configured_cursor = b.cursor;
            self.color.configured_foreground = b.foreground;
            self.color.configured_background = b.background;
            if let Some(scheme) = b.color_scheme {
                self.color.configured_color_scheme = scheme;
            }
            self.color.configured_selection_background = b.selection_background;
            self.color.configured_selection_foreground = b.selection_foreground;
            self.color.configured_palette.clone_from(&b.palette);
            self.color.restore_baseline(true);
            self.color.apply_overrides(&captured.color_overrides, true);
            self.color.bold_is_bright = captured.bold_is_bright;
            self.color.faint_opacity = captured.faint_opacity;
            self.color.indexed_minimum_contrast = captured.indexed_minimum_contrast;
            self.color.palette_set_refused = captured.palette_set_refused;
            self.color.frame_foreground_authoritative = captured.frame_foreground_authoritative;
            self.color.frame_background_authoritative = captured.frame_background_authoritative;
            self.color.frame_cursor_authoritative = captured.frame_cursor_authoritative;
        }
        let limit = match (self.scrollback_line_limit(), captured.scrollback_line_limit) {
            (Some(current), Some(carried)) => Some(current.min(carried)),
            (Some(current), None) => Some(current),
            (None, carried) => carried,
        };
        if self.scrollback_line_limit() != limit {
            self.set_scrollback_line_limit(limit);
        }
        let mutations = self.transient.screen_mutations;
        self.holder_carried_at = match captured.pending_restore {
            PendingRestore::None => None,
            PendingRestore::Untouched => Some(mutations),
            PendingRestore::Touched => Some(mutations.wrapping_sub(1)),
        };
        let to = &mut self.transient;
        to.response_rate_limiter = captured.response_rate_limiter.clone();
        to.sync_start = captured.sync_start;
        to.alt_entered_without_cursor_save = captured.alt_entered_without_cursor_save;
        to.conhost_alt_screen_left_up = captured.conhost_alt_screen_left_up;
        to.dead_alt_screen = captured.dead_alt_screen;
        to.dead_alt_prompt_at = captured.dead_alt_prompt_at;
        to.screen_replaced = captured.screen_replaced;
        to.size_report_held = captured.size_report_held;
        to.bracketed_paste_from_prompt = captured.bracketed_paste_from_prompt;
        to.kitty_last_assigned_id = captured.kitty_last_assigned_id;
        to.kitty_serial = captured.kitty_serial;
        to.live_image_charge = captured.live_image_charge;
        to.history_image_room = captured.history_image_room;
        to.last_osc_bel_terminated = captured.last_osc_bel_terminated;
        to.kitty_images.clone_from(&captured.kitty_images);
        to.kitty_frames.clone_from(&captured.kitty_frames);
        to.kitty_variants.clone_from(&captured.kitty_variants);
        to.kitty_virtual.clone_from(&captured.kitty_virtual);
        to.kitty_numbers.clone_from(&captured.kitty_numbers);
        to.kitty_pending.clone_from(&captured.kitty_pending);
        to.kitty_relations.clone_from(&captured.kitty_relations);
        to.kitty_floating.clone_from(&captured.kitty_floating);
        to.kitty_total_bytes =
            kitty_maps_total_bytes(&to.kitty_images, &to.kitty_frames, &to.kitty_variants);
        self.refresh_mode_mirror();
    }

    /// A copy of this engine that continues as it does.
    #[must_use]
    pub fn replay_fork(&self) -> Option<Terminal> {
        if !self.parser_is_ground() || self.carried_parser().is_some() {
            return None;
        }
        let active = self.grid.replay_fork()?;
        let inactive = match &self.alt_grid {
            Some(grid) => Some(grid.replay_fork()?),
            None => None,
        };
        let scalars = self.project(usize::MAX, usize::MAX, true, false, MarksFrom::Carried);
        let mut fork = Self::restored(&scalars, active, inactive);
        fork.holder_fence.clone_from(&self.holder_fence);
        fork.apply_captured_scalars(&self.captured_carry());
        Some(fork)
    }

    /// Distinct image and in-flight chunk payload bytes held in the Kitty store and visible cell layers that [`Grid::memory_used`] does not book (history [`aterm_scrollback::ImageSpan`]s already book their own row share inside [`Grid::memory_used`]).
    #[must_use]
    pub fn unbooked_image_bytes(&self) -> usize {
        use aterm_types::ImageData;
        use std::sync::Arc;
        let t = &self.transient;
        let active_has = self.grid.extras().any_image();
        let inactive_has = self
            .alt_grid
            .as_ref()
            .is_some_and(|g| g.extras().any_image());
        if t.kitty_images.is_empty()
            && t.kitty_frames.is_empty()
            && t.kitty_variants.is_empty()
            && t.kitty_floating.is_empty()
            && t.kitty_pending.is_none()
            && !active_has
            && !inactive_has
        {
            return 0;
        }
        let mut seen: Vec<usize> = Vec::new();
        let mut total = t
            .kitty_pending
            .as_ref()
            .map_or(0, |cmd| cmd.payload.capacity());
        let mut visit = |img: &Arc<ImageData>| {
            let ptr = Arc::as_ptr(img) as usize;
            if let Err(pos) = seen.binary_search(&ptr) {
                seen.insert(pos, ptr);
                total = total.saturating_add(img.payload_bytes());
            }
        };
        for img in t.kitty_images.values() {
            visit(img);
        }
        for anim in t.kitty_frames.values() {
            for frame in &anim.frames {
                visit(&frame.image);
            }
        }
        for variants in t.kitty_variants.values() {
            for variant in variants {
                visit(&variant.image);
                visit(&variant.source);
            }
        }
        for floating in &t.kitty_floating {
            visit(&floating.image);
        }
        if active_has {
            for (_, extra) in self.grid.extras().iter() {
                for layer in extra.images() {
                    visit(&layer.image);
                }
            }
        }
        if inactive_has && let Some(alt) = &self.alt_grid {
            for (_, extra) in alt.extras().iter() {
                for layer in extra.images() {
                    visit(&layer.image);
                }
            }
        }
        total
    }

    fn restored(c: &TerminalCheckpoint, active_grid: Grid, saved_grid: Option<Grid>) -> Terminal {
        let mut terminal = Terminal::with_grid(active_grid);
        terminal.alt_grid = saved_grid;

        terminal.modes = c.modes;
        terminal.cursor_save.main = c.saved_cursor_main.map(SavedCursorRepr::into_saved);
        terminal.cursor_save.alt = c.saved_cursor_alt.map(SavedCursorRepr::into_saved);
        terminal.charset = c.charset;
        terminal.kitty_keyboard.restore_snapshot(c.kitty_keyboard);
        terminal.xterm_keyboard = c.xterm_keyboard;
        terminal.taskbar_progress = c.taskbar_progress;
        terminal.secure_keyboard_entry = c.secure_keyboard_entry;
        terminal
            .current_working_directory
            .clone_from(&c.current_working_directory);

        terminal.style = c.style.into_style();
        {
            let (_parser, mut handler) = terminal.split_for_process();
            handler.sgr_style().apply_style_change();
        }
        terminal.apply_color_repr(&c.color);
        terminal.apply_shell_repr(&c.shell);
        terminal.install_holder_repr(&c.holder, main_counter(c));
        if let Some(rev) = c
            .shell_integration_rev
            .as_deref()
            .filter(|rev| crate::shell_integration::is_integration_rev(rev))
        {
            let mut bytes = [0u8; 16];
            bytes.copy_from_slice(rev.as_bytes());
            terminal.shell.integration_rev = Some(bytes);
        }
        if let Some(title) = &c.title {
            terminal.restore_title(title);
        }
        terminal.apply_output_state_repr(&c.output_state);
        terminal.restore_parser_carry(c.parser.as_ref());
        terminal.arm_carried_sync_window();
        terminal.refresh_mode_mirror();

        terminal
    }

    /// Restore a checkpoint's buffer state into an already-configured live terminal (the seamless-update adopt path).
    pub fn restore_checkpoint(&mut self, c: &TerminalCheckpoint) {
        let configured_scrollback_limit = self.scrollback_line_limit();
        let prior_pullback = self.deep_history_pullback_ordinal();
        self.release_search_index();
        let on_alt = c.modes.alternate_screen;
        self.grid = restore_grid(
            c.rows,
            c.cols,
            &c.grid,
            &c.visible_cells,
            &c.cursor,
            on_alt,
            None,
        );
        self.grid
            .continue_absolute_numbering(c.absolute_row_counter);
        self.alt_grid = match (&c.alt_grid, &c.alt_cursor) {
            (Some(alt_bytes), Some(alt_cursor)) => {
                let mut saved = restore_grid(
                    c.rows,
                    c.cols,
                    alt_bytes,
                    &c.alt_visible_cells,
                    alt_cursor,
                    !on_alt,
                    None,
                );
                saved.continue_absolute_numbering(c.alt_absolute_row_counter);
                Some(saved)
            }
            _ => None,
        };
        if on_alt {
            if let Some(main) = self.alt_grid.as_mut() {
                main.advance_deep_history_pullback_ordinal_by(prior_pullback);
            } else {
                self.grid
                    .advance_deep_history_pullback_ordinal_by(prior_pullback);
            }
        } else {
            self.grid
                .advance_deep_history_pullback_ordinal_by(prior_pullback);
        }
        self.modes = c.modes;
        self.set_scrollback_line_limit(configured_scrollback_limit);
        self.cursor_save.main = c.saved_cursor_main.map(SavedCursorRepr::into_saved);
        self.cursor_save.alt = c.saved_cursor_alt.map(SavedCursorRepr::into_saved);
        self.charset = c.charset;
        self.kitty_keyboard.restore_snapshot(c.kitty_keyboard);
        self.xterm_keyboard = c.xterm_keyboard;
        self.taskbar_progress = c.taskbar_progress;
        self.secure_keyboard_entry = c.secure_keyboard_entry;
        self.current_working_directory
            .clone_from(&c.current_working_directory);
        if let Some(rev) = c
            .shell_integration_rev
            .as_deref()
            .filter(|rev| crate::shell_integration::is_integration_rev(rev))
        {
            let mut bytes = [0u8; 16];
            bytes.copy_from_slice(rev.as_bytes());
            self.shell.integration_rev = Some(bytes);
        }
        self.style = c.style.into_style();
        {
            let (_parser, mut handler) = self.split_for_process();
            handler.sgr_style().apply_style_change();
        }
        self.apply_color_repr(&c.color);
        let _ = self.reconcile_color_scheme();
        self.apply_shell_repr(&c.shell);
        self.install_holder_repr(&c.holder, main_counter(c));
        if let Some(title) = &c.title {
            self.restore_title(title);
        }
        self.apply_output_state_repr(&c.output_state);
        self.restore_parser_carry(c.parser.as_ref());
        self.arm_carried_sync_window();
        self.content_scroll_state.invalidate();
        self.transient.sync_end_caret = None;
        self.parked_text_selection.clear();
        self.refresh_mode_mirror();
        self.alt_archive_after_restore();
    }
}

/// The [`CheckpointMeta`] keys a reader requires before it deserializes a meta it did not write in this process.
#[cfg(feature = "serde")]
pub const CHECKPOINT_META_REQUIRED_KEYS: &[&str] = &[
    "rows",
    "cols",
    "cursor",
    "alt_cursor",
    "saved_cursor_main",
    "saved_cursor_alt",
    "modes",
    "style_fg_bits",
    "style_bg_bits",
    "style_flag_bits",
    "style_protected",
    "charset",
    "kitty_keyboard",
    "xterm_keyboard",
    "taskbar_progress",
    "secure_keyboard_entry",
    "current_working_directory",
];

/// The serde-carryable half of a [`TerminalCheckpoint`].
#[cfg(feature = "serde")]
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct CheckpointMeta {
    /// Grid rows (visible).
    pub rows: u16,
    /// Grid cols.
    pub cols: u16,
    /// How many leading records of the main grid blob are scrollback rather than visible rows (see `TerminalCheckpoint::history_lines`).
    #[serde(default)]
    pub history_lines: u32,
    /// Main-grid cursor/region/wrap/tab projection.
    pub cursor: GridCursorRepr,
    /// Alt-grid cursor projection, if an alt grid exists.
    #[serde(default)]
    pub alt_cursor: Option<GridCursorRepr>,
    /// Main-screen DECSC/DECRC save slot.
    #[serde(default)]
    pub saved_cursor_main: Option<SavedCursorRepr>,
    /// Alternate-screen DECSC/DECRC save slot.
    #[serde(default)]
    pub saved_cursor_alt: Option<SavedCursorRepr>,
    /// Terminal modes.
    #[serde(default)]
    pub modes: TerminalModes,
    /// Current SGR foreground as raw `PackedColor` bits (see the struct doc).
    #[serde(default)]
    pub style_fg_bits: u32,
    /// Current SGR background as raw `PackedColor` bits.
    #[serde(default)]
    pub style_bg_bits: u32,
    /// Current SGR `CellFlags` bits (bold, italic, underline, …).
    #[serde(default)]
    pub style_flag_bits: u16,
    /// DECSCA selective-erase protection.
    #[serde(default)]
    pub style_protected: bool,
    /// Character set state (G0-G3, GL, GR, single-shift).
    #[serde(default)]
    pub charset: CharacterSetState,
    /// Kitty keyboard protocol state.
    #[serde(default)]
    pub kitty_keyboard: KittyKeyboardStateSnapshot,
    /// xterm keyboard modifier/format options.
    #[serde(default)]
    pub xterm_keyboard: XtermKeyboardState,
    /// Taskbar progress (ConEmu OSC 9;4).
    #[serde(default)]
    pub taskbar_progress: Option<TaskbarProgress>,
    /// Secure keyboard entry mode.
    #[serde(default)]
    pub secure_keyboard_entry: bool,
    /// Current working directory (OSC 7).
    #[serde(default)]
    pub current_working_directory: Option<String>,
    /// The authorized shell-integration nonce as 64 hex digits ([`TerminalCheckpoint::shell_integration_nonce`]).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shell_integration_nonce: Option<String>,
    /// [`TerminalCheckpoint::absolute_row_counter`].
    #[serde(default, skip_serializing_if = "is_zero")]
    pub absolute_row_counter: u64,
    /// [`TerminalCheckpoint::alt_absolute_row_counter`], as above.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub alt_absolute_row_counter: u64,
    /// [`TerminalCheckpoint::color`].
    #[serde(default, skip_serializing_if = "ColorRepr::is_default")]
    pub color: ColorRepr,
    /// [`TerminalCheckpoint::shell`].
    #[serde(default, skip_serializing_if = "ShellRepr::is_default")]
    pub shell: ShellRepr,
    /// The integration body the shell last signed that it runs, 16 hex digits ([`TerminalCheckpoint::shell_integration_rev`]).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shell_integration_rev: Option<String>,
    /// [`TerminalCheckpoint::title`].
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "lenient_title"
    )]
    pub title: Option<TitleRepr>,
    /// [`TerminalCheckpoint::parser`].
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "super::checkpoint_parser::lenient_parser"
    )]
    pub parser: Option<super::checkpoint_parser::ParserRepr>,
    /// [`TerminalCheckpoint::output_state`].
    #[serde(
        default,
        skip_serializing_if = "OutputStateRepr::is_default",
        deserialize_with = "super::checkpoint_output::lenient_output_state"
    )]
    pub output_state: OutputStateRepr,
    /// [`TerminalCheckpoint::holder`].
    #[serde(
        default,
        skip_serializing_if = "HolderRepr::is_empty",
        deserialize_with = "lenient_holder"
    )]
    pub holder: HolderRepr,
    /// [`TerminalCheckpoint::visible_cells`]: additive, absent when empty, and a value that does not read costs only itself.
    #[serde(
        default,
        skip_serializing_if = "super::checkpoint_cells::VisibleCellsRepr::is_empty",
        deserialize_with = "super::checkpoint_cells::lenient_visible_cells"
    )]
    pub visible_cells: super::checkpoint_cells::VisibleCellsRepr,
    /// [`TerminalCheckpoint::alt_visible_cells`], dropped with the inactive grid whenever [`Self::into_checkpoint`] drops the grid.
    #[serde(
        default,
        skip_serializing_if = "super::checkpoint_cells::VisibleCellsRepr::is_empty",
        deserialize_with = "super::checkpoint_cells::lenient_visible_cells"
    )]
    pub alt_visible_cells: super::checkpoint_cells::VisibleCellsRepr,
}

#[cfg(feature = "serde")]
fn lenient_holder<'de, D>(deserializer: D) -> Result<HolderRepr, D::Error>
where
    D: serde::Deserializer<'de>,
{
    #[derive(serde::Deserialize)]
    #[serde(untagged)]
    enum Wire {
        Holder(HolderRepr),
        Unreadable(serde::de::IgnoredAny),
    }
    Ok(
        match <Wire as serde::Deserialize>::deserialize(deserializer)? {
            Wire::Holder(holder) => holder,
            Wire::Unreadable(_) => HolderRepr {
                seed_untrusted: true,
                ..HolderRepr::default()
            },
        },
    )
}

#[cfg(feature = "serde")]
fn lenient_title<'de, D>(deserializer: D) -> Result<Option<TitleRepr>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    #[derive(serde::Deserialize)]
    #[serde(untagged)]
    enum Wire {
        Title(TitleRepr),
        Unreadable(serde::de::IgnoredAny),
    }
    Ok(
        match <Wire as serde::Deserialize>::deserialize(deserializer)? {
            Wire::Title(title) => Some(title),
            Wire::Unreadable(_) => None,
        },
    )
}

#[cfg(feature = "serde")]
#[allow(
    clippy::trivially_copy_pass_by_ref,
    reason = "serde's skip_serializing_if hands the predicate a reference"
)]
fn is_zero(n: &u64) -> bool {
    *n == 0
}

#[cfg(feature = "serde")]
#[allow(
    clippy::trivially_copy_pass_by_ref,
    reason = "serde's skip_serializing_if hands the predicate a reference"
)]
fn is_zero_rows(n: &i32) -> bool {
    *n == 0
}

#[cfg(feature = "serde")]
#[allow(
    clippy::trivially_copy_pass_by_ref,
    reason = "serde's skip_serializing_if hands the predicate a reference"
)]
fn is_zero_above(n: &u16) -> bool {
    *n == 0
}

#[cfg(feature = "serde")]
#[allow(
    clippy::trivially_copy_pass_by_ref,
    reason = "serde's skip_serializing_if hands the predicate a reference"
)]
fn is_unresized(resized: &SavedCursorResized) -> bool {
    !resized.get()
}

#[cfg(feature = "serde")]
impl CheckpointMeta {
    /// Project a full checkpoint into its carryable meta half.
    #[must_use]
    pub fn from_checkpoint(c: &TerminalCheckpoint) -> Self {
        let TerminalCheckpoint {
            rows,
            cols,
            grid: _, // sidecar blob, not meta
            history_lines,
            alt_grid: _, // sidecar blob, not meta
            cursor,
            alt_cursor,
            visible_cells,
            alt_visible_cells,
            saved_cursor_main,
            saved_cursor_alt,
            modes,
            style,
            charset,
            kitty_keyboard,
            xterm_keyboard,
            taskbar_progress,
            secure_keyboard_entry,
            current_working_directory,
            parser_ground: _,
            parser,
            shell_integration_nonce,
            absolute_row_counter,
            alt_absolute_row_counter,
            color,
            shell,
            shell_integration_rev,
            title,
            output_state,
            holder,
        } = c;
        Self {
            rows: *rows,
            cols: *cols,
            cursor: cursor.clone(),
            alt_cursor: alt_cursor.clone(),
            saved_cursor_main: *saved_cursor_main,
            history_lines: *history_lines,
            saved_cursor_alt: *saved_cursor_alt,
            modes: *modes,
            style_fg_bits: style.fg.0,
            style_bg_bits: style.bg.0,
            style_flag_bits: style.flags.0,
            style_protected: style.protected,
            charset: *charset,
            kitty_keyboard: *kitty_keyboard,
            xterm_keyboard: *xterm_keyboard,
            taskbar_progress: *taskbar_progress,
            secure_keyboard_entry: *secure_keyboard_entry,
            current_working_directory: current_working_directory.clone(),
            shell_integration_nonce: shell_integration_nonce.map(|n| n.to_hex()),
            absolute_row_counter: *absolute_row_counter,
            alt_absolute_row_counter: *alt_absolute_row_counter,
            color: color.clone(),
            shell: shell.clone(),
            shell_integration_rev: shell_integration_rev.clone(),
            title: title.clone(),
            parser: parser
                .as_ref()
                .map(super::checkpoint_parser::ParserRepr::from_carry),
            output_state: output_state.clone(),
            holder: holder.clone(),
            visible_cells: super::checkpoint_cells::VisibleCellsRepr::from_cells(visible_cells),
            alt_visible_cells: super::checkpoint_cells::VisibleCellsRepr::from_cells(
                alt_visible_cells,
            ),
        }
    }

    /// Reassemble a full [`TerminalCheckpoint`] from the meta + grid blobs.
    #[must_use]
    pub fn into_checkpoint(self, grid: Vec<u8>, alt_grid: Option<Vec<u8>>) -> TerminalCheckpoint {
        let parser = self
            .parser
            .as_ref()
            .and_then(super::checkpoint_parser::ParserRepr::to_carry)
            .filter(|carry| !carry.is_ground());
        let (alt_grid, alt_cursor, alt_visible_cells) = match (alt_grid, self.alt_cursor) {
            (Some(g), Some(c)) => (Some(g), Some(c), self.alt_visible_cells.into_cells()),
            _ => (None, None, VisibleCells::default()),
        };
        TerminalCheckpoint {
            rows: self.rows,
            cols: self.cols,
            grid,
            history_lines: self.history_lines,
            cursor: self.cursor,
            alt_grid,
            alt_cursor,
            visible_cells: self.visible_cells.into_cells(),
            alt_visible_cells,
            saved_cursor_main: self.saved_cursor_main,
            saved_cursor_alt: self.saved_cursor_alt,
            modes: self.modes,
            style: StyleRepr {
                fg: PackedColor(self.style_fg_bits),
                bg: PackedColor(self.style_bg_bits),
                flags: CellFlags(self.style_flag_bits),
                protected: self.style_protected,
            },
            charset: self.charset,
            kitty_keyboard: self.kitty_keyboard,
            xterm_keyboard: self.xterm_keyboard,
            taskbar_progress: self.taskbar_progress,
            secure_keyboard_entry: self.secure_keyboard_entry,
            current_working_directory: self.current_working_directory,
            parser_ground: parser.is_none(),
            parser,
            shell_integration_nonce: self
                .shell_integration_nonce
                .as_deref()
                .and_then(ShellIntegrationNonce::from_hex),
            absolute_row_counter: self.absolute_row_counter,
            alt_absolute_row_counter: self.alt_absolute_row_counter,
            color: self.color,
            shell: self.shell,
            shell_integration_rev: self
                .shell_integration_rev
                .filter(|rev| crate::shell_integration::is_integration_rev(rev)),
            title: self.title,
            output_state: self.output_state,
            holder: self.holder,
        }
    }
}

/// The absolute row counter of the checkpoint's main grid: the inactive one
/// while the alternate screen is up.
const fn main_counter(c: &TerminalCheckpoint) -> u64 {
    if c.modes.alternate_screen {
        c.alt_absolute_row_counter
    } else {
        c.absolute_row_counter
    }
}

fn restore_grid(
    rows: u16,
    cols: u16,
    bytes: &[u8],
    cells: &VisibleCells,
    cursor: &GridCursorRepr,
    is_alt: bool,
    store_lines: Option<usize>,
) -> Grid {
    let mut lines = deserialize_lines(bytes);
    let mut visible_start = lines.len().saturating_sub(rows as usize);

    let mut grid = if is_alt {
        // The alternate screen keeps no scrollback (as `enter_alternate_screen`): a
        // ring would accrue history the source never had and diverge on re-checkpoint.
        Grid::with_scrollback(rows, cols, 0)
    } else {
        const RING_CAP: usize = 1000;
        let in_ring = visible_start.min(RING_CAP);
        let to_store = visible_start - in_ring;
        let mut scrollback = Scrollback::with_defaults();
        scrollback.set_line_limit(store_lines);
        // These decoded lines already belong to the restored store; moving them
        // avoids cloning their text and attributes before dropping the originals.
        for line in lines.drain(..to_store) {
            // push_line is infallible for the in-memory tier.
            scrollback.push_line(line);
        }
        visible_start = in_ring;
        let mut grid = Grid::with_tiered_scrollback(rows, cols, RING_CAP, scrollback);
        grid.scroll_in_history(&lines[..visible_start]);
        grid
    };
    grid.restore_visible_from_lines(&lines[visible_start..]);
    // Before the cursor, which a row's line size bounds.
    grid.restore_visible_cells(cells);
    grid.damage_mut().mark_full();
    cursor.apply(&mut grid);
    grid
}

#[cfg(test)]
#[path = "checkpoint_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "replay_fork_tests.rs"]
mod replay_fork_tests;
