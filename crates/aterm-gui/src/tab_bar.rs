// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A clickable tab strip composed at the GUI level (no renderer pass).
//!
//! Tabs are state ([`crate::TabIndex`] + one [`crate::pane::PaneTree`] per tab).
//! This module reserves `tab_strip_rows` rows at the top of the window (default 1;
//! 0 on macOS, where the native toolbar carries tabs) and draws one segment per
//! top-level tab (title, active highlight, close `✕`) plus a trailing `+`.
//!
//! Every chrome band uses chip cards ([`STRIP_CHIP_CARDS`]), with truncation that
//! keeps what distinguishes a title from its neighbours' ([`distinct_chip_labels`]);
//! [`pixel_band`] paints them in pixels in the UI font. Below a legible chip width
//! ([`STRIP_SEATS_FLOOR`]) the strip seats a window of tabs containing the
//! selection, marking continued edges with `‹`/`›` ([`strip_overflow`]). The window
//! is a function of `(cols, tab_count, active)` with no stored scroll offset, so
//! the hit map and the painter cannot disagree.
//!
//! Layout is pure, mirroring [`crate::pane`]: [`layout_segments`] produces the
//! segments and [`paint_strip`] writes them into the composed frame's top rows. A
//! click in `row < tab_strip_rows` maps through [`hit_test`] before the focused
//! pane's cell mapping. The strip is spliced above the terminal content in the
//! composed `RenderInput` only; session grids are never shifted.

use std::cell::RefCell;
use std::sync::Arc;

use aterm_core::grid::extra::{ImageData, ImageFormat, ImageRef, ImageScaling};
use aterm_core::terminal::{RenderCell, UnderlineStyle};
#[cfg(test)]
use aterm_render::Theme;

use crate::chrome_band::ChromePalette;
/// The window's own buttons, re-exported beside the strip that lays them out.
pub(crate) use crate::client_frame::{CaptionButton, CaptionSlot};

pub(crate) use crate::presence::ChipLevel;
/// The connection-mark role, re-exported beside the icon kind so both renderers
/// consume one vocabulary defined on the tab model.
pub(crate) use crate::tab_model::TabConnRole;
/// The four first-party non-terminal tab application identities. A terminal is
/// `None`. Every renderer draws them from the code-native primitives below, so no
/// installed font or asset can change what an aterm app tab means.
pub(crate) use crate::tab_model::TabIconKind;

/// Non-title presentation metadata consumed by both tab-strip renderers. Titles
/// stay separate because they are live OSC state.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub(crate) struct TabStripMetadata {
    pub(crate) icon: Option<TabIconKind>,
    pub(crate) dirty: bool,
    pub(crate) busy: bool,
    /// The need mark: `Wait` the hollow diamond (a decide need only), `Stop` a
    /// filled one (a stuck need), `Yours` the person's hold, `Done` a dot (a
    /// finished task not yet looked at); the tab's own indicator bit (a
    /// failed command, a notice) is `Note`, a short dash in the label ink,
    /// never a need mark. `App::stamp_presence_chips` folds the published
    /// verdict's mark over the bit.
    pub(crate) attention: ChipLevel,
    /// The connection-mark role; one shape per role. The `Hash` derive folds it
    /// into the in-grid strip fingerprint.
    pub(crate) conn: Option<TabConnRole>,
    pub(crate) closable: bool,
    /// WHO DRIVES (owner surfaces §5, S9): the tab's team marks, `↳` before a
    /// worker's title and `⇉N` after a manager's ([`team_title`]). Stamped by
    /// `App::stamp_presence_chips` from the team table (`crate::team`); always
    /// [`TabTeam::NONE`] from [`Self::from_presentation`]. Not a status mark.
    pub(crate) team: TabTeam,
    /// The tab's LOAD METER (design §12.4): its focused session's cores-worth
    /// on the machine's capacity in 255ths ([`chip_load_step`]), 0 for no
    /// meter (under a tenth of a core, the stats bars off, a one-tab window,
    /// no reading yet). Stamped by `App::stamp_presence_chips` from the vitals
    /// snapshot; always 0 from [`Self::from_presentation`]. Not a status mark:
    /// it takes no slot, so the packing is unchanged.
    pub(crate) load: u8,
}

/// A tab's team marks (owner surfaces §5, S9): whether its session is a
/// worker (its task's `from=` names another session) and how many workers
/// name it. A record: no colour, no wake.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub(crate) struct TabTeam {
    /// The session is a worker.
    pub(crate) worker: bool,
    /// How many workers name it (0: no manager).
    pub(crate) workers: u16,
}

impl TabTeam {
    /// No team: a session that is neither a worker nor a manager.
    pub(crate) const NONE: Self = Self {
        worker: false,
        workers: 0,
    };
}

/// `title` as a strip shows it for a tab wearing `team`: `↳ ` before a
/// worker's, ` ⇉N` after a manager's, the title itself (borrowed) wearing
/// neither. Applied where a strip lays its titles out — the in-grid splice
/// and the native push — never to the tab's own name, which renames,
/// tooltips and identity read.
#[must_use]
pub(crate) fn team_title(title: &str, team: TabTeam) -> std::borrow::Cow<'_, str> {
    if team == TabTeam::NONE {
        return std::borrow::Cow::Borrowed(title);
    }
    let mut out = String::with_capacity(title.len() + 12);
    if team.worker {
        out.push_str("\u{21b3} ");
    }
    out.push_str(title);
    if team.workers > 0 {
        out.push_str(" \u{21c9}");
        out.push_str(&team.workers.to_string());
    }
    std::borrow::Cow::Owned(out)
}

/// Every title of a strip as it shows them ([`team_title`] by index over
/// `metadata`), borrowed whole where no tab wears a team mark, so a strip
/// with no team allocates nothing for it.
#[must_use]
pub(crate) fn team_titles<'a>(
    titles: &'a [String],
    metadata: &[TabStripMetadata],
) -> std::borrow::Cow<'a, [String]> {
    if metadata.iter().all(|item| item.team == TabTeam::NONE) {
        return std::borrow::Cow::Borrowed(titles);
    }
    std::borrow::Cow::Owned(
        titles
            .iter()
            .enumerate()
            .map(|(i, title)| {
                let team = metadata.get(i).map_or(TabTeam::NONE, |item| item.team);
                team_title(title, team).into_owned()
            })
            .collect(),
    )
}

impl TabStripMetadata {
    #[must_use]
    pub(crate) fn from_presentation(presentation: &crate::tab_model::TabPresentation) -> Self {
        Self {
            icon: presentation.icon,
            dirty: presentation.indicators.dirty,
            busy: presentation.indicators.busy,
            // The strip draws one attention mark, so the two owners fold here.
            attention: crate::app_presence::chip_of_attention(
                presentation.indicators.wants_attention(),
            ),
            conn: presentation.conn,
            closable: presentation.closable,
            team: TabTeam::NONE,
            load: 0,
        }
    }

    #[must_use]
    pub(crate) const fn has_status(self) -> bool {
        self.dirty || self.busy || !matches!(self.attention, ChipLevel::Off) || self.conn.is_some()
    }

    #[must_use]
    pub(crate) const fn status_count(self) -> usize {
        self.dirty as usize
            + self.busy as usize
            + !matches!(self.attention, ChipLevel::Off) as usize
            + self.conn.is_some() as usize
    }
}

/// Independently visible status marks carried by a tab. Order and shapes are
/// shared by the in-grid renderer and the native macOS host.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub(crate) enum TabStatusKind {
    Dirty,
    Busy,
    Attention,
    /// A live session connection touches this tab. The drawn shape comes from
    /// [`TabStripMetadata::conn`] at paint time.
    Connection,
}

pub(crate) const TAB_STATUS_KINDS: [TabStatusKind; 4] = [
    TabStatusKind::Dirty,
    TabStatusKind::Busy,
    TabStatusKind::Attention,
    TabStatusKind::Connection,
];

impl TabStripMetadata {
    #[must_use]
    pub(crate) const fn has_status_kind(self, kind: TabStatusKind) -> bool {
        match kind {
            TabStatusKind::Dirty => self.dirty,
            TabStatusKind::Busy => self.busy,
            TabStatusKind::Attention => !matches!(self.attention, ChipLevel::Off),
            TabStatusKind::Connection => self.conn.is_some(),
        }
    }
}

/// How far a chip's load meter sits in from each end of the pill, in points.
pub(crate) const CHIP_LOAD_INSET_PT: f32 = 10.0;
/// How far above the pill's bottom the meter sits, in points: clear of the
/// selected chip's 2 pt accent keyline.
pub(crate) const CHIP_LOAD_LIFT_PT: f32 = 2.5;
/// The meter's thickness, in points.
pub(crate) const CHIP_LOAD_H_PT: f32 = 1.5;

/// A tab's load step ([`TabStripMetadata::load`]) from its cores-worth and
/// the machine's capacity in cores: 0 under a tenth of a core (quiet earns no
/// meter) or with no capacity known, else its share in 255ths, at least 1 so
/// a tenth of a core shows even on a machine of hundreds.
#[must_use]
pub(crate) fn chip_load_step(cores: f32, capacity: f32) -> u8 {
    if !(cores >= 0.1 && capacity > 0.0) {
        return 0;
    }
    let share = (cores / capacity).clamp(0.0, 1.0);
    (share * 255.0).round().clamp(1.0, 255.0) as u8
}

/// The load meter's length in points on a pill `pill_w_pt` wide: the step's
/// share of the track between the insets, in whole points, at least one.
/// `None` for no meter or a pill too narrow to hold one. A native chip
/// repaints only when this length moves (`toolbar.rs`'s `set_load`); the
/// in-grid strip folds the step into its fingerprint, so it re-rasters with
/// the frame the reading presents for the bars anyway.
#[must_use]
pub(crate) fn chip_load_len_pt(pill_w_pt: f32, load: u8) -> Option<f32> {
    let track = pill_w_pt - 2.0 * CHIP_LOAD_INSET_PT;
    if load == 0 || track.is_nan() || track < 4.0 {
        return None;
    }
    Some(
        (track * f32::from(load) / 255.0)
            .round()
            .clamp(1.0, track.floor()),
    )
}

/// Horizontal centre for one status mark in a 16-unit design box. Active marks pack
/// toward the middle without leaving mystery gaps when only one or two states exist.
#[must_use]
pub(crate) fn tab_status_center(ordinal: usize, count: usize) -> f32 {
    match count {
        0 => 8.0,
        1 => 8.0,
        // The busy ring (half-extent 2.625) and the attention diamond (3.075, 3.31
        // mitred by AppKit) need 5.94 between centres; 7.0 leaves a 1.06-unit gap.
        2 => 4.5 + ordinal.min(1) as f32 * 7.0,
        3 => 3.5 + ordinal.min(2) as f32 * 4.5,
        _ => 2.0 + ordinal.min(3) as f32 * 4.0,
    }
}

/// Per-shape scale for `count` packed status marks. Four marks on a 4-unit pitch
/// cannot hold the full-size shapes (the attention diamond spans 5 units), so
/// every mark shrinks by one shared factor; three or fewer stay full size.
/// Consumed by both renderers, like [`tab_status_center`].
#[must_use]
pub(crate) fn tab_status_mark_scale(count: usize) -> f32 {
    // At 0.75 the widest shapes (the diamond and the outbound triangle, 2.5 from
    // centre at full size) fit the 4-unit pitch, only the diamond's stroke reaching
    // past it, and the edge marks (centres 2.0/14.0) stay inside the box.
    if count >= 4 { 0.75 } else { 1.0 }
}

/// One normalized code-native icon primitive in a 16×16 design box. Lines use
/// round caps; outlined rounded rectangles use the given corner radius. The in-grid
/// path rasterizes these into `RawRgba8`; the macOS titlebar maps them to
/// `NSBezierPath`, so both strips draw identical geometry.
#[derive(Clone, Copy, PartialEq, Debug)]
pub(crate) enum TabIconPrimitive {
    Line {
        from: [f32; 2],
        to: [f32; 2],
        width: f32,
    },
    RoundedRect {
        rect: [f32; 4],
        radius: f32,
        width: f32,
    },
    Dot {
        center: [f32; 2],
        radius: f32,
    },
    /// A filled triangle (the connection mark's arrows); the IR's only filled
    /// polygon. Hollow triangles are compositions of `Line`.
    Triangle {
        points: [[f32; 2]; 3],
    },
}

pub(crate) const TAB_ICON_DESIGN_SIZE: f32 = 16.0;
/// Native toolbar icon size in logical points. The primitive design box is scaled
/// uniformly into this square, keeping a crisp, restrained ~16 px optical mark.
#[cfg(any(target_os = "macos", test))]
pub(crate) const TAB_ICON_NATIVE_SIZE: f64 = 14.0;

const SETTINGS_ICON: &[TabIconPrimitive] = &[
    TabIconPrimitive::Line {
        from: [2.0, 4.0],
        to: [14.0, 4.0],
        width: 1.25,
    },
    TabIconPrimitive::Line {
        from: [2.0, 8.0],
        to: [14.0, 8.0],
        width: 1.25,
    },
    TabIconPrimitive::Line {
        from: [2.0, 12.0],
        to: [14.0, 12.0],
        width: 1.25,
    },
    TabIconPrimitive::Dot {
        center: [5.0, 4.0],
        radius: 1.65,
    },
    TabIconPrimitive::Dot {
        center: [10.5, 8.0],
        radius: 1.65,
    },
    TabIconPrimitive::Dot {
        center: [7.25, 12.0],
        radius: 1.65,
    },
];

// A constructed `M` plus two compact reading lines: unmistakably Markdown without
// asking a font for the letter or depending on a private-use icon.
const MARKDOWN_ICON: &[TabIconPrimitive] = &[
    TabIconPrimitive::RoundedRect {
        rect: [1.5, 2.25, 13.0, 11.5],
        radius: 1.75,
        width: 1.2,
    },
    TabIconPrimitive::Line {
        from: [4.0, 9.8],
        to: [4.0, 5.7],
        width: 1.25,
    },
    TabIconPrimitive::Line {
        from: [4.0, 5.7],
        to: [6.15, 8.0],
        width: 1.25,
    },
    TabIconPrimitive::Line {
        from: [6.15, 8.0],
        to: [8.3, 5.7],
        width: 1.25,
    },
    TabIconPrimitive::Line {
        from: [8.3, 5.7],
        to: [8.3, 9.8],
        width: 1.25,
    },
    TabIconPrimitive::Line {
        from: [10.2, 6.5],
        to: [12.2, 6.5],
        width: 1.1,
    },
    TabIconPrimitive::Line {
        from: [10.2, 9.0],
        to: [12.2, 9.0],
        width: 1.1,
    },
];

// A document/editor surface with text rules and a strong insertion caret.  The caret
// is geometry, not a text glyph, and remains legible at the compact 14 pt toolbar size.
const EDITOR_ICON: &[TabIconPrimitive] = &[
    TabIconPrimitive::RoundedRect {
        rect: [2.0, 1.5, 12.0, 13.0],
        radius: 1.75,
        width: 1.2,
    },
    TabIconPrimitive::Line {
        from: [4.25, 5.0],
        to: [11.75, 5.0],
        width: 1.05,
    },
    TabIconPrimitive::Line {
        from: [4.25, 8.0],
        to: [11.75, 8.0],
        width: 1.05,
    },
    TabIconPrimitive::Line {
        from: [4.25, 11.0],
        to: [10.0, 11.0],
        width: 1.05,
    },
    TabIconPrimitive::Line {
        from: [7.35, 3.6],
        to: [7.35, 12.35],
        width: 1.45,
    },
];

const RECOVERY_ICON: &[TabIconPrimitive] = &[
    TabIconPrimitive::RoundedRect {
        rect: [2.0, 1.5, 12.0, 13.0],
        radius: 2.5,
        width: 1.25,
    },
    TabIconPrimitive::Line {
        from: [8.0, 4.25],
        to: [8.0, 9.5],
        width: 1.5,
    },
    TabIconPrimitive::Dot {
        center: [8.0, 12.0],
        radius: 1.0,
    },
];

#[must_use]
pub(crate) const fn tab_icon_primitives(kind: TabIconKind) -> &'static [TabIconPrimitive] {
    match kind {
        TabIconKind::Settings => SETTINGS_ICON,
        TabIconKind::Markdown => MARKDOWN_ICON,
        TabIconKind::Editor => EDITOR_ICON,
        TabIconKind::Recovery => RECOVERY_ICON,
    }
}

/// What clicking a strip column does.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum TabHit {
    /// Switch the window to this tab index (a click anywhere on the segment but the
    /// close `x`).
    Select(usize),
    /// Close this tab index (a click on the segment's close `x`).
    Close(usize),
    /// Open a new tab (a click on the trailing `+`).
    NewTab,
    /// Apply the staged update (a click on the leading `↻`, shown only while a build
    /// is staged).
    Update,
    /// The build's version label at the strip's trailing end (Windows only,
    /// [`STRIP_VERSION_SLOT`]): the macOS menu bar's Version title without its colour
    /// emoji ([`crate::menu::strip_version_label`] at `rung`, ` ↑` after it while
    /// `attention` holds). A button committed on press like `+` and `↻`: it fires
    /// [`crate::menu::version_slot_click`] and is never caption, so it neither drags
    /// nor maximizes the window. Laid out only where it costs no tab its seat
    /// ([`layout_segments_with_version`]).
    Version {
        /// Which length of the label this layout had room for.
        rung: crate::menu::VersionRung,
        /// An update is staged ([`crate::menu::staged_for_version_menu`]).
        attention: bool,
    },
    /// The window's NEEDS PILL (owner surfaces §5), where this strip carries
    /// it ([`layout_segments_with_pill`]): one segment per region the
    /// engine's layout resolves a click to ([`strip_pill_layout`],
    /// `PillLayout::hit`) — the body goes to the tab, the act runs, the `+N`
    /// goes to the window's next need (`App::on_pill_act`). Committed on the
    /// press like the `+`, never caption.
    Pill(crate::presence::PillHit),
    /// One of the window's own buttons at the strip's far right, where the band is
    /// the window's header (a Wayland window the compositor leaves undecorated,
    /// [`crate::client_frame`]). A BUTTON, committed on the press like the `+`:
    /// minimize, maximize or restore, close. Laid out by
    /// [`layout_segments_with_caption`], and only there.
    Caption(CaptionButton),
}

/// One laid-out tab strip segment: a half-open column range `[start_col, end_col)`,
/// an optional close-`x` column, and what a plain click does. Cached per frame so a
/// click maps back to a tab in O(segments).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) struct TabSegment {
    /// First column of this segment (inclusive).
    pub start_col: u16,
    /// One past the last column of this segment (exclusive).
    pub end_col: u16,
    /// The column of the close `x`, if this segment drew one. A click here is a
    /// [`TabHit::Close`]; every other column of the segment is the `kind` action.
    pub close_col: Option<u16>,
    /// The action a plain (non-close) click on this segment performs — on
    /// its status marks too, which are part of the chip: a press on a tab's
    /// `◆` selects it like any other press on it
    /// (`docs/DESIGN-owner-surfaces-2026-10-16.md` §5).
    pub kind: TabHit,
    /// This is the window's only tab, drawn as the window title (name and
    /// description centred on the body background) rather than a chip with a close
    /// affordance. Mirrors the native strip's solo mode (`toolbar::TabView`).
    pub solo: bool,
}

/// The minimum cells a tab segment needs to show ` x ` (pad, one title cell, pad,
/// close `x`, pad). Narrower tabs are drawn without a close `x`.
const MIN_SEG_WITH_CLOSE: u16 = 5;
/// Columns the trailing `+` (open-a-tab) affordance occupies: ` + `.
const NEW_TAB_W: u16 = 3;
/// Columns the leading update icon (`↻`) occupies: ` ↻ `, the same width as `+`.
const UPDATE_W: u16 = 3;

/// Whether the tab band wears the Windows paint of the 2026-10-17 audit: one
/// chip cap for every tab count ([`CHIP_MAX_COLS`]), the seam a rail on the
/// band's last pixel rows ([`strip_seam_rail`]) that the pixel band also draws
/// on its own canvas, status marks sized off the label, and titles cut in
/// UI-face cells ([`pixel_band::ui_cells_per_cell`]).
///
/// WINDOWS ONLY, as [`STRIP_VERSION_SLOT`] is: Linux keeps its strip as it was
/// until that lane is reviewed, a lone chip capped at [`LONE_CHIP_COLS`], two or
/// more sharing the whole band, and the seam a cell underline
/// ([`seal_strip_bottom`]). macOS has no band.
pub(crate) const STRIP_WINDOWS_PAINT: bool = cfg!(windows);

/// Whether the strip draws Windows 11 tabs rather than chips (Windows Terminal's,
/// Edge's and Explorer's tab rows): the selected tab is the terminal's own ground
/// ([`StripColors::ground`]) with its top corners rounded, and its foot opens
/// through the seam into the grid, so the page and its tab are one surface;
/// quiet tabs are bare titles on the band, with a hover wash under the pointer
/// and a hairline between two quiet neighbours. It replaced grey capsules
/// floating on the band, whose selected one was the DARKEST chip of a light
/// theme (the 2026-10-18 judges' `tab-chips-not-win11-tabs`).
///
/// WINDOWS ONLY, like [`STRIP_WINDOWS_PAINT`]: Linux keeps its chip cards
/// (libadwaita's tab bar is a row of cards) until that lane is reviewed.
pub(crate) const SELECTED_TAB_IS_GROUND: bool = STRIP_WINDOWS_PAINT;

/// How far a quiet tab's hover wash steps from a LIGHT Windows tab row's band
/// toward the theme's ink: about WinUI's light `TabViewItem` hover
/// (`SubtleFillColorSecondary`, 6% black), `#d3d4d7` on Default Light's
/// `#e2e3e5` band. The selected tab and the update CTA rise toward white, so the
/// wash is the opposite step and never reads as a second selected tab; it rose
/// toward white too (`#eeeeef`, 1.08:1 from the selected `#f6f7f9`), the
/// 2026-10-18 judges' `light-hover-wash-rises-toward-white` and
/// `light-hover-mimics-selected-tab`.
const LIGHT_TAB_HOVER_T: f32 = 0.08;

/// How wide any tab's chip may grow, in columns, whatever the tab count; `None`
/// where tabs share the band with no maximum (macOS, where a lone tab is also the
/// window title band and spends the whole strip, [`SOLO_TITLE_BAND`], and Linux,
/// which caps only a lone chip, [`SOLO_CHIP_MAX_COLS`]).
///
/// A full-width raised card would read as a heavy grey slab, so on Windows
/// chips are capped and stay leading, leaving band to their right for the `+`,
/// as browser and Windows Terminal tabs are. ONE cap for every count: the lone
/// chip used to be capped at 28 columns while each of two chips took half the
/// band, so a lone title was cut with half the strip empty and the chip jumped
/// wider when a second tab opened (the 2026-10-17 audit's
/// `lone-chip-narrower-than-pair`). The shipping strip takes the cap in DIPs
/// and lets a long title's tab grow past it while the strip has room
/// ([`TabSizing`]); its measure ignores a spinner's frames, so they never move
/// the `✕`. A narrow window still gets the smaller equal share: this is a
/// maximum.
const CHIP_MAX_COLS: Option<u16> = if STRIP_WINDOWS_PAINT {
    Some(MAX_CHIP_COLS)
} else {
    None
};

/// How wide a lone tab's chip may grow, in columns; `None` where a lone tab is the
/// window title band and spends the whole strip (macOS; see [`SOLO_TITLE_BAND`]).
/// Elsewhere the lone chip is capped and stays leading, leaving band to its right
/// for the `+`: at [`LONE_CHIP_COLS`].
const SOLO_CHIP_MAX_COLS: Option<u16> = if cfg!(target_os = "macos") {
    None
} else {
    Some(LONE_CHIP_COLS)
};

/// The lone chip's width off macOS ([`SOLO_CHIP_MAX_COLS`]): the shared
/// [`MAX_CHIP_COLS`] on Windows, 28 columns (~24 title cells) on Linux. Named
/// on its own because the trailing version label ([`TabHit::Version`]) is
/// admitted beside a lone tab only while the band left of it still holds a chip
/// this wide — so the label can never be the reason a one-tab window's chip
/// shrinks.
const LONE_CHIP_COLS: u16 = if STRIP_WINDOWS_PAINT {
    MAX_CHIP_COLS
} else {
    28
};

/// The Windows tab cap ([`CHIP_MAX_COLS`]) in 9 px cells, the 100% reading of
/// [`TAB_MAX_DIP`]: 27 columns, 243 px. The live strip takes the cap in DIPs at
/// its own cell width and scale ([`TabSizing::at`]); this is the fixed cap the
/// layout contract's tests and a window without its metrics use
/// ([`TabSizing::FIXED`]). It was 48 columns (432 px), which left a lone short
/// title at the far left of an empty 432 px pill with its `✕` 360 px away,
/// read as a text field rather than a tab (the 2026-10-18 judges'
/// `lone-chip-432px`).
const MAX_CHIP_COLS: u16 = 27;

/// A Windows tab's width while the strip has room, in DIPs: WinUI's
/// `TabViewItemMaxWidth` (240 epx), the equal width Windows Terminal's tabs
/// keep. Counted in DIPs, not cell columns: the cell width rounds down from the
/// font at 150% and 200% (13 and 17 px for 13.5 and 18) while the UI label
/// scales exactly, so 27 columns cut a title at 2x that fit at 1x (the
/// 2026-10-18 judges' `lone-tab-cap-hidpi`).
const TAB_MAX_DIP: f32 = 240.0;

/// The widest a Windows tab grows to seat its whole title, in DIPs: 432, the 48
/// columns of 9 px the cap was, about sixty characters of the UI face (a Claude
/// Code task title, a `less` or `vim` command line with its file). A tab whose
/// title is wider than [`TAB_MAX_DIP`] grows toward it while the strip has room
/// beside its neighbours, as WinUI's `TabView` sizes a tab to its content
/// (Windows Terminal's `titleLength` width); a crowded strip shares the band
/// equally as before. At a fixed 240 the lone tab cut `less -R +1200
/// CHANGELOG.md` and Claude Code's task titles with 700 px of empty strip
/// beside it (the judges' `tab-title-cap-27-cols-lone-tab`); a short title
/// keeps the 240 tab, never the empty 432 px pill.
const TAB_GROW_DIP: f32 = 432.0;

/// How wide the Windows tab row's tabs are ([`layout_segments_with_caption_sized`]):
/// equal at [`Self::cap`] while the strip has room, each growing toward the
/// width its title wants ([`Self::wants`]) up to [`Self::grow`] with the room
/// that is left. Under pressure, and off Windows, the layout is the equal-share
/// one ([`layout_segments_seated`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct TabSizing<'a> {
    /// The equal width in columns ([`TAB_MAX_DIP`] at the window's metrics).
    pub(crate) cap: u16,
    /// The most a tab grows to, in columns ([`TAB_GROW_DIP`]).
    pub(crate) grow: u16,
    /// Each tab's wanted width in columns, title and chrome
    /// ([`pixel_band::tab_wants`]); a tab past the end, or at 0, wants the cap.
    pub(crate) wants: &'a [u16],
}

impl<'a> TabSizing<'a> {
    /// Equal tabs at [`MAX_CHIP_COLS`] that never grow: the layout's contract
    /// where no metrics or titles are at hand.
    #[cfg(test)]
    pub(crate) const FIXED: TabSizing<'static> = TabSizing {
        cap: MAX_CHIP_COLS,
        grow: MAX_CHIP_COLS,
        wants: &[],
    };

    /// The sizing of a window whose cells are `cell_w` device px at `scale`:
    /// the caps in DIPs rounded to whole columns (27 and 48 at 100%, 28 and 50
    /// at 150%, 28 and 51 at 200%), and the tabs' `wants`.
    pub(crate) fn at(cell_w: usize, scale: f32, wants: &'a [u16]) -> Self {
        let scale = if scale.is_finite() && scale > 0.0 {
            scale
        } else {
            1.0
        };
        let cols = |dip: f32| {
            let cols = (dip * scale / cell_w.max(1) as f32).round();
            cols.clamp(1.0, f32::from(u16::MAX)) as u16
        };
        let cap = cols(TAB_MAX_DIP);
        Self {
            cap,
            grow: cols(TAB_GROW_DIP).max(cap),
            wants,
        }
    }

    /// Tab `i`'s width when the strip has room: its want, held between the
    /// equal width and the growth cap.
    fn wanted(&self, i: usize) -> u16 {
        self.wants
            .get(i)
            .copied()
            .unwrap_or(0)
            .clamp(self.cap, self.grow.max(self.cap))
    }
}

/// The widths of `count` tabs from `first` that each start at `base` and may
/// grow toward [`TabSizing::wanted`] within `band` columns: the room past
/// `count × base` handed out evenly to the tabs still short of their want, in a
/// few rounds, never more than a tab wants. Every tab keeps `base` when nothing
/// is left.
fn grown_widths(
    sizing: &TabSizing<'_>,
    first: usize,
    count: usize,
    base: u16,
    band: u16,
) -> Vec<u16> {
    let mut widths = vec![base; count];
    let wants: Vec<u16> = (first..first + count).map(|i| sizing.wanted(i)).collect();
    let used = u32::from(base) * u32::try_from(count).unwrap_or(u32::MAX);
    let mut room = u32::from(band).saturating_sub(used);
    while room > 0 {
        let short: Vec<usize> = (0..count).filter(|&i| widths[i] < wants[i]).collect();
        if short.is_empty() {
            break;
        }
        let each = (room / u32::try_from(short.len()).unwrap_or(u32::MAX)).max(1);
        let mut given = 0u32;
        for i in short {
            let add = u32::from(wants[i] - widths[i]).min(each).min(room - given);
            widths[i] += u16::try_from(add).unwrap_or(0);
            given += add;
            if given == room {
                break;
            }
        }
        if given == 0 {
            break;
        }
        room -= given;
    }
    widths
}

/// The equal-share legibility floor, in columns. While every tab's equal share stays
/// at or above this, tabs split the band evenly; below it [`layout_segments`] seats a
/// window of tabs ([`STRIP_SEATS_FLOOR`]), or, at the two-chip minimum, gives the
/// selected tab the active-priority share.
///
/// The native strip's `toolbar::PREFERRED_MIN_TAB_WIDTH` (64 pt) in cells: after the
/// pads and the reserved `✕` slot ([`tab_content_layout`]), 12 columns keep 8 title
/// cells ("Settings"), 7 on a chip-card band ([`preferred_min_title_cols`]).
const PREFERRED_MIN_TAB_COLS: u16 = 12;

/// The most band the active tab may reserve under the pressure layout (two tabs
/// below the floor): the native strip's 96 pt cap (`toolbar::native_tab_cells`) at
/// the same 96:64 ratio (1.5 × 12 = 18). Below the cap the share is 60% of the band.
const ACTIVE_TAB_PRESSURE_CAP_COLS: u16 = 18;

/// The chip width, in columns, below which the strip stops compressing and seats a
/// window of its tabs instead ([`layout_segments`]), marking continued edges with
/// `‹`/`›` like the inline rename well ([`paint_rename_field`]).
///
/// Equal to [`PREFERRED_MIN_TAB_COLS`]: below it no title cut can name a tab. It is
/// the plain chip's floor, where the seat search starts ([`chip_seat_cols`]); a chip
/// that also carries a status canvas or an app icon needs more columns.
const STRIP_SEATS_FLOOR: u16 = PREFERRED_MIN_TAB_COLS;

/// The fewest chips a windowed strip seats, however narrow the band. One chip is a
/// title bar, not a switcher. Seats are also capped at one per column, so a band
/// too narrow for two chips seats one rather than emitting zero-width segments.
const STRIP_MIN_SEATS: usize = 2;

/// How many chips `band` columns seat: one per `seat` columns, where `seat` is the
/// narrowest chip this strip's chrome can carry a title in ([`strip_seat_cols`]).
///
/// Clamped into `STRIP_MIN_SEATS..=tab_count` (fewer with fewer tabs) and to one
/// seat per column. No active reservation: seated chips share the band equally
/// ([`strip_window`]).
fn strip_seats(band: u16, tab_count: usize, seat: u16) -> usize {
    usize::from(band / seat.max(1))
        .clamp(STRIP_MIN_SEATS.min(tab_count.max(1)), tab_count.max(1))
        .min(usize::from(band).max(1))
}

/// The window of tabs the strip seats when `active` is selected, as
/// `(first, len)`: a page of a fixed partition of the tab list, never a run
/// centred on the selection.
///
/// The window is recomputed every frame, so a window centred on the selection
/// would re-anchor the strip on every click and move chips under a still pointer.
/// Requiring that selecting any seated tab yields the same window forces a
/// partition. Crossing a page edge moves the whole strip at once.
///
/// Pages: as few as the seat count allows (`tab_count.div_ceil(seats)`), sized as
/// evenly as possible with the longer pages first, so no page is a runt. Every
/// page spends the whole band, so a shorter page means wider chips.
fn strip_window(active: usize, seats: usize, tab_count: usize) -> (usize, usize) {
    let seats = seats.max(1);
    if tab_count <= seats {
        return (0, tab_count);
    }
    let pages = tab_count.div_ceil(seats);
    let narrow = tab_count / pages;
    // How many pages carry the extra tab; `narrow + 1` wide when any do.
    let long = tab_count % pages;
    let wide = narrow + usize::from(long > 0);
    if active < long * wide {
        let page = active / wide;
        (page * wide, wide)
    } else {
        let page = (active - long * wide) / narrow.max(1);
        (long * wide + page * narrow, narrow)
    }
}

/// Cell-space content order inside an in-grid tab: pad, two-cell icon, one-cell
/// gap, title, optional status canvas, close affordance. Narrow tabs drop the icon
/// and status before they move the close hit target.
const ICON_COLS: u16 = 2;
const ICON_GAP: u16 = 1;

/// Point-space content geometry shared with the macOS view renderer. The whole tab
/// view is the select/reorder target. Wide chips reserve a close target; compact
/// chips give that space to the title. Icon and status slots appear only after a
/// useful title width survives.
///
/// The label's leading and trailing insets are equal, so a centred title stays
/// centred. The close slot is reserved whether or not the hover-only `✕` is
/// painted, so revealing it never reflows the title (`toolbar::TabView`). The
/// icon takes the leading slot and the status canvas the trailing one, each only
/// while the label still clears the legibility floor.
#[derive(Clone, Copy, PartialEq, Debug)]
#[cfg(any(target_os = "macos", test))]
pub(crate) struct NativeTabContentLayout {
    pub(crate) close: [f64; 4],
    pub(crate) close_available: bool,
    pub(crate) icon: Option<[f64; 4]>,
    pub(crate) status: Option<[f64; 4]>,
    pub(crate) label: [f64; 4],
}

/// Preference order for the solo title band's subtitle: authored description, then
/// location, then activity. `state` is excluded: its steady value "alive" says
/// nothing.
const SOLO_SUBTITLE_KEYS: [&str; 3] = ["description", "cwd", "activity"];

/// The one-line description a single-tab window shows beside its title, from the
/// same composed session chrome (`session_chrome::compose_tooltip`) the hover card
/// and context menu render. With one tab the strip is the window title; `None`
/// when the session carries nothing the title does not already say.
#[must_use]
pub(crate) fn solo_subtitle(title: &str, tooltip: Option<&str>) -> Option<String> {
    let tooltip = tooltip?;
    SOLO_SUBTITLE_KEYS.iter().find_map(|key| {
        tooltip
            .lines()
            .find_map(|line| line.strip_prefix(key)?.strip_prefix(": "))
            .map(str::trim)
            .filter(|value| !value.is_empty() && value != &"-" && !title.contains(*value))
            .map(str::to_string)
    })
}

/// Lay a tab chip's interior out around `center_y`, the strip's vertical centre
/// measured from the traffic lights (`toolbar::strip_metrics`). Every vertical
/// position derives from it.
#[must_use]
#[cfg(any(target_os = "macos", test))]
pub(crate) fn native_tab_content_layout(
    width: f64,
    center_y: f64,
    has_icon: bool,
    has_status: bool,
) -> NativeTabContentLayout {
    // The hover-only ✕ has a slot reserved on both edges so revealing it never
    // reflows the title; that slot is a permanent tax on title width, so the
    // target is small.
    const CLOSE_X: f64 = 2.0;
    const CLOSE_SIZE: f64 = 16.0;
    const AFTER_CLOSE: f64 = 2.0;
    const AFTER_ICON: f64 = 4.0;
    const STATUS_SIZE: f64 = 16.0;
    const BEFORE_STATUS: f64 = 4.0;
    const WIDE_TRAILING: f64 = 6.0;
    const COMPACT_INSET: f64 = 4.0;
    const LABEL_H: f64 = 17.0;
    const MIN_USEFUL_LABEL: f64 = 36.0;
    // AppKit's bold 12 pt "Settings" measures just under 55 pt. A compact chip
    // must preserve that identity before it admits icon/status ornamentation.
    const MIN_COMPACT_IDENTITY_LABEL: f64 = 55.0;
    // Below this width the title outranks the close target, which yields its slot.
    const MIN_CLOSE_RESERVING_WIDTH: f64 = 110.0;

    let centred = |size: f64| center_y - size * 0.5;
    let close = [CLOSE_X, centred(CLOSE_SIZE), CLOSE_SIZE, CLOSE_SIZE];
    let close_available = width >= MIN_CLOSE_RESERVING_WIDTH;
    let lead_base = if close_available {
        CLOSE_X + CLOSE_SIZE + AFTER_CLOSE
    } else {
        COMPACT_INSET
    };
    let trail_base = if close_available {
        WIDE_TRAILING
    } else {
        COMPACT_INSET
    };
    let minimum_label = if close_available {
        MIN_USEFUL_LABEL
    } else {
        MIN_COMPACT_IDENTITY_LABEL
    };
    // What the centred label would measure if the two edges cost `lead` and `trail`:
    // the wider edge sets both insets, because the title is centred in the cell.
    let symmetric_label = |lead: f64, trail: f64| (width - 2.0 * f64::max(lead, trail)).max(0.0);

    let icon_lead = lead_base + TAB_ICON_NATIVE_SIZE + AFTER_ICON;
    let icon = (has_icon && symmetric_label(icon_lead, trail_base) >= minimum_label).then(|| {
        [
            lead_base,
            centred(TAB_ICON_NATIVE_SIZE),
            TAB_ICON_NATIVE_SIZE,
            TAB_ICON_NATIVE_SIZE,
        ]
    });
    let lead = if icon.is_some() { icon_lead } else { lead_base };

    let status_trail = trail_base + STATUS_SIZE + BEFORE_STATUS;
    let status = (has_status && symmetric_label(lead, status_trail) >= minimum_label).then(|| {
        [
            width - trail_base - STATUS_SIZE,
            centred(STATUS_SIZE),
            STATUS_SIZE,
            STATUS_SIZE,
        ]
    });
    let trail = if status.is_some() {
        status_trail
    } else {
        trail_base
    };

    let inset = f64::max(lead, trail);
    let label = [
        inset,
        centred(LABEL_H),
        (width - 2.0 * inset).max(0.0),
        LABEL_H,
    ];
    NativeTabContentLayout {
        close,
        close_available,
        icon,
        status,
        label,
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
struct TabContentLayout {
    icon_start: Option<u16>,
    title_start: u16,
    title_end: u16,
    status_col: Option<u16>,
}

/// Metadata of a plain terminal tab (no icon, no status, closable). The painter
/// uses it when handed no metadata slice, so both paths share
/// [`tab_content_layout`].
const PLAIN_TAB: TabStripMetadata = TabStripMetadata {
    icon: None,
    dirty: false,
    busy: false,
    attention: ChipLevel::Off,
    conn: None,
    team: TabTeam::NONE,
    closable: true,
    load: 0,
};

/// Title/icon/status slots for one laid-out tab segment. The segment and
/// `close_col` stay authoritative hit geometry; this only divides paint space.
/// When width is scarce the title narrows first, then the icon drops, then the
/// status canvas.
fn tab_content_layout(seg: &TabSegment, metadata: TabStripMetadata) -> TabContentLayout {
    // A roomy card keeps one interior pad cell after the gutter; a card below the
    // legibility floor spends that cell on title.
    let pad = u16::from(
        STRIP_CHIP_CARDS && seg.end_col.saturating_sub(seg.start_col) >= PREFERRED_MIN_TAB_COLS,
    );
    let leading = seg.start_col.saturating_add(1 + pad);
    let mut title_end = match seg.close_col {
        Some(close) => close.saturating_sub(1),
        // Without a close cell, pad the trailing side equally so a centred label
        // lands on the card's true centre.
        None => seg.end_col.saturating_sub(1 + pad),
    };

    // All status marks share one shape-coded status canvas, which costs the title
    // three cells and shows only while the title span has them; paint never
    // overwrites the close affordance.
    //
    // A closable chip keeps its mark one cell inside the ✕; a non-closable one
    // takes the cell the ✕ would have owned (`end_col - 2`), never the label's
    // trailing pad.
    let status_anchor = match seg.close_col {
        Some(close) => close.saturating_sub(2),
        None => seg.end_col.saturating_sub(2),
    };
    let status_col = if metadata.has_status() && title_end >= leading.saturating_add(3) {
        title_end = title_end.min(status_anchor.saturating_sub(2));
        Some(status_anchor)
    } else {
        None
    };

    // Reserve icon + gap only when one title cell still fits after it. Unknown icon
    // metadata consumes no space, failing closed without an unexplained blank slot.
    let icon_fits =
        metadata.icon.is_some() && title_end >= leading.saturating_add(ICON_COLS + ICON_GAP + 1);
    let icon_start = icon_fits.then_some(leading);
    let title_start = if icon_fits {
        leading + ICON_COLS + ICON_GAP
    } else {
        leading
    };

    TabContentLayout {
        icon_start,
        title_start,
        title_end: title_end.max(title_start),
        status_col,
    }
}

/// How many title cells a `width`-column chip wearing `chrome` keeps:
/// [`tab_content_layout`] asked of a hypothetical segment.
///
/// The legibility floor is about the name, which is painted into
/// `title_start..title_end`, not the whole segment. Pads, icon, close slot and
/// status canvas tax chips unequally (a busy chip spends three more cells than
/// an idle one), so the seat gate measures title cells.
fn chip_title_cols(width: u16, chrome: TabStripMetadata) -> u16 {
    let seg = TabSegment {
        start_col: 0,
        end_col: width,
        // The same two conditions the layouts apply, in order: a segment too
        // narrow for ` x ` reserves no close cell, and a non-closable chip gives
        // that cell back.
        close_col: (chrome.closable && width >= MIN_SEG_WITH_CLOSE).then(|| width - 2),
        kind: TabHit::Select(0),
        solo: false,
    };
    let layout = tab_content_layout(&seg, chrome);
    layout.title_end.saturating_sub(layout.title_start)
}

/// The legibility floor in title cells: what a plain chip keeps at
/// [`PREFERRED_MIN_TAB_COLS`]. Derived from [`tab_content_layout`] so the two
/// cannot drift (7 on a chip-card band, whose interior pad costs a cell; 8 on
/// padless bands).
fn preferred_min_title_cols() -> u16 {
    chip_title_cols(PREFERRED_MIN_TAB_COLS, PLAIN_TAB)
}

/// The narrowest chip that still carries a legible title while wearing `chrome`;
/// the per-tab reading of [`STRIP_SEATS_FLOOR`]. Scanned upward from
/// [`PREFERRED_MIN_TAB_COLS`] because pad, icon and status each switch on at
/// their own width; bounded by [`SEAT_COLS_CEILING`].
fn chip_seat_cols(chrome: TabStripMetadata) -> u16 {
    let floor = preferred_min_title_cols();
    (STRIP_SEATS_FLOOR..=SEAT_COLS_CEILING)
        .find(|&width| chip_title_cols(width, chrome) >= floor)
        .unwrap_or(SEAT_COLS_CEILING)
}

/// The widest a chrome-aware seat can be: [`STRIP_SEATS_FLOOR`] plus the icon and
/// its gap, the three cells the status canvas costs a title, and one column of
/// slack so the scan in [`chip_seat_cols`] always ends on a real answer
/// (asserted by `the_legibility_floor_is_a_title_width_and_every_seat_is_minimal`).
const SEAT_COLS_CEILING: u16 = STRIP_SEATS_FLOOR + (ICON_COLS + ICON_GAP) + 3 + 1;

/// The narrowest chip this strip can seat: the widest seat any of its tabs needs
/// ([`chip_seat_cols`]). Quiet chips share one width, so the tab paying the most
/// chrome decides. Tabs past the end of `metadata` read as [`PLAIN_TAB`], as the
/// painter reads them.
fn strip_seat_cols(tab_count: usize, metadata: &[TabStripMetadata]) -> u16 {
    (0..tab_count)
        .map(|i| chip_seat_cols(metadata.get(i).copied().unwrap_or(PLAIN_TAB)))
        .max()
        .unwrap_or(PREFERRED_MIN_TAB_COLS)
}

/// Lay out the tab strip across `cols` columns for `tab_count` tabs (`active` is
/// the highlighted one). Returns one [`TabSegment`] per visible tab plus, when
/// room remains, a trailing [`TabHit::NewTab`] `+` segment, packed left to right.
/// `tab_count == 0` yields just the `+`.
///
/// Same rules as the native strip (`toolbar::native_tab_cells` /
/// `toolbar::set_window_tabs`):
/// - tabs divide the band into equal shares, each capped at [`CHIP_MAX_COLS`]
///   on Windows (no maximum elsewhere);
/// - a lone tab is the window title: one full-band [`TabSegment::solo`] segment
///   with no close column (macOS only; elsewhere it is a leading chip capped at
///   [`SOLO_CHIP_MAX_COLS`], see [`SOLO_TITLE_BAND`]);
/// - below [`STRIP_SEATS_FLOOR`] compression stops and the strip seats a window
///   of tabs containing the selection, marking continued edges with `‹`/`›`
///   ([`strip_overflow`]); unseated tabs stay reachable by cycling and
///   `switch_tab_<n>`;
/// - only at the window's two-chip minimum ([`STRIP_MIN_SEATS`]) does the active
///   tab reserve 60% of the band, capped at [`ACTIVE_TAB_PRESSURE_CAP_COLS`], with
///   the other compressing around it.
///
/// This entry measures the legibility floor against a [`PLAIN_TAB`] chip, having no
/// metadata. The shipping strip goes through [`layout_segments_with_version`], which
/// measures it against the chrome its tabs wear ([`strip_seat_cols`]); the two agree
/// wherever every tab is plain.
#[cfg(test)]
#[must_use]
pub(crate) fn layout_segments(
    cols: u16,
    tab_count: usize,
    active: usize,
    show_update: bool,
) -> Vec<TabSegment> {
    layout_segments_seated(
        cols,
        tab_count,
        active,
        show_update,
        chip_seat_cols(PLAIN_TAB),
        TabSizing::FIXED,
    )
}

/// [`layout_segments`] with the seat width (`seat`, from [`strip_seat_cols`]):
/// the one number the two entry points disagree about. On Windows the tab cap
/// and the tabs' growth are `sizing`'s ([`TabSizing`]).
fn layout_segments_seated(
    cols: u16,
    tab_count: usize,
    active: usize,
    show_update: bool,
    seat: u16,
    sizing: TabSizing<'_>,
) -> Vec<TabSegment> {
    let mut segs = Vec::new();
    if cols == 0 {
        return segs;
    }
    // The leading update icon (`↻`) while a build is staged; it shifts the tabs right.
    let lead = if show_update && cols > UPDATE_W {
        segs.push(TabSegment {
            start_col: 0,
            end_col: UPDATE_W,
            close_col: None,
            kind: TabHit::Update,
            solo: false,
        });
        UPDATE_W
    } else {
        0
    };
    // Reserve the trailing `+` when there's room for at least one tab and the `+`.
    let plus_room = cols > lead + NEW_TAB_W;
    let avail = if plus_room { cols - NEW_TAB_W } else { cols };
    // One tab is the window title where the window has no title of its own
    // (macOS, `SOLO_TITLE_BAND`): the whole band, no close column. Elsewhere a
    // lone tab is an ordinary chip with its card, selection rule and close column.
    let solo = tab_count == 1 && SOLO_TITLE_BAND;
    let mut x: u16 = lead;
    if tab_count > 0 {
        let band = avail.saturating_sub(lead);
        // Equal shares of the band past the leading update icon; floored so a
        // tab is at least 1 cell.
        let share = band / tab_count as u16;
        // A capped chip ([`CHIP_MAX_COLS`] on Windows, a lone one
        // [`SOLO_CHIP_MAX_COLS`] off macOS) is kept leading, so the band and the
        // `+` show to its right.
        let per = match (tab_count, SOLO_CHIP_MAX_COLS, CHIP_MAX_COLS) {
            (1, Some(_), _) | (_, _, Some(_)) if STRIP_WINDOWS_PAINT => share.min(sizing.cap),
            (1, Some(cap), _) | (_, _, Some(cap)) => share.min(cap),
            _ => share,
        };
        // Below the seat width the strip seats a page of `seats` chips from `first`
        // ([`strip_window`]) with a marked column at each continued end; when every
        // tab fits, `seats == tab_count`, `first == 0` and nothing is marked. A page
        // already at the two-chip minimum takes the active-priority split instead
        // (`toolbar::native_tab_cells`, in cells): the selected tab up to 60% of the
        // band, capped, the other the rest, floored at one cell.
        let active = active.min(tab_count - 1);
        let mut seats = tab_count;
        let mut first = 0usize;
        let (mut mark_left, mut mark_right) = (false, false);
        let (active_w, inactive_w) = if tab_count > 1 && share < seat {
            if strip_seats(band, tab_count, seat) >= tab_count {
                // Every tab is seated below the floor only at the two-chip minimum
                // ([`STRIP_MIN_SEATS`]): the pressure split. u32 because `band * 3`
                // can overflow u16.
                let selected = u16::try_from(u32::from(band) * 3 / 5)
                    .unwrap_or(u16::MAX)
                    .min(ACTIVE_TAB_PRESSURE_CAP_COLS)
                    .max(share);
                let inactive = (band.saturating_sub(selected) / (tab_count as u16 - 1)).max(1);
                (selected, inactive)
            } else {
                // The window. Count seats against the band left after both marks
                // (pessimistically; an unused mark gives its column back). Marks
                // appear only when the band still seats a chip at the floor beside
                // them; otherwise the chip gets the columns.
                let marked = band >= seat + 2;
                let counted = if marked { band - 2 } else { band };
                let (start, len) =
                    strip_window(active, strip_seats(counted, tab_count, seat), tab_count);
                seats = len;
                first = start;
                mark_left = marked && first > 0;
                mark_right = marked && first + seats < tab_count;
                let chip_band = band - u16::from(mark_left) - u16::from(mark_right);
                // Equal shares, no active reservation: every seated chip already
                // clears the floor, and a reservation would make seated geometry
                // depend on which chip is selected. The layout is a function of the
                // page, so selecting a seated chip moves nothing. The remainder
                // stays band.
                let per = (chip_band / seats.max(1) as u16).max(1);
                (per, per)
            }
        } else {
            (per, per)
        };
        // A Windows tab row with room past the equal width: each tab grows
        // toward its title ([`TabSizing`]), the room shared among the tabs that
        // want it. Never under pressure, and never past the band.
        let grown = (STRIP_WINDOWS_PAINT
            && !(tab_count > 1 && share < seat)
            && per == sizing.cap
            && sizing.grow > sizing.cap)
            .then(|| grown_widths(&sizing, first, seats.min(tab_count - first), per, band));
        x += u16::from(mark_left);
        for i in first..(first + seats).min(tab_count) {
            let per = match &grown {
                Some(widths) => widths[i - first],
                None if i == active => active_w,
                None => inactive_w,
            };
            if per == 0 || x >= avail {
                break; // out of room: remaining tabs are not drawn (still reachable)
            }
            let seg_w = per.min(avail - x);
            let start = x;
            let end = x + seg_w;
            // A close `x` only when the segment also fits a title; its column is
            // the last cell minus the trailing pad. A solo title band has none.
            let close_col = (!solo && seg_w >= MIN_SEG_WITH_CLOSE).then(|| end - 2);
            segs.push(TabSegment {
                start_col: start,
                end_col: end,
                close_col,
                kind: TabHit::Select(i),
                solo,
            });
            x = end;
        }
        // Skip the trailing mark's column: it owns no hit target but must not
        // share a cell with one.
        x = x.saturating_add(u16::from(mark_right)).min(avail);
    }
    // Trailing `+` (open a tab), placed flush after the last tab when it fits.
    if plus_room {
        let start = x.min(cols - NEW_TAB_W);
        segs.push(TabSegment {
            start_col: start,
            end_col: start + NEW_TAB_W,
            close_col: None,
            kind: TabHit::NewTab,
            solo: false,
        });
    }
    segs
}

/// The metadata-aware layout: the shipping strip's less its version label
/// ([`layout_segments_with_version`] at [`VersionSlotCols::NONE`]), the entry the
/// layout tests hold the strip's contract against. Segment order, select hit areas,
/// reorder geometry and the trailing `+` are [`layout_segments`]'s; only a tab marked
/// non-closable loses its close column, and no icon/status field moves a hit target.
///
/// The one thing metadata moves is the width at which the strip gives up on equal
/// shares ([`strip_seat_cols`]): a busy chip's title is three cells shorter than an
/// idle one's at the same width, so the floor is measured in the chips' title cells.
/// Above it the output is [`layout_segments`]'s to the column, so a tab going busy
/// cannot move the hover-reserved `✕` on a strip with width to spare.
#[cfg(test)]
#[must_use]
pub(crate) fn layout_segments_with_metadata(
    cols: u16,
    tab_count: usize,
    metadata: &[TabStripMetadata],
    active: usize,
    show_update: bool,
) -> Vec<TabSegment> {
    layout_segments_with_version(
        cols,
        tab_count,
        metadata,
        active,
        show_update,
        VersionSlotCols::NONE,
    )
}

/// How many columns each rung of the strip's version label needs at this window's
/// metrics ([`pixel_band::version_slot_cols`] measures them in the band's UI face and
/// pen), and whether the label carries the update mark. A width of 0 is a rung that
/// does not exist here (a release's short rung, or any rung before the UI face is
/// installed), so [`Self::NONE`] lays out no label.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, Hash)]
pub(crate) struct VersionSlotCols {
    /// Columns of the full label ([`crate::menu::VersionRung::Full`]), 0 = none.
    pub(crate) full: u16,
    /// Columns of the short label ([`crate::menu::VersionRung::Short`]), 0 = none.
    pub(crate) short: u16,
    /// An update is staged: the label ends ` ↑` and wears the update card.
    pub(crate) attention: bool,
}

impl VersionSlotCols {
    /// No label: no strip, no UI face yet, or a platform without the slot.
    pub(crate) const NONE: Self = Self {
        full: 0,
        short: 0,
        attention: false,
    };
}

/// THE STRIP LAYOUT WITHOUT A PILL, its tabs equal at [`MAX_CHIP_COLS`]
/// ([`layout_segments_with_version_sized`] sizes them):
/// [`layout_segments_seated`] and the metadata pass, plus — where it fits — the
/// build's VERSION LABEL flush against the strip's right edge
/// ([`TabHit::Version`], `[cols - w, cols)`). The shipping splice calls
/// [`layout_segments_with_pill`], which is this exactly while the window shows
/// no needs pill and its tabs are not sized; the layout tests hold the strip's
/// own contract here.
///
/// The label yields; the tabs never do. Everything left of the label (tabs, `+`,
/// leading `↻`, `‹`/`›` marks) is the layout of `cols - w` columns, so nothing lands
/// in the label's columns, and the label is admitted only while it costs no tab
/// anything ([`version_rung_that_fits`]). The full rung is tried first, then the
/// short one.
#[cfg(test)]
#[must_use]
pub(crate) fn layout_segments_with_version(
    cols: u16,
    tab_count: usize,
    metadata: &[TabStripMetadata],
    active: usize,
    show_update: bool,
    slot: VersionSlotCols,
) -> Vec<TabSegment> {
    layout_segments_with_version_sized(
        cols,
        tab_count,
        metadata,
        active,
        show_update,
        slot,
        TabSizing::FIXED,
    )
}

/// [`layout_segments_with_version`] with the Windows tabs sized by `sizing`
/// ([`TabSizing`]).
#[cfg(test)]
#[must_use]
pub(crate) fn layout_segments_with_version_sized(
    cols: u16,
    tab_count: usize,
    metadata: &[TabStripMetadata],
    active: usize,
    show_update: bool,
    slot: VersionSlotCols,
    sizing: TabSizing<'_>,
) -> Vec<TabSegment> {
    layout_segments_with_pill(
        cols,
        tab_count,
        metadata,
        active,
        show_update,
        slot,
        None,
        sizing,
    )
}

/// THE SHIPPING STRIP LAYOUT, WITH THE WINDOW'S NEEDS PILL
/// ([`layout_segments_with_version_sized`]'s exactly when `pill` is `None`),
/// its Windows tabs sized by `sizing` ([`TabSizing`]). Unlike the
/// version label the pill outranks the tabs, within its share (at most 40%
/// of the strip, [`strip_pill_layout`]): it takes the trailing columns left
/// of the version label, the label is admitted against what the pill leaves,
/// and the tabs, the `+` and the `↻` are laid out left of both, as ever. Its
/// regions are segments of their own ([`TabHit::Pill`]), cut from the
/// engine's layout by its own hit rule (`PillLayout::hit` at each cell's
/// centre; a pad or a gap belongs to the region left of it), so a click
/// lands on what the painter drew there ([`paint_strip_pill`]).
#[allow(
    clippy::too_many_arguments,
    reason = "the strip's layout inputs, each independent: width, tabs, chrome, \
              selection, the update alert, the version label, the needs pill and \
              the tabs' sizing"
)]
#[must_use]
pub(crate) fn layout_segments_with_pill(
    cols: u16,
    tab_count: usize,
    metadata: &[TabStripMetadata],
    active: usize,
    show_update: bool,
    slot: VersionSlotCols,
    pill: Option<&crate::presence::PillWords>,
    sizing: TabSizing<'_>,
) -> Vec<TabSegment> {
    let seat = strip_seat_cols(tab_count, metadata);
    let lone = if STRIP_WINDOWS_PAINT {
        sizing.cap
    } else {
        LONE_CHIP_COLS
    };
    let pill = pill.and_then(|words| strip_pill_layout(cols, words));
    let pill_w = pill.as_ref().map_or(0, strip_pill_cols);
    let inner = cols - pill_w;
    let version = version_rung_that_fits(inner, tab_count, seat, show_update, slot, lone);
    let laid_cols = version.map_or(inner, |(_, w)| inner - w);
    let mut segments =
        layout_segments_seated(laid_cols, tab_count, active, show_update, seat, sizing);
    for segment in &mut segments {
        let TabHit::Select(index) = segment.kind else {
            continue;
        };
        if metadata.get(index).is_some_and(|item| !item.closable) {
            segment.close_col = None;
        }
    }
    if let Some(layout) = pill.as_ref() {
        push_pill_segments(&mut segments, layout, laid_cols);
    }
    if let Some((rung, w)) = version {
        let start = laid_cols + pill_w;
        segments.push(TabSegment {
            start_col: start,
            end_col: start + w,
            close_col: None,
            kind: TabHit::Version {
                rung,
                attention: slot.attention,
            },
            solo: false,
        });
    }
    segments
}

/// THE WINDOW'S NEEDS PILL as the in-grid strip carries it (owner surfaces
/// §5; a macOS window on glass draws it in its titlebar instead,
/// `toolbar.rs`'s `NeedsPill`): what lit it and its words — the engine's one
/// model, laid out here in strip cells ([`strip_pill_layout`]).
#[derive(Clone, Copy, Debug)]
pub(crate) struct StripPill<'a> {
    /// What lit it: its glyph's tone.
    pub(crate) kind: crate::presence::PillKind,
    /// Its words, in the pieces the layout clips.
    pub(crate) words: &'a crate::presence::PillWords,
}

/// The in-grid pill's share of a `cols`-wide strip, its pads included: at
/// most 40% of the strip (§5), as the titlebar pill's is of its band.
fn strip_pill_share(cols: u16) -> u16 {
    u16::try_from(u32::from(cols) * 2 / 5).unwrap_or(u16::MAX)
}

/// The in-grid pill laid out in its share of a `cols`-wide strip: the
/// engine's `pill_layout` measured in strip cells ([`strip_display_cells`]),
/// one cell between its pieces, inside a pad cell at each end — the words
/// clip first, then the label, never the glyph. `None` where the share has no
/// room for the glyph. The ONE geometry the layout cuts the pill's hit
/// regions from and its painter draws by.
#[must_use]
pub(crate) fn strip_pill_layout(
    cols: u16,
    words: &crate::presence::PillWords,
) -> Option<aterm_messages::presence::PillLayout> {
    let inner = strip_pill_share(cols).checked_sub(2).filter(|w| *w > 0)?;
    let layout = aterm_messages::presence::pill_layout(words, f64::from(inner), 1.0, |s| {
        strip_display_cells(s) as f64
    });
    (layout.width >= 1.0 && layout.width <= f64::from(inner)).then_some(layout)
}

/// The columns a laid-out in-grid pill takes: its pieces and its two pads.
fn strip_pill_cols(layout: &aterm_messages::presence::PillLayout) -> u16 {
    cell_of(layout.width).saturating_add(2)
}

/// A whole-cell span edge of the in-grid pill's layout (measured in cells, so
/// every edge is a whole number) as a column offset.
fn cell_of(x: f64) -> u16 {
    x.round().clamp(0.0, f64::from(u16::MAX)) as u16
}

/// The pill's regions from column `start` on, one [`TabHit::Pill`] segment
/// per run of cells the engine's hit rule (`PillLayout::hit` at the cell's
/// centre) gives one answer; a pad or a gap cell takes the region left of it,
/// the leading pad the body's. Every column of the pill is one of its
/// regions, so no click on it falls through to the bare band.
fn push_pill_segments(
    segments: &mut Vec<TabSegment>,
    layout: &aterm_messages::presence::PillLayout,
    start: u16,
) {
    let width = strip_pill_cols(layout);
    let mut region = crate::presence::PillHit::Go;
    let mut run_start = start;
    for offset in 0..width {
        // The pill's own coordinate: its pieces start past the leading pad.
        let x = f64::from(offset) - 1.0 + 0.5;
        let hit = layout.hit(x).unwrap_or(region);
        if hit != region {
            segments.push(pill_segment(run_start, start + offset, region));
            region = hit;
            run_start = start + offset;
        }
    }
    segments.push(pill_segment(run_start, start + width, region));
}

fn pill_segment(start_col: u16, end_col: u16, hit: crate::presence::PillHit) -> TabSegment {
    TabSegment {
        start_col,
        end_col,
        close_col: None,
        kind: TabHit::Pill(hit),
        solo: false,
    }
}

/// The cells a strip keeps left of the window buttons: the `+` and a few cells of
/// tab, so the narrowest window still opens and shows a tab.
const CAPTION_STRIP_FLOOR: u16 = NEW_TAB_W + 3;

/// [`layout_segments_with_caption_sized`] with no needs pill and equal tabs:
/// the header strip the caption tests hold the buttons' contract against.
#[cfg(test)]
#[must_use]
pub(crate) fn layout_segments_with_caption(
    cols: u16,
    tab_count: usize,
    metadata: &[TabStripMetadata],
    active: usize,
    show_update: bool,
    slot: VersionSlotCols,
    caption: CaptionSlot,
) -> Vec<TabSegment> {
    layout_segments_with_caption_sized(
        cols,
        tab_count,
        metadata,
        active,
        show_update,
        slot,
        None,
        caption,
        TabSizing::FIXED,
    )
}

/// THE HEADER STRIP, the shipping layout: [`layout_segments_with_pill`] (the
/// tabs, sized by `sizing` ([`TabSizing`]: the cap in DIPs at the window's
/// metrics, and each tab's growth toward its title), the version label and
/// the window's needs pill) on the cells between the window's own buttons,
/// and the buttons ([`TabHit::Caption`]) flush against the strip's edges
/// where the desktop's layout puts them ([`crate::client_frame::ButtonLayout`]:
/// minimize, maximize and close at the right end unless it says otherwise),
/// one [`CaptionSlot::button_cols`]-wide segment each.
///
/// THE TABS YIELD; THE BUTTONS DO NOT (the version label's rule, reversed): a
/// window whose band is its header cannot lose its close button to a long tab
/// list, nor to the pill, which takes its share of the cells the buttons
/// leave ([`paint_strip_pill`] lays it out in the same cells). Only a strip
/// narrower than [`CAPTION_STRIP_FLOOR`] plus the buttons sheds them
/// ([`CaptionSlot::fit`]). [`CaptionSlot::NONE`] (every window the window
/// system decorates) is exactly [`layout_segments_with_pill`].
#[allow(
    clippy::too_many_arguments,
    reason = "the strip's layout inputs, each independent: width, tabs, chrome, \
              selection, the update alert, the version label, the needs pill, the \
              window buttons and the tabs' sizing"
)]
#[must_use]
pub(crate) fn layout_segments_with_caption_sized(
    cols: u16,
    tab_count: usize,
    metadata: &[TabStripMetadata],
    active: usize,
    show_update: bool,
    slot: VersionSlotCols,
    pill: Option<&crate::presence::PillWords>,
    caption: CaptionSlot,
    sizing: TabSizing<'_>,
) -> Vec<TabSegment> {
    use crate::client_frame::Side;
    let caption = caption.fit(cols, CAPTION_STRIP_FLOOR);
    let lead = caption.side_cols(Side::Left);
    let inner = cols.saturating_sub(caption.cols());
    let mut segments = layout_segments_with_pill(
        inner,
        tab_count,
        metadata,
        active,
        show_update,
        slot,
        pill,
        sizing,
    );
    // The left end's buttons come first: the strip's own segments move
    // right past them.
    if lead > 0 {
        for seg in &mut segments {
            seg.start_col = seg.start_col.saturating_add(lead);
            seg.end_col = seg.end_col.saturating_add(lead);
            seg.close_col = seg.close_col.map(|c| c.saturating_add(lead));
        }
    }
    let width = caption.button_cols();
    for (side, from) in [(Side::Left, 0), (Side::Right, lead.saturating_add(inner))] {
        for (i, button) in caption.on(side).enumerate() {
            let start_col =
                from.saturating_add(u16::try_from(i).unwrap_or(u16::MAX).saturating_mul(width));
            segments.push(TabSegment {
                start_col,
                end_col: start_col.saturating_add(width),
                close_col: None,
                kind: TabHit::Caption(button),
                solo: false,
            });
        }
    }
    segments
}

/// Which rung of the version label fits beside `tab_count` tabs on a `cols`-wide
/// strip, and how wide it is; `None` shows no label.
///
/// Full first, then short; a rung of width 0 (absent) or one at least as wide as
/// the strip is skipped. A rung fits when the strip LEFT of it (`inner`) still
/// holds the `↻` (if shown) and the `+`, and the band between them still gives:
/// no tab — always; one tab — a whole `lone` chip ([`LONE_CHIP_COLS`], on
/// Windows the window's tab cap, [`TabSizing::cap`]), so the lone chip never
/// shrinks for the label; `n ≥ 2` tabs — an equal share of at least `seat`
/// ([`strip_seat_cols`]) each, so the label yields before the strip would start
/// compressing or windowing its tabs.
fn version_rung_that_fits(
    cols: u16,
    tab_count: usize,
    seat: u16,
    show_update: bool,
    slot: VersionSlotCols,
    lone: u16,
) -> Option<(crate::menu::VersionRung, u16)> {
    [
        (crate::menu::VersionRung::Full, slot.full),
        (crate::menu::VersionRung::Short, slot.short),
    ]
    .into_iter()
    .filter(|&(_, w)| w > 0 && w < cols)
    .find(|&(_, w)| {
        let inner = cols - w;
        // The same lead rule `layout_segments_seated` applies to `inner`.
        let lead = if show_update && inner > UPDATE_W {
            UPDATE_W
        } else {
            0
        };
        if inner <= lead + NEW_TAB_W {
            return false;
        }
        let band = inner - NEW_TAB_W - lead;
        match tab_count {
            0 => true,
            1 => band >= lone,
            n => {
                let n = u32::try_from(n).unwrap_or(u32::MAX);
                u32::from(band) / n >= u32::from(seat)
            }
        }
    })
}

/// Map a strip click at column `col` to its [`TabHit`], or `None` for a click on
/// bare strip background (between/after segments). A click on a segment's
/// `close_col` is a [`TabHit::Close`]; any other column of a tab segment,
/// its status marks included, selects it; the `+` segment opens a tab.
#[must_use]
pub(crate) fn hit_test(segments: &[TabSegment], col: u16) -> Option<TabHit> {
    for seg in segments {
        if col >= seg.start_col && col < seg.end_col {
            if let (Some(cx), TabHit::Select(i)) = (seg.close_col, seg.kind)
                && col == cx
            {
                return Some(TabHit::Close(i));
            }
            return Some(seg.kind);
        }
    }
    None
}

/// The columns the `‹` / `›` overflow marks paint on, or `None` for an edge that
/// reaches the end of the tab list.
///
/// Read from the segments, like [`hit_test`], by both painters (the cell lane on
/// the baseline, `pixel_band` as strokes on the band's centre). It only names
/// columns no segment covers, so a mark never lands on a hit target.
///
/// The marks are signs, not buttons, as in [`paint_rename_field`]: the window is a
/// function of the selection, so reaching a marked-off tab means selecting it.
#[must_use]
pub(crate) fn strip_overflow(segments: &[TabSegment], tab_count: usize) -> StripOverflow {
    let mut lo: Option<(usize, u16)> = None;
    let mut hi: Option<(usize, u16)> = None;
    for seg in segments {
        let TabHit::Select(i) = seg.kind else {
            continue;
        };
        // The solo band is the window's title and the only tab: nothing to overflow.
        if seg.solo {
            return StripOverflow::NONE;
        }
        if lo.is_none_or(|(j, _)| i < j) {
            lo = Some((i, seg.start_col));
        }
        if hi.is_none_or(|(j, _)| i > j) {
            hi = Some((i, seg.end_col));
        }
    }
    let free = |col: u16| {
        !segments
            .iter()
            .any(|seg| col >= seg.start_col && col < seg.end_col)
    };
    StripOverflow {
        left: lo
            .and_then(|(tab, start)| (tab > 0 && start > 0 && free(start - 1)).then(|| start - 1)),
        right: hi.and_then(|(tab, end)| (tab + 1 < tab_count && free(end)).then_some(end)),
    }
}

/// The two overflow-mark columns [`strip_overflow`] resolves.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub(crate) struct StripOverflow {
    /// The column carrying `‹` — tabs before the window's first chip.
    pub(crate) left: Option<u16>,
    /// The column carrying `›` — tabs after the window's last chip.
    pub(crate) right: Option<u16>,
}

impl StripOverflow {
    /// Neither edge overflows: every tab is on the strip.
    pub(crate) const NONE: Self = Self {
        left: None,
        right: None,
    };

    /// The mark columns that exist, in strip order — what both painters walk.
    pub(crate) fn columns(self) -> impl Iterator<Item = (u16, char)> {
        [
            self.left.map(|col| (col, STRIP_OVERFLOW_LEFT)),
            self.right.map(|col| (col, STRIP_OVERFLOW_RIGHT)),
        ]
        .into_iter()
        .flatten()
    }
}

/// What a strip cell represents, selecting its precomputed tone in [`strip_cell`].
#[derive(Clone, Copy, PartialEq, Eq)]
enum StripRole {
    /// The focused tab: full-strength bold fg on the raised button bg.
    Active,
    /// An unfocused tab or the bare strip: dimmed fg on the body bg (recedes).
    Inactive,
    /// An unfocused tab's card on a chip-card band ([`STRIP_CHIP_CARDS`]): dim
    /// label on a quiet chip surface. Bare strip stays [`StripRole::Inactive`], so
    /// gutters between cards show the band.
    Chip,
    /// An unfocused tab under the pointer (chrome band only): the inactive label on
    /// a faint wash, like the native strip's hover pill (`toolbar.rs`). macOS's
    /// flat in-grid strip changes only the `✕` on hover.
    Hover,
    /// The vertical hairline on a quiet chip's leading pad cell (chrome band only),
    /// so identical adjacent titles still read as separate tabs. Seam ink;
    /// suppressed beside the active and hovered chips ([`strip_separates`]).
    Separator,
    /// The trailing `+` new-tab affordance: a button, full-strength fg on the body
    /// (not the dim inactive treatment), so it meets WCAG-AA contrast on every theme.
    NewTab,
    /// The `+` under the pointer: the band's hover material
    /// ([`StripColors::hover_bg`]) under the button's full-strength ink.
    NewTabHover,
    /// The solo band's title: full-strength fg on the body, no raised card and no
    /// selection underline.
    Title,
    /// The leading `↻` update-ready alert: a raised highlighted button (the active-tab
    /// treatment) so it stands out from the flat `+` and the receded tabs.
    Update,
}

/// Whether the in-grid strip paints itself as a chrome band (own surface tone, a
/// hairline seam against the grid, a raised `+`) instead of receding into the
/// terminal background.
///
/// Not on macOS: there the in-grid strip is a fallback for the native AppKit
/// toolbar (`DEFAULT_TAB_STRIP_ROWS` is 0) and composites over titlebar vibrancy,
/// where a grey band reads heavy (see [`strip_colors`]). Elsewhere it is the
/// window's only chrome, so it takes a surface of its own.
pub(crate) const STRIP_IS_CHROME_BAND: bool = !cfg!(target_os = "macos");

/// Whether the strip carries the build's version label at its trailing end
/// ([`TabHit::Version`]).
///
/// Windows only, spelled `cfg!(windows)` rather than borrowed from
/// [`STRIP_IS_CHROME_BAND`], which covers Linux too: macOS shows the version as the
/// last menu of its menu bar (`menu::version_menu_bar_title`), and Windows has no
/// menu bar. A window with no strip (`tab_strip_rows == 0`) has no label.
pub(crate) const STRIP_VERSION_SLOT: bool = cfg!(windows);

/// The trailing version label's card and ink ([`TabHit::Version`]): the update CTA's
/// while an update is staged (it outranks the pointer, as the `↻` it echoes does);
/// the band's hover card and ink under the pointer; at rest no card and the inactive
/// ink, since it is strip content, not a tab or a caption button. The band raster
/// paints from it, and the High Contrast contrast rows hold these pairs to the
/// UI-text floor.
#[cfg(any(windows, target_os = "linux", test))]
fn version_label_material(
    colors: &StripColors,
    attention: bool,
    hovered: bool,
) -> (Option<[u8; 3]>, [u8; 3]) {
    if attention {
        (Some(colors.update_bg), colors.update_fg)
    } else if hovered {
        (Some(colors.hover_bg), colors.hover_fg)
    } else {
        (None, colors.inactive_fg)
    }
}

/// Whether a lone tab collapses into a full-width window-title band (centred title +
/// description, no card, no `✕`) instead of painting an ordinary selected chip.
///
/// macOS hides the OS title text (`setTitleVisibility(NSWindowTitleHidden)`,
/// `toolbar.rs`), so the solo band is the only place the title appears. Windows and
/// Linux keep an OS caption with the same string, so a lone tab stays a chip there.
const SOLO_TITLE_BAND: bool = cfg!(target_os = "macos");

/// Whether the chrome band uses chip cards: every tab an inset card on the band
/// (quiet chips a half-step off it, the hovered chip a full step, the selected
/// card strongly filled), separated by gutter columns of bare band instead of `│`
/// glyphs, with the `✕` resident on the selected card and truncation that keeps a
/// colliding title's distinguishing tail.
///
/// [`pixel_band`] repaints the band in pixels over these cell tones where it draws.
/// macOS has no band ([`STRIP_IS_CHROME_BAND`]).
pub(crate) const STRIP_CHIP_CARDS: bool = STRIP_IS_CHROME_BAND;

/// Whether the strip resolves every chip's label in one pass over all tabs
/// ([`distinct_chip_labels`]) instead of truncating each title on its own.
///
/// Spends the same `title_end - title_start` budget as the per-tab
/// `truncate_title`; only which characters survive changes, so two shells in one
/// cwd do not render byte-identical labels. macOS keeps the head cut: its tabs
/// live in the native toolbar ([`STRIP_IS_CHROME_BAND`]).
pub(crate) const STRIP_DISTINCT_LABELS: bool = STRIP_IS_CHROME_BAND;

/// The legibility floor every strip ink is held to against its surface: 3:1, below
/// WCAG AA's 4.5:1 on purpose. The strip keeps the theme's own foreground wherever
/// it clears this floor, where the message band darkens ink to AA: muted schemes
/// such as Solarized Light sit between the two against the strip (fg ~4.4–5.0:1,
/// dim labels ~3.4–3.7:1), so an AA floor would repaint their text. On a bundled
/// scheme it lifts Solarized Dark's active-tab ink on a blended band's raised card
/// (~2.85:1 unfloored); it also keeps a user theme from putting illegible ink on
/// the band.
const STRIP_LEGIBILITY_FLOOR: f64 = 3.0;

/// The non-text floor of the strip's accent marks (the active underline, the
/// update rule, the status marks) against the surfaces they are drawn on: WCAG
/// 1.4.11's 3:1 for a graphical object, the floor the message band's accent takes.
const STRIP_ACCENT_FLOOR: f64 = 3.0;

/// The strip tones derived from a theme, computed once per [`paint_strip`] (hoisted
/// out of the per-cell [`strip_cell`]; the blends are frame-invariant).
#[derive(Clone, Copy)]
struct StripColors {
    /// Full-strength foreground (the `+` affordance and, by default, the
    /// active-tab text), contrast-floored against [`Self::band_bg`].
    fg: [u8; 3],
    /// The band surface under bare strip, inactive tabs and a solo title. On macOS
    /// the terminal background, so the strip recedes; elsewhere
    /// [`crate::chrome_band::band_colors`]'s `bar_bg`, shared with the find bar,
    /// config notices and settings band.
    band_bg: [u8; 3],
    /// The surface under the seam ([`StripFloor`]; the terminal's own ground,
    /// the theme background, over the grid): the band's foot
    /// ([`BandGeometry::foot_px`]) and the gutters' rail are this tone, and the
    /// selected tab opens through the seam exactly when it IS this tone.
    foot: [u8; 3],
    /// The active tab's background: on Windows the terminal's ground
    /// ([`SELECTED_TAB_IS_GROUND`]), elsewhere the raised card [`Self::raise_bg`];
    /// the user's `active_tab_color` replaces either.
    active_bg: [u8; 3],
    /// The theme-derived raise, before any `active_tab_color` override: the
    /// update CTA's card, and the selected chip off Windows.
    raise_bg: [u8; 3],
    /// Ink for [`Self::raise_bg`], floored against it: on muted schemes the raise
    /// walks the card toward its own label (Solarized Dark's fg would be 2.85:1).
    raise_fg: [u8; 3],
    /// The active tab's label ink. Equal to [`Self::raise_fg`] for the theme-derived
    /// strip; an `active_tab_color` override picks black or white by the override's
    /// luminance.
    active_fg: [u8; 3],
    /// Dimmed foreground for inactive tab labels, contrast-floored against
    /// [`Self::band_bg`].
    inactive_fg: [u8; 3],
    /// The quiet chip's card surface ([`STRIP_CHIP_CARDS`] band only): a half-step
    /// off [`Self::band_bg`], below the hover wash. Elsewhere equal to `band_bg`
    /// and never painted.
    chip_bg: [u8; 3],
    /// Ink for [`Self::chip_bg`]: the inactive dim re-floored on the chip card.
    chip_fg: [u8; 3],
    /// Full-strength ink floored against [`Self::chip_bg`]: the `+` button's ink
    /// on a chip-card band.
    chip_button_fg: [u8; 3],
    /// The hover wash under an inactive chip: a step off [`Self::band_bg`], well
    /// below the active raise. Chrome bands only.
    hover_bg: [u8; 3],
    /// Ink for [`Self::hover_bg`]: the inactive dim re-floored against the wash.
    hover_fg: [u8; 3],
    /// Full-strength ink floored against [`Self::hover_bg`] for a hovered `+`.
    button_hover_fg: [u8; 3],
    /// The selected underline's and the status marks' accent: the theme cursor
    /// floored to [`STRIP_ACCENT_FLOOR`], or the forced palette's highlight.
    accent: [u8; 3],
    /// Surface / ink / rule for the `↻` update CTA ([`StripRole::Update`]).
    ///
    /// Separate from the selection's colours: under an OS-forced palette those would
    /// make the CTA identical to the selected tab and its rule invisible (see
    /// [`forced_strip_colors`]). With a theme-derived strip these equal the selected
    /// pair plus [`Self::accent`].
    update_bg: [u8; 3],
    /// Ink for [`Self::update_bg`].
    update_fg: [u8; 3],
    /// The CTA's underline rule. Always contrasting against [`Self::update_bg`] —
    /// a rule the same colour as its own surface is not a rule.
    update_rule: [u8; 3],
    /// The hairline closing the bottom strip row against the terminal content
    /// ([`strip_seam_rail`]). `None` without a band ([`STRIP_IS_CHROME_BAND`]).
    seam: Option<[u8; 3]>,
    /// The selected Windows tab's 1-px outline, in [`Self::seam`]'s tone: on
    /// every selected tab of a light band, and on a dark band where its fill
    /// alone would not stand off the band ([`TAB_RIM_BELOW`]); `None`
    /// elsewhere ([`selected_tab_rim`]).
    active_rim: Option<[u8; 3]>,
}

/// The contrast with a DARK band under which a selected Windows tab
/// ([`SELECTED_TAB_IS_GROUND`]) draws its outline ([`selected_tab_rim`]; a
/// light band's selected tab always does). Default's grid and Settings page
/// (1.28:1) read as tabs without one. The Appearance preview's window sample
/// takes the same seam rim (`settings_preview::WINDOW_SAMPLE_RIM_BELOW`).
const TAB_RIM_BELOW: f64 = 1.15;

/// The selected tab's outline over the band ([`StripColors::active_rim`]): the
/// band's `seam`, so the tab keeps the floor's tone and still reads as a tab,
/// as WinUI's selected `TabViewItem` carries its card stroke. On a light band
/// every selected Windows tab wears it, so a tab is one shape whatever it
/// holds: drawn only within [`TAB_RIM_BELOW`] of the band, Default Light's
/// Settings tab (`#eaebeb` on its `#e2e3e5` band, 1.08:1) had the outline and
/// its terminal tab (1.20:1) none, and the selected tab changed shape with
/// what it selected (the 2026-10-18 judge's
/// `J4F-4-light-selected-tab-two-idioms`; before that the Settings tab was a
/// shape only its missing seam drew, `j3d-light-settings-tab-faint`). On a
/// dark band, where the selected tab is a darker slab the eye reads as a tab,
/// only a fill within [`TAB_RIM_BELOW`] of the band wears it. `None` off
/// Windows (a chip is a card) and without a seam.
fn selected_tab_rim(active: [u8; 3], band: [u8; 3], seam: Option<[u8; 3]>) -> Option<[u8; 3]> {
    seam.filter(|_| SELECTED_TAB_IS_GROUND && windows_tab_wears_rim(active, band))
}

/// Whether a selected Windows tab filled `active` on `band` wears the seam's
/// outline: [`selected_tab_rim`]'s rule, asked on any host. Always on a light
/// band, on a dark one only within [`TAB_RIM_BELOW`]. The Themes demo's
/// Windows window (`settings_preview::paint_demo_windows_strip`) asks it, so
/// its selected tab is the strip's own shape in every theme.
pub(crate) fn windows_tab_wears_rim(active: [u8; 3], band: [u8; 3]) -> bool {
    bg_is_light(band) || crate::chrome_band::contrast(active, band) < TAB_RIM_BELOW
}

/// The strip tones under an OS-forced chrome palette
/// ([`crate::chrome_band::ForcedChrome`], i.e. Windows High Contrast).
///
/// The band is the control face and the selected chip the OS selection pair, as in
/// every Win32 tab control under HC. Two deliberate collapses:
///
///  * **No inactive dim.** `inactive_fg == fg == COLOR_WINDOWTEXT`: HC publishes one
///    text colour. The `HIGHLIGHT` chip carries the active/inactive distinction.
///  * **`COLOR_HIGHLIGHT` is reserved for the selected tab.** The `+` and a hovered
///    chip take `COLOR_WINDOW`; on the stock HC schemes `WINDOW == BTNFACE`, so they
///    are bare glyphs on the band (HC separates surfaces by borders, not fills).
///
/// The seam is `COLOR_WINDOWTEXT`, not a blend, for the same reason; it carries the
/// band's edge on schemes whose band and grid are both black.
fn forced_strip_colors(hc: crate::chrome_band::ForcedChrome, foot: [u8; 3]) -> StripColors {
    let ink = crate::chrome_band::forced_ink;
    let band_fg = ink(hc.window_text, hc.btn_face);
    let plain_fg = ink(hc.window_text, hc.window);
    StripColors {
        fg: band_fg,
        band_bg: hc.btn_face,
        // The terminal grid keeps its own colours under HC (`chrome_band`), so
        // the band's foot is still what lies under it; the selected tab is the
        // OS selection, which never opens into it.
        foot,
        active_bg: hc.highlight,
        raise_bg: hc.window,
        raise_fg: plain_fg,
        active_fg: ink(hc.highlight_text, hc.highlight),
        inactive_fg: band_fg,
        // The quiet card collapses into the band: HC publishes no third surface
        // and blending is what HC exists to stop. The pointer's `WINDOW` wash still
        // lifts the hovered chip; the `✕` and position carry the rest.
        chip_bg: hc.btn_face,
        chip_fg: band_fg,
        chip_button_fg: band_fg,
        hover_bg: hc.window,
        hover_fg: plain_fg,
        // The hovered `+` sits on `hc.window`, so it takes the plain document text.
        button_hover_fg: plain_fg,
        accent: hc.highlight,
        // The `↻` takes the plain document surface like the `+`: HIGHLIGHT means
        // selected. It keeps its bold and gains a `WINDOWTEXT` rule (a border) so it
        // still stands out without impersonating the active tab.
        update_bg: hc.window,
        update_fg: plain_fg,
        update_rule: plain_fg,
        // Only Windows publishes a forced palette, and there the strip is already a
        // chrome band.
        seam: Some(ink(hc.window_text, hc.btn_face)),
        // The OS selection is its own outline.
        active_rim: None,
    }
}

/// Derive the strip tones from a theme.
///
/// On macOS bare strip and inactive tabs sit on the terminal background, so a
/// sparse strip recedes into the content; the active tab is a raised card (bg
/// stepped toward fg) with bold fg. Elsewhere ([`STRIP_IS_CHROME_BAND`]) the strip
/// takes a band surface and the raise is anchored on it, `mix3(band_bg, fg,
/// active_t)`, so the selected card sits the same step above the band that macOS
/// puts it above the body (there `band_bg == theme.bg`).
///
/// Each ink is floored to [`STRIP_LEGIBILITY_FLOOR`] against the surface it is drawn
/// on (band ink against `band_bg`, card ink against `raise_bg`), and the accent to
/// [`STRIP_ACCENT_FLOOR`] against both.
///
/// The raise is gentler on light themes (`bg_is_light`), where a deeper step reads as
/// a slab; the inactive-label dim is mild, since the raised card carries the focus
/// cue. `strip_inks_clear_the_legibility_floor` guards the inks and the accent.
///
/// Windows ([`SELECTED_TAB_IS_GROUND`]) draws tabs, not chips: the selected tab
/// is the terminal's own ground and opens into it, quiet tabs have no card,
/// and on a light theme the update CTA steps toward white, as Windows 11's
/// elevated surfaces do, never toward the ink: stepped toward the ink, the
/// selected card of a light theme was the darkest chip on the band and read as
/// pressed (the 2026-10-18 judges' `tab-chips-not-win11-tabs`). The hover wash
/// is the one surface that steps toward the ink on a light band
/// ([`LIGHT_TAB_HOVER_T`]), as WinUI's subtle fill does.
///
/// Under an OS-forced chrome palette (Windows High Contrast) [`forced_strip_colors`]
/// answers instead.
///
/// The strip over the terminal's own ground ([`StripFloor::Ground`]); the band
/// paints over whatever this frame puts under its seam through
/// [`strip_colors_on`].
fn strip_colors(p: impl Into<ChromePalette>) -> StripColors {
    strip_colors_on(p, StripFloor::Ground)
}

/// What lies right under the strip's seam this frame, across the window: the
/// surface the selected tab opens into, and the tone of the band's foot
/// ([`BandGeometry::foot_px`](pixel_band::BandGeometry)) and of the rail in the
/// gutters ([`strip_seam_rail`]). Windows tabs ([`SELECTED_TAB_IS_GROUND`]):
/// the selected tab is ONE surface with what it opens into, as Windows
/// Terminal's tab is with its terminal and WinUI's `TabView` tab is with its
/// content area.
///
/// The selected tab takes the floor's tone and opens through the seam into it
/// when that tone stands apart from the band ([`TAB_FLOOR_APART`]): the grid
/// and a split's pane headers (both the terminal's ground, the headers wearing
/// it on Windows, [`crate::pane_header`]) and a native page (Settings, its
/// `surface`). Where the floor IS the band (the find panel on the window's top
/// row, the message band, a pane header on the band's tone), a tab of that
/// tone would vanish into the strip: the selected tab stands closed on the seam
/// in the terminal's ground and the foot takes the floor's tone. It used to be
/// the ground over every floor, opening into a grey header band, fusing with
/// the find panel's well into one dark L, and stepping down onto a lighter
/// Settings page (the 2026-10-18 judges' `selected-tab-opens-onto-grey-band`,
/// `selected-tab-opens-into-header-band`, `settings-tab-ground-mismatch`).
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Default)]
pub(crate) enum StripFloor {
    /// The terminal's own ground (the strip palette's theme background).
    #[default]
    Ground,
    /// Any other surface, `0xRRGGBB` channels.
    Surface([u8; 3]),
}

impl StripFloor {
    /// The floor's tone over `ground`, the terminal's own.
    fn tone(self, ground: [u8; 3]) -> [u8; 3] {
        match self {
            Self::Ground => ground,
            Self::Surface(tone) => tone,
        }
    }

    /// The selected tab's surface over this floor: the floor's tone where it
    /// stands apart from the band (`band`), else the terminal's `ground`,
    /// standing closed on the seam.
    fn selected_surface(self, ground: [u8; 3], band: [u8; 3]) -> [u8; 3] {
        let tone = self.tone(ground);
        if crate::chrome_band::contrast(tone, band) >= TAB_FLOOR_APART {
            tone
        } else {
            ground
        }
    }
}

/// How far a floor ([`StripFloor`]) must stand from the band, as a contrast
/// ratio, for the selected tab to take its tone: just past the rounding of two
/// derivations of the same band (the find panel's band over a live background is
/// the strip's band to the byte or within a step), well under the subtlest page
/// a tab opens into (Default Light's Settings `#eaebeb` on its `#e2e3e5` band,
/// 1.08:1; WinUI's light selected tab is a step that small).
const TAB_FLOOR_APART: f64 = 1.03;

/// [`strip_colors`] over `floor`: the selected tab ([`StripFloor::selected_surface`])
/// and the band's foot ([`StripColors::foot`]) follow what lies under the seam.
fn strip_colors_on(p: impl Into<ChromePalette>, floor: StripFloor) -> StripColors {
    let p = p.into();
    // High Contrast: the OS palette owns chrome, returned whole. The terminal grid
    // is untouched (see `chrome_band`'s module comment).
    if let Some(hc) = crate::chrome_band::forced_chrome() {
        return forced_strip_colors(hc, floor.tone(crate::settings::u32_rgb(p.theme.bg)));
    }
    let theme = p.theme;
    // The shared chrome tokens: the accent and the seam every surface draws.
    let tokens = p.tokens();
    let rgb = |c: u32| {
        [
            ((c >> 16) & 0xff) as u8,
            ((c >> 8) & 0xff) as u8,
            (c & 0xff) as u8,
        ]
    };
    // `hover_t` is a third of the raise, like the native strip's hover pill. On a
    // chip-card band ([`STRIP_CHIP_CARDS`]) the ladder re-spaces: band (0) → quiet
    // chip (~1/3 of the raise) → hover wash (~2/3) → selected card (the raise). Light
    // themes take a slightly deeper chip step so quiet cards stay visible on muted
    // schemes (Solarized).
    let light = bg_is_light(rgb(theme.bg));
    let (active_t, inactive_t, hover_t, chip_t) = match (light, STRIP_CHIP_CARDS) {
        // Windows tabs: no quiet card; on a light band the CTA steps toward
        // white, which takes a longer step to show, and the hover wash steps
        // toward the ink ([`LIGHT_TAB_HOVER_T`]).
        (true, true) if SELECTED_TAB_IS_GROUND => (0.6, 0.15, LIGHT_TAB_HOVER_T, 0.0),
        (false, true) if SELECTED_TAB_IS_GROUND => (0.21, 0.15, 0.13, 0.0),
        (true, false) => (0.14, 0.15, 0.05, 0.0),
        (true, true) => (0.14, 0.15, 0.10, 0.055),
        (false, false) => (0.21, 0.15, 0.07, 0.0),
        (false, true) => (0.21, 0.15, 0.13, 0.06),
    };
    let fg = rgb(theme.fg);
    let ground = rgb(theme.bg);
    // The one platform decision in the colour work: which surface the strip is.
    let band_bg = if STRIP_IS_CHROME_BAND {
        crate::chrome_band::band_colors(p).bar_bg
    } else {
        rgb(theme.bg)
    };
    // What a surface rises toward: the ink, except a light Windows band's,
    // which rises toward white (an elevated surface, never a pressed one).
    let rise = if light && SELECTED_TAB_IS_GROUND {
        [0xff, 0xff, 0xff]
    } else {
        fg
    };
    // Surface blends take the raw theme fg (a surface is not ink and must not move
    // with an ink floor); only the inks below are floored.
    let raise_bg = crate::chrome_band::mix3(band_bg, rise, active_t);
    // Each ink is floored against the surface it lands on: band and raised card are
    // two surfaces, and the raise also moves the card toward its ink.
    let ink = |c: [u8; 3], on: [u8; 3]| {
        crate::chrome_band::ensure_contrast(c, on, STRIP_LEGIBILITY_FLOOR)
    };
    let raise_fg = ink(fg, raise_bg);
    // Full-strength ink on the band; also the ceiling for the dim below.
    let band_fg = ink(fg, band_bg);
    // The inactive dim, floored, then capped at the band ink. `ensure_contrast` only
    // pushes ink away from its surface, so on a muted light scheme it can lift the
    // dim back toward full strength; past the band ink, unfocused labels would
    // outshine the focused one. Inert on bundled schemes
    // (`strip_inks_clear_the_legibility_floor`); it guards re-tunes and user themes.
    let dim = ink(crate::chrome_band::mix3(fg, band_bg, inactive_t), band_bg);
    let inactive_fg = if crate::chrome_band::contrast(dim, band_bg)
        > crate::chrome_band::contrast(band_fg, band_bg)
    {
        band_fg
    } else {
        dim
    };
    // The hover wash is a surface (raw fg blend); its label is the inactive ink
    // re-floored on it. It steps toward the ink on every band, a light Windows
    // band's too: there the selected tab and the CTA rise toward white, and a
    // wash rising the same way read as a second selected tab (WinUI's light
    // `TabViewItem` hover is `SubtleFillColorSecondary`, a black overlay).
    let hover_bg = crate::chrome_band::mix3(band_bg, fg, hover_t);
    let hover_fg = ink(inactive_fg, hover_bg);
    // The `+` on the wash: full-strength ink floored against the wash.
    let button_hover_fg = ink(fg, hover_bg);
    // The quiet chip's card, same construction. `chip_t` is 0.0 off chip-card
    // platforms, making these copies of their band counterparts.
    let chip_bg = crate::chrome_band::mix3(band_bg, fg, chip_t);
    let chip_fg = ink(inactive_fg, chip_bg);
    let chip_button_fg = ink(fg, chip_bg);
    // The selected tab: the raised card, or on Windows the surface it opens
    // into ([`StripFloor::selected_surface`]: the terminal's ground, a native
    // page's surface) with the terminal's own ink floored on it.
    let foot = floor.tone(ground);
    let (active_bg, active_fg) = if SELECTED_TAB_IS_GROUND {
        let selected = floor.selected_surface(ground, band_bg);
        (selected, ink(fg, selected))
    } else {
        (raise_bg, raise_fg)
    };
    // The accent marks: the active underline and a selected tab's status marks
    // sit on the selected surface, the update rule on the `↻` card (the
    // raise), and the status marks on the band, so the token accent is floored
    // against the selected surface and the band ([`floor_on_both`]), and the
    // update rule on its card.
    let accent = floor_on_both(
        tokens.accent_on(active_bg, STRIP_ACCENT_FLOOR),
        active_bg,
        band_bg,
        STRIP_ACCENT_FLOOR,
    );
    // A Windows tab row's hover wash steps the other way from the band than
    // its selected tab does, so it is not between two floored surfaces: the
    // marks a hovered tab carries are held on it too.
    let accent = if SELECTED_TAB_IS_GROUND {
        floor_on_all(accent, &[active_bg, band_bg, hover_bg], STRIP_ACCENT_FLOOR)
    } else {
        accent
    };
    let update_rule = if SELECTED_TAB_IS_GROUND {
        floor_on_both(accent, raise_bg, band_bg, STRIP_ACCENT_FLOOR)
    } else {
        accent
    };
    // The band's seam: the authored border, else the band stepped toward the
    // ink (`aterm_messages::tokens::SEAM_T`), held to the seam floor.
    let seam = STRIP_IS_CHROME_BAND.then(|| tokens.seam_on(band_bg));
    StripColors {
        fg: band_fg,
        band_bg,
        foot,
        active_bg,
        raise_bg,
        raise_fg,
        active_fg,
        inactive_fg,
        chip_bg,
        chip_fg,
        chip_button_fg,
        hover_bg,
        hover_fg,
        button_hover_fg,
        accent,
        // The raised pair plus the theme accent as its rule; only the
        // forced-palette branch differs.
        update_bg: raise_bg,
        update_fg: raise_fg,
        update_rule,
        seam,
        active_rim: selected_tab_rim(active_bg, band_bg, seam),
    }
}

/// [`floor_on_both`]'s last rule over any number of grounds: `mark` unchanged
/// when it clears `floor` on every one, else its smallest step toward black or
/// white that does; where none does, the step that came closest.
fn floor_on_all(mark: [u8; 3], grounds: &[[u8; 3]], floor: f64) -> [u8; 3] {
    use crate::chrome_band::{contrast, mix3};
    let worst = |c: [u8; 3]| {
        grounds
            .iter()
            .map(|g| contrast(c, *g))
            .fold(f64::INFINITY, f64::min)
    };
    if worst(mark) >= floor {
        return mark;
    }
    // The smallest step toward black or white that clears every ground; where
    // none does, the step that came closest.
    let (mut best, mut best_ratio) = (mark, worst(mark));
    for step in 1..=10u8 {
        for pole in [[0, 0, 0], [0xff, 0xff, 0xff]] {
            let stepped = mix3(mark, pole, f32::from(step) / 10.0);
            let ratio = worst(stepped);
            if ratio >= floor {
                return stepped;
            }
            if ratio > best_ratio {
                (best, best_ratio) = (stepped, ratio);
            }
        }
    }
    best
}

/// `mark` held to `floor` on the card AND the band it may sit on: floored on
/// the card, then on the band. The card is a step from the band toward the
/// ink, so the band's floor is a no-op unless the card's left the mark on the
/// band's side; there it walks the mark toward a pole THROUGH the card's tone,
/// and can stop on it (an authored accent at the terminal background on a
/// chrome band, 1.76:1 on the card). Then the mark takes the smallest step
/// toward black or white that clears both grounds; where none does, the step
/// that came closest.
fn floor_on_both(mark: [u8; 3], card: [u8; 3], band: [u8; 3], floor: f64) -> [u8; 3] {
    use crate::chrome_band::{contrast, ensure_contrast, mix3};
    let worst = |c: [u8; 3]| contrast(c, card).min(contrast(c, band));
    let floored = ensure_contrast(mark, band, floor);
    if worst(floored) >= floor {
        return floored;
    }
    let (mut best, mut best_ratio) = (floored, worst(floored));
    for step in 1..=10u8 {
        for pole in [[0, 0, 0], [0xff, 0xff, 0xff]] {
            let stepped = mix3(mark, pole, f32::from(step) / 10.0);
            let ratio = worst(stepped);
            if ratio >= floor {
                return stepped;
            }
            if ratio > best_ratio {
                (best, best_ratio) = (stepped, ratio);
            }
        }
    }
    best
}

/// [`strip_colors`] test shorthand: `theme` with `active_override` as its authored
/// `active_tab_color` ([`strip_colors_for`]).
#[cfg(test)]
fn strip_colors_with_active(theme: Theme, active_override: Option<[u8; 3]>) -> StripColors {
    strip_colors_for(ChromePalette {
        authored: aterm_messages::tokens::AuthoredChrome {
            active_tab: active_override,
            ..aterm_messages::tokens::AuthoredChrome::NONE
        },
        ..ChromePalette::from(theme)
    })
}

/// [`strip_colors`] with the authored `active_tab_color` (config over theme): the
/// active tab's background becomes that colour and its label ink flips black/white
/// by the colour's luminance ([`bg_is_light`], the classifier the native toolbar
/// pill uses). None authored is the theme-derived strip.
fn strip_colors_for(p: impl Into<ChromePalette>) -> StripColors {
    strip_colors_for_on(p, StripFloor::Ground)
}

/// [`strip_colors_for`] over `floor` ([`strip_colors_on`]): what the live strip
/// paints. An authored `active_tab_color` stands on the seam over any floor.
fn strip_colors_for_on(p: impl Into<ChromePalette>, floor: StripFloor) -> StripColors {
    let p = p.into();
    let active_override = p.authored.active_tab;
    let mut colors = strip_colors_on(p, floor);
    // Under an OS-forced palette (High Contrast) the override is dropped: HC is an
    // accessibility contract that the OS picks chrome colour. The config value is
    // kept and applies again once HC is off.
    if crate::chrome_band::forced_chrome().is_some() {
        return colors;
    }
    if let Some(active_bg) = active_override {
        let active_fg = if bg_is_light(active_bg) {
            [0, 0, 0]
        } else {
            [255, 255, 255]
        };
        colors.active_bg = active_bg;
        colors.active_fg = active_fg;
        colors.active_rim = selected_tab_rim(active_bg, colors.band_bg, colors.seam);
        // The `↻` CTA also follows `active_tab_color`.
        colors.update_bg = active_bg;
        colors.update_fg = active_fg;
    }
    colors
}

/// How strongly macOS's native toolbar paints the selected tab's pill:
/// `selectedContentBackgroundColor` at this alpha over the strip band, while
/// no `active_tab_color` replaces it. The toolbar draws with it and
/// [`automatic_active_tab_color`] composites with it, so the colour Settings
/// names is the one on screen.
pub(crate) const NATIVE_PILL_AUTO_ALPHA: f32 = 0.42;

/// THE AUTOMATIC SELECTED-TAB COLOUR: what the selected tab is painted with
/// while the `active_tab_color` key is unset, as one value Settings can show
/// (the row's chip, the wheel's marker) rather than something only the paint
/// paths know. The same derivation the strip runs ([`strip_colors_for_on`]):
///
/// * `header` is the strip's palette with the THEME's own authored chrome,
///   no `active_tab_color` key over it, so a theme that authors the selected
///   tab's colour names that colour;
/// * `floor` is what lies under the strip's seam ([`StripFloor`]: on Windows
///   the selected tab opens into it);
/// * `native_selection` is the system's selected-content colour where macOS's
///   native toolbar draws the pill (resolved in the strip's own appearance),
///   `None` where the strip is drawn in the grid or as the pixel band.
///
/// Under an OS-forced palette (High Contrast) it is the highlight the strip
/// paints. An authored colour is what the strip and the pill paint. On macOS's
/// toolbar it is otherwise the pill composited over the band, elsewhere the
/// strip's selected tab.
pub(crate) fn automatic_active_tab_color(
    header: ChromePalette,
    floor: StripFloor,
    native_selection: Option<[u8; 3]>,
) -> [u8; 3] {
    let colors = strip_colors_for_on(header, floor);
    if crate::chrome_band::forced_chrome().is_some() || header.authored.active_tab.is_some() {
        return colors.active_bg;
    }
    match native_selection {
        Some(selection) => {
            crate::chrome_band::mix3(colors.band_bg, selection, NATIVE_PILL_AUTO_ALPHA)
        }
        None => colors.active_bg,
    }
}

/// The strip's tones as a picture of it paints them (the Themes page's demo
/// window): the same derivation the live strip runs, so a theme's
/// `active_tab_color`, `accent_color` and `border_color` show there exactly as
/// they will on its tabs, and a theme that leaves them unset shows the
/// derived ones.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct StripSample {
    /// The band under the tabs.
    pub(crate) band: [u8; 3],
    /// The selected tab's fill.
    pub(crate) active: [u8; 3],
    /// The selected tab's label.
    pub(crate) active_ink: [u8; 3],
    /// An unselected tab's label.
    pub(crate) inactive_ink: [u8; 3],
    /// The accent rule under the selected tab.
    pub(crate) rule: [u8; 3],
    /// The seam between the tabs and under the band.
    pub(crate) seam: [u8; 3],
}

/// [`StripSample`] for one palette.
pub(crate) fn strip_sample(p: impl Into<ChromePalette>) -> StripSample {
    let p = p.into();
    let colors = strip_colors_for(p);
    StripSample {
        band: colors.band_bg,
        active: colors.active_bg,
        active_ink: colors.active_fg,
        inactive_ink: colors.inactive_fg,
        rule: colors.accent,
        seam: p.tokens().seam_on(colors.band_bg),
    }
}

/// Whether this background is light: black text out-contrasts white on it (the
/// WCAG pole). Every bundled scheme sits well clear of it. The one dark/light
/// classifier for all chrome (this strip and the native toolbar via
/// [`theme_is_dark`]).
///
/// Delegates to the engine's WCAG-pole classifier,
/// [`aterm_messages::ink::bg_is_light`].
pub(crate) fn bg_is_light(bg: [u8; 3]) -> bool {
    aterm_messages::ink::bg_is_light(bg)
}

/// [`bg_is_light`] inverted for a packed-RGB theme colour: `true` when the theme
/// background is dark. Pins the native toolbar strip's appearance
/// ([`crate::toolbar::set_strip_dark`]) to the theme.
pub(crate) fn theme_is_dark(bg: u32) -> bool {
    !bg_is_light([
        ((bg >> 16) & 0xff) as u8,
        ((bg >> 8) & 0xff) as u8,
        (bg & 0xff) as u8,
    ])
}

/// The band tone and seam hairline, packed `0x00RRGGBB`, for the padding gutters
/// the grid columns cannot reach ([`aterm_render::ChromeBleed`]), so the band and
/// its seam reach the window edges. `None` on macOS, where the strip has no
/// surface of its own ([`STRIP_IS_CHROME_BAND`]) and the padding already matches.
#[must_use]
pub(crate) fn strip_bleed_tones(p: impl Into<ChromePalette>) -> Option<(u32, Option<u32>)> {
    STRIP_IS_CHROME_BAND.then(|| {
        let pack = |c: [u8; 3]| (u32::from(c[0]) << 16) | (u32::from(c[1]) << 8) | u32::from(c[2]);
        let colors = strip_colors(p);
        (pack(colors.band_bg), colors.seam.map(pack))
    })
}

/// The band's hover wash for `theme` ([`StripColors::hover_bg`]), exposed for the
/// band-hover pointer tests in `app_mouse`; `StripColors` itself stays private.
// Gated like its one caller, the band-hover test in `app_mouse.rs`, so macOS has
// no dead-code warning.
#[cfg(all(test, any(windows, target_os = "linux")))]
pub(crate) fn strip_hover_bg_for_test(theme: Theme) -> [u8; 3] {
    strip_colors(theme).hover_bg
}

/// A bare strip-background [`RenderCell`], to pre-fill strip rows before
/// [`paint_strip`]. Recomputes the tones; not for the hot per-cell loop.
#[must_use]
pub(crate) fn blank_cell(p: impl Into<ChromePalette>) -> RenderCell {
    strip_cell(' ', &strip_colors(p), StripRole::Inactive)
}

/// The chrome label's logical size: the settings tray's chrome base (13 px at
/// 96 dpi ≈ 10 pt), not the terminal `font_px`. The strip's titles are set at
/// it ([`chrome_label_base_px`]) and the pane headers a type step below.
pub(crate) const CHROME_LABEL_LOGICAL_PX: f32 = 13.0;

/// The chrome label base for a `row_h`-px row at the window's `scale`:
/// [`CHROME_LABEL_LOGICAL_PX`] scaled and rounded, capped at 62% of the row,
/// floored at a legible 6 px. The strip band's title size, and the base the
/// pane headers' smaller step is taken from, so the two rows share one scale.
#[must_use]
pub(crate) fn chrome_label_base_px(row_h: f32, scale: f32) -> f32 {
    let scale = if scale.is_finite() && scale > 0.0 {
        scale
    } else {
        1.0
    };
    (CHROME_LABEL_LOGICAL_PX * scale)
        .round()
        .min((row_h * 0.62).floor())
        .max(6.0)
}

/// The band's foot in logical px (Windows): the terminal's ground under the
/// seam, before the grid's first row. Windows Terminal keeps 8 px of padding
/// between its tab row and the text; the first row here sat flush under the
/// rule, its ink 4 px below it and a prompt's coloured segment touching it,
/// against 12 px of padding on the other three sides (the 2026-10-18 judges'
/// `grid-flush-under-strip-seam`, `top-pad-zero-under-strip`). The synthetic
/// head reserves it (`App::synthetic_strip_head_px`), so the visible band
/// keeps its height and the grid starts this much lower.
pub(crate) const STRIP_FOOT_LOGICAL_PX: f32 = if STRIP_WINDOWS_PAINT { 6.0 } else { 0.0 };

/// The band's foot in device px for a strip canvas `canvas_px` tall (the top
/// pad, the head and the strip's rows) whose visible band should measure
/// `target_px` (the `tab_band_height` target at this scale; 0 for a compact
/// band): what the canvas holds past the target, up to
/// [`STRIP_FOOT_LOGICAL_PX`] at `scale`. A compact band, and a canvas with no
/// head to spare (a headless window keeps its sealed boot band), have none.
#[must_use]
pub(crate) fn strip_foot_px(canvas_px: usize, target_px: usize, scale: f32) -> usize {
    if target_px == 0 || STRIP_FOOT_LOGICAL_PX <= 0.0 {
        return 0;
    }
    let scale = if scale.is_finite() && scale > 0.0 {
        scale
    } else {
        1.0
    };
    let most = (STRIP_FOOT_LOGICAL_PX * scale).round().max(0.0) as usize;
    canvas_px.saturating_sub(target_px).min(most)
}

impl crate::App {
    /// Window `wid`'s band foot ([`strip_foot_px`]) under a `strip`-row strip:
    /// out of the head the synthetic band reserved for it
    /// (`App::synthetic_strip_head_px`); 0 on a compact band and on a headless
    /// window's sealed boot band.
    pub(crate) fn strip_foot_px(&self, wid: crate::WindowId, strip: usize) -> usize {
        let cell_h = self.win_cell_size(wid).1;
        let scale = self.windows.get(&wid).map_or(1.0, |ws| ws.scale);
        let canvas = self.strip_band_top_px(wid) + strip * cell_h;
        let target = crate::logical_to_device_px(
            self.config.tab_band_height_or_default().target_logical_px(),
            scale,
        );
        strip_foot_px(canvas, target, scale as f32)
    }
}

/// The strip seam's thickness in device px at `scale`: one logical px, never
/// under one device px (1 at 100% and 150%, 2 at 200%).
#[must_use]
pub(crate) fn strip_seam_px(scale: f32) -> usize {
    if scale.is_finite() && scale >= 1.0 {
        scale.floor() as usize
    } else {
        1
    }
}

/// Close the bottom of the strip's last row against the terminal content with a
/// hairline seam, in place: Linux's seam. A no-op without a band
/// ([`STRIP_IS_CHROME_BAND`]) and where the seam is a rail instead
/// ([`STRIP_WINDOWS_PAINT`], [`strip_seam_rail`]).
///
/// The seam is an underline on the strip's own last row, not an overline on the
/// first content row, which belongs to the session. On a chip-card band the
/// selection carries no rule, so this is one unbroken hairline.
///
/// Called once per strip rebuild from the row splice; recomputes the tones like
/// [`blank_cell`].
pub(crate) fn seal_strip_bottom(row: &mut [RenderCell], p: impl Into<ChromePalette>) {
    if STRIP_WINDOWS_PAINT {
        return;
    }
    let Some(seam) = strip_colors(p).seam else {
        return;
    };
    for cell in row.iter_mut() {
        // Never overwrite an existing rule: the selected chip's accent underline is
        // its seam.
        if cell.underline == UnderlineStyle::None {
            cell.underline = UnderlineStyle::Single;
            cell.underline_color = Some(seam);
        }
    }
}

/// The hairline closing the band against the terminal content on Windows
/// ([`STRIP_WINDOWS_PAINT`]): a rail on the LAST `seam_px` pixel rows of the
/// strip's last row (`row`), `frame_w` px wide, window edge to window edge, so
/// it meets the grid and covers the gutters
/// ([`aterm_render::chrome_rail_covers_gutters`] then retires the bleed's own
/// gutter seam for that row). `None` without a band ([`STRIP_IS_CHROME_BAND`]),
/// and on Linux, whose seam is a cell underline ([`seal_strip_bottom`]).
///
/// A rail, not a cell underline: an underline lands at the face's underline
/// offset (row 19 of a 23 px Iosevka cell), which left a 4 px slab of band under
/// the rule and the cards centred on it, high in the band (the 2026-10-17
/// audit's `strip-seam-floats-above-band-edge`). The message band rails its
/// seams the same way (`aterm_render::band`). The pixel band draws the same rule
/// on its canvas's last rows ([`pixel_band::raster_band`]), since its opaque
/// image covers the rail in the grid columns.
///
/// With a FOOT (`foot_px`, [`strip_foot_px`]) the canvas draws the rule that
/// far above its last row and the floor's tone below it ([`StripFloor`]: the
/// terminal's ground over the grid); the rail is then that tone, `foot_px` rows
/// tall, so the gutters' band ends level with the rule's foot and the first row
/// under the strip keeps the same air at the window's edges. The rule itself
/// spans the grid's columns, inset like the text.
///
/// The rail is cached per `(frame_w, tone)` so a steady frame allocates nothing.
#[must_use]
pub(crate) fn strip_seam_rail(
    row: u16,
    frame_w: usize,
    seam_px: usize,
    foot_px: usize,
    p: impl Into<ChromePalette>,
    floor: StripFloor,
) -> Option<aterm_core::render::ChromeRaster> {
    if !STRIP_WINDOWS_PAINT {
        return None;
    }
    let colors = strip_colors_on(p, floor);
    let seam = colors.seam?;
    let (tone, rail_h) = if foot_px > 0 {
        (colors.foot, foot_px)
    } else {
        (seam, seam_px.max(1))
    };
    let seam = (u32::from(tone[0]) << 16) | (u32::from(tone[1]) << 8) | u32::from(tone[2]);
    // The last rail laid: its width, its colour and its pixels.
    type Rail = Option<(usize, u32, Arc<[u32]>)>;
    thread_local! {
        static RAIL: RefCell<Rail> = const { RefCell::new(None) };
    }
    let rail = RAIL.with(|slot| {
        let mut slot = slot.borrow_mut();
        match slot.as_ref() {
            Some((w, c, rail)) if *w == frame_w && *c == seam => Arc::clone(rail),
            _ => {
                let rail: Arc<[u32]> = std::iter::repeat_n(seam, frame_w).collect();
                *slot = Some((frame_w, seam, Arc::clone(&rail)));
                rail
            }
        }
    });
    Some(aterm_core::render::ChromeRaster {
        row,
        ground: Arc::from(Vec::new()),
        rail,
        rail_h: u16::try_from(rail_h).unwrap_or(1),
        clear_rail: false,
        own: Vec::new(),
        split: None,
        rings: Vec::new(),
        icons: Vec::new(),
    })
}

/// Build the [`RenderCell`] for a strip cell from precomputed [`StripColors`] and the
/// cell's [`StripRole`].
fn strip_cell(ch: char, colors: &StripColors, role: StripRole) -> RenderCell {
    // The active tab is a raised bg with a full-width accent underline, which also
    // seams it against the content below. Inactive tabs and the `+` are flat labels.
    let (fg, bg, bold, underline, underline_color) = match role {
        // On a chip-card band the filled card alone marks the selection: a cell
        // underline would graze descenders, and the seam ([`strip_seam_rail`],
        // [`seal_strip_bottom`] on Linux) closes the band unbroken.
        StripRole::Active if STRIP_CHIP_CARDS => (
            colors.active_fg,
            colors.active_bg,
            true,
            UnderlineStyle::None,
            None,
        ),
        StripRole::Active => (
            colors.active_fg,
            colors.active_bg,
            true,
            UnderlineStyle::Single,
            Some(colors.accent),
        ),
        StripRole::Inactive => (
            colors.inactive_fg,
            colors.band_bg,
            false,
            UnderlineStyle::None,
            None,
        ),
        // The quiet chip: the inactive vocabulary on its own card surface.
        StripRole::Chip => (
            colors.chip_fg,
            colors.chip_bg,
            false,
            UnderlineStyle::None,
            None,
        ),
        // Hover keeps the inactive vocabulary (no bold, no rule) on the faint wash.
        StripRole::Hover => (
            colors.hover_fg,
            colors.hover_bg,
            false,
            UnderlineStyle::None,
            None,
        ),
        // The divider is band furniture: seam ink on the band tone. The
        // `unwrap_or` is unreachable (Separators are painted only on a band, which
        // always has a seam) but keeps this total.
        StripRole::Separator => (
            colors.seam.unwrap_or(colors.inactive_fg),
            colors.band_bg,
            false,
            UnderlineStyle::None,
            None,
        ),
        // On a chip-card band the `+` is a quiet chip with a bright glyph; a raised
        // card would match the selected tab and read as another tab.
        StripRole::NewTab if STRIP_CHIP_CARDS => (
            colors.chip_button_fg,
            colors.chip_bg,
            false,
            UnderlineStyle::None,
            None,
        ),
        // On a chrome band the `+` is a raised button like the `↻`, minus the
        // underline (which means selected). It uses `raise_bg`, not `active_bg`, so
        // `active_tab_color` recolours only the selected tab. On macOS's flat strip
        // it stays a bare glyph on the body.
        StripRole::NewTab if STRIP_IS_CHROME_BAND => (
            colors.raise_fg,
            colors.raise_bg,
            false,
            UnderlineStyle::None,
            None,
        ),
        StripRole::NewTab => (colors.fg, colors.band_bg, false, UnderlineStyle::None, None),
        // The hovered `+`: the band's hover wash under full-strength ink. On the
        // flat strip (macOS) this wash is the button's only surface.
        StripRole::NewTabHover => (
            colors.button_hover_fg,
            colors.hover_bg,
            false,
            UnderlineStyle::None,
            None,
        ),
        StripRole::Title => (colors.fg, colors.band_bg, true, UnderlineStyle::None, None),
        // Chip-card band: the update alert keeps its highlighted card without the
        // accent underline.
        StripRole::Update if STRIP_CHIP_CARDS => (
            colors.active_fg,
            colors.active_bg,
            true,
            UnderlineStyle::None,
            None,
        ),
        // A raised, underlined, bold button so the update alert draws the eye. Its
        // own triple ([`StripColors::update_bg`]): under an OS palette the selection
        // pair would make it a copy of the selected tab with an invisible rule.
        StripRole::Update => (
            colors.update_fg,
            colors.update_bg,
            true,
            UnderlineStyle::Single,
            Some(colors.update_rule),
        ),
    };
    RenderCell {
        ch,
        fg,
        bg,
        wide: false,
        emoji_presentation: false,
        text_presentation: false,
        bold,
        italic: false,
        underline,
        strikethrough: false,
        overline: false,
        underline_color,
        overline_color: None,
        script: aterm_core::terminal::Script::Normal,
    }
}

/// Sanitize one title char for the strip: control chars are blanked; everything
/// else (emoji, CJK, Nerd Font icons) passes through. Column math runs in display
/// cells ([`strip_char_cells`]).
fn strip_char(c: char) -> char {
    if c.is_control() { ' ' } else { c }
}

/// Display cells one projected title char occupies on the strip: 1 or 2, from the
/// grid's width tables (`aterm_grapheme::char_width`). Zero-width chars are clamped
/// to one cell, which keeps the rename caret's char-index ↔ column mapping exact.
///
/// DEC ambiguous-wide mode is not honoured: titles are chrome with no session
/// mode to read.
fn strip_char_cells(c: char) -> u16 {
    aterm_grapheme::char_width(c).max(1) as u16
}

/// Display cells a whole title occupies after [`strip_char`] projection.
fn strip_display_cells(s: &str) -> usize {
    s.chars()
        .map(|c| usize::from(strip_char_cells(strip_char(c))))
        .sum()
}

/// Whether the chip at `index` draws a leading `│` divider: never before the first
/// chip, and never beside the active or hovered chip, which draw their own edge.
/// Mirrors the native strip's `TabGeometry::separates` (`toolbar.rs`), plus the
/// hover term the native strip handles per view.
fn strip_separates(index: usize, active: usize, hovered: Option<usize>) -> bool {
    let beside = |edge: usize| index == edge || index == edge + 1;
    index > 0 && !beside(active) && !hovered.is_some_and(beside)
}

/// Paint the laid-out `segments` into `row` (one strip row, `cols` wide, pre-filled
/// with the chrome background). `titles[i]` is tab `i`'s label; `active` is the
/// highlighted tab. Tabs draw ` <title> ✕ ` (truncated with `…`), the `+` draws
/// ` + `. Bounds-checked against `row`'s length.
#[cfg(test)]
pub(crate) fn paint_strip(
    row: &mut [RenderCell],
    segments: &[TabSegment],
    titles: &[String],
    hovered: Option<usize>,
    active: usize,
    theme: Theme,
) {
    let paint = StripPaint {
        hovered,
        new_tab_hovered: false,
        version_hovered: false,
        caption_hovered: None,
        subtitle: None,
        rename: None,
    };
    let _ = paint_strip_impl(
        row,
        segments,
        titles,
        None,
        paint,
        active,
        theme.into(),
        StripFloor::Ground,
    );
}

/// Per-frame paint inputs that are neither geometry nor colour: the hovered tab and
/// the solo band's description.
#[derive(Clone, Copy, Default)]
pub(crate) struct StripPaint<'a> {
    /// Index of the tab under the pointer, if any. The `✕` is hover-only on quiet
    /// tabs (resident on the selected card of a chip-card band); its column is
    /// reserved by [`layout_segments`] either way, so revealing it never reflows a
    /// title. On a chrome band the hovered chip also takes the
    /// [`StripRole::Hover`] wash (paint only).
    pub hovered: Option<usize>,
    /// Whether the pointer is on the trailing `+`. Fed from the same per-motion hit
    /// test as [`Self::hovered`] and part of the strip cache key, so the wash
    /// follows the pointer and motion within the button costs nothing.
    pub new_tab_hovered: bool,
    /// Whether the pointer is on the trailing version label ([`TabHit::Version`]).
    /// Its own flag, like [`Self::new_tab_hovered`], because the label is not a tab;
    /// fed from the same hit test and cache key, so at most one of a chip, the `+`
    /// and the label is lit.
    pub version_hovered: bool,
    /// The window button under the pointer ([`TabHit::Caption`]), which takes the
    /// band's hover material. Same per-motion hit test and cache key as the `+`.
    pub caption_hovered: Option<CaptionButton>,
    /// The lone tab's one-line description ([`solo_subtitle`]), drawn dim after
    /// the centred title. `None` = nothing the title does not already say.
    pub subtitle: Option<&'a str>,
    /// The live inline session-rename field, when this window has one and the
    /// in-grid strip presents it. `None` = paint labels as usual.
    pub rename: Option<StripRenameField<'a>>,
}

/// The in-grid inline rename editor as the strip painter needs it: which tab it
/// sits over, and the field's text and caret. Positional: the strip has no session
/// ids, so `App` resolves the edited tab's current index per paint. Ownership
/// lives in [`crate::app_rename::TabRenameEdit`].
#[derive(Clone, Copy)]
pub(crate) struct StripRenameField<'a> {
    /// Index of the edited tab in `titles`/`segments`.
    pub tab: usize,
    /// The field's text.
    pub text: &'a str,
    /// Caret position in `text`, as a byte offset on a char boundary.
    pub cursor: usize,
}

/// Paint the strip from presentation metadata and return inline-image placements
/// for its code-native icon/status marks. `RawRgba8` images are consumed
/// identically by CPU, GPU, cached frames and `image`; the cells beneath stay
/// ordinary strip cells, so backgrounds and hit geometry are unchanged.
///
/// Test entry point; the splice calls [`paint_strip_with_metadata_and_rename_caret`].
#[cfg(test)]
#[allow(
    clippy::too_many_arguments,
    reason = "the strip's paint inputs: target row, geometry, text, metadata, per-frame paint state, selection, theme, and the user's active-tab override — all genuinely independent"
)]
pub(crate) fn paint_strip_with_metadata(
    row: &mut [RenderCell],
    segments: &[TabSegment],
    titles: &[String],
    metadata: &[TabStripMetadata],
    paint: StripPaint<'_>,
    active: usize,
    theme: Theme,
    active_override: Option<[u8; 3]>,
) -> Vec<(usize, ImageRef)> {
    paint_strip_impl(
        row,
        segments,
        titles,
        Some(metadata),
        paint,
        active,
        ChromePalette {
            authored: aterm_messages::tokens::AuthoredChrome {
                active_tab: active_override,
                ..aterm_messages::tokens::AuthoredChrome::NONE
            },
            ..ChromePalette::from(theme)
        },
        StripFloor::Ground,
    )
    .0
}

/// [`paint_strip_with_metadata`] plus the rename well's painted caret column
/// (`None` when no rename field was drawn). The splice anchors the OS IME
/// candidate window there while the rename field owns a composition
/// ([`crate::app_input::PreeditOwner::Rename`]). The selected tab's cells take
/// the surface it opens into over `floor` ([`StripFloor`]), as the band does.
#[allow(
    clippy::too_many_arguments,
    reason = "see `paint_strip_with_metadata`; identical inputs, plus what lies under the seam"
)]
pub(crate) fn paint_strip_with_metadata_and_rename_caret(
    row: &mut [RenderCell],
    segments: &[TabSegment],
    titles: &[String],
    metadata: &[TabStripMetadata],
    paint: StripPaint<'_>,
    active: usize,
    palette: ChromePalette,
    floor: StripFloor,
) -> (Vec<(usize, ImageRef)>, Option<usize>) {
    paint_strip_impl(
        row,
        segments,
        titles,
        Some(metadata),
        paint,
        active,
        palette,
        floor,
    )
}

/// The rename composition's frame cell: its painted caret when one fits,
/// otherwise the start of the strip's last row. A missing strip has no anchor.
/// The caller decides whether the rename field owns the composition.
#[must_use]
pub(crate) fn rename_ime_cursor_cell(
    strip_rows: usize,
    painted_col: Option<usize>,
) -> Option<(usize, usize)> {
    strip_rows
        .checked_sub(1)
        .map(|row| (row, painted_col.unwrap_or(0)))
}

#[allow(
    clippy::too_many_arguments,
    reason = "see `paint_strip_with_metadata`; this is its one implementation"
)]
fn paint_strip_impl(
    row: &mut [RenderCell],
    segments: &[TabSegment],
    titles: &[String],
    metadata: Option<&[TabStripMetadata]>,
    paint: StripPaint<'_>,
    active: usize,
    palette: ChromePalette,
    floor: StripFloor,
) -> (Vec<(usize, ImageRef)>, Option<usize>) {
    // Derive the strip tones once (frame-invariant), not per cell.
    let colors = strip_colors_for_on(palette, floor);
    let mut images = Vec::new();
    // The rename well's caret column, when a rename field paints this frame —
    // at most one segment carries the field, so last-write-wins is exact.
    let mut rename_caret = None;
    // Background fill: every strip cell is body-coloured chrome unless a segment
    // overwrites it, so gaps between segments read as strip, not terminal.
    for cell in row.iter_mut() {
        *cell = strip_cell(' ', &colors, StripRole::Inactive);
    }
    // Resolve every chip's label in one pass before painting
    // ([`STRIP_DISTINCT_LABELS`], [`distinct_chip_labels`]) so truncation keeps what
    // tells tabs apart. Tabs the pass skipped (solo, renaming, no band) compute
    // their label per tab below.
    let mut labels = if STRIP_DISTINCT_LABELS {
        distinct_chip_labels(
            segments,
            titles,
            metadata,
            active,
            paint.rename.map(|edit| edit.tab),
        )
    } else {
        Vec::new()
    };
    for seg in segments {
        let is_active = matches!(seg.kind, TabHit::Select(i) if i == active);
        // The hovered tab takes the hover wash on a chrome band only; on macOS's
        // flat strip hover only reveals the `✕`.
        let is_hovered = STRIP_IS_CHROME_BAND
            && !is_active
            && !seg.solo
            && matches!(seg.kind, TabHit::Select(i) if paint.hovered == Some(i));
        let tab_role = if is_active {
            StripRole::Active
        } else if is_hovered {
            StripRole::Hover
        } else if STRIP_CHIP_CARDS && !seg.solo && matches!(seg.kind, TabHit::Select(_)) {
            // A quiet tab on a chip-card band is a card, not bare band.
            StripRole::Chip
        } else {
            StripRole::Inactive
        };
        let put = |row: &mut [RenderCell], col: u16, ch: char, role: StripRole| {
            if let Some(slot) = row.get_mut(col as usize) {
                *slot = strip_cell(ch, &colors, role);
            }
        };
        // The right half of a width-2 glyph: a continuation cell in the same tones;
        // the lead cell's raster overflows into it (`RenderCell::wide`).
        let put_wide_tail = |row: &mut [RenderCell], col: u16, role: StripRole| {
            if let Some(slot) = row.get_mut(col as usize) {
                let mut cell = strip_cell(' ', &colors, role);
                cell.wide = true;
                *slot = cell;
            }
        };
        // Paint `text` from `col` up to `end` in display cells, emitting lead +
        // continuation for width-2 chars; returns one past the last painted
        // column. A wide char that would straddle `end` is dropped whole.
        let put_text = |row: &mut [RenderCell], col: u16, end: u16, text: &str, role: StripRole| {
            let mut col = col;
            for ch in text.chars() {
                let ch = strip_char(ch);
                let w = strip_char_cells(ch);
                if col.saturating_add(w) > end {
                    break;
                }
                put(row, col, ch, role);
                if w == 2 {
                    put_wide_tail(row, col + 1, role);
                }
                col += w;
            }
            col
        };
        match seg.kind {
            // Solo: the window's only tab is its title: flat body background (no
            // chip, underline or `✕`), title centred, description trailing dim.
            TabHit::Select(i) if seg.solo => {
                let span = seg.end_col.saturating_sub(seg.start_col);
                let band_role = StripRole::Inactive;
                let title_role = StripRole::Title;
                for c in seg.start_col..seg.end_col {
                    put(row, c, ' ', band_role);
                }
                let title = titles.get(i).map(String::as_str).unwrap_or("");
                // Editing the lone tab: the band becomes one left-aligned inset
                // field. A centred field would recentre (and move the caret) on
                // every keystroke, so title, description, icon and marks are dropped
                // while editing.
                if let Some(edit) = paint.rename.filter(|edit| edit.tab == i) {
                    let start = usize::from(seg.start_col) + SOLO_EDGE_COLS;
                    let end = usize::from(seg.end_col).saturating_sub(SOLO_EDGE_COLS);
                    if start < end {
                        rename_caret = paint_rename_field(
                            row,
                            start..end,
                            edit.text,
                            edit.cursor,
                            title,
                            palette,
                            None,
                        );
                    }
                    continue;
                }
                let (title, subtitle) = solo_band_text(title, paint.subtitle, span as usize);
                // Centred by display width, so wide chars do not shift the group.
                let width = strip_display_cells(&title)
                    + subtitle
                        .as_ref()
                        .map_or(0, |s| SOLO_GAP_COLS + strip_display_cells(s));
                let start = seg.start_col + (span.saturating_sub(width as u16)) / 2;
                let mut col = put_text(row, start, seg.end_col, &title, title_role);
                if let Some(subtitle) = subtitle {
                    col = col.saturating_add(SOLO_GAP_COLS as u16);
                    put_text(row, col, seg.end_col, &subtitle, band_role);
                }
                // The lone tab keeps its icon and state marks in the segment's
                // leading and trailing cells, outside the centred group.
                let item = metadata.and_then(|items| items.get(i)).copied();
                if let (Some(kind), true) = (item.and_then(|item| item.icon), span > 4) {
                    append_icon_images(&mut images, seg.start_col + 1, kind, colors.fg);
                }
                if let (Some(item), true) = (item.filter(|it| it.has_status()), span > 6) {
                    append_status_image(
                        &mut images,
                        seg.end_col.saturating_sub(2),
                        item,
                        colors.accent,
                    );
                }
            }
            TabHit::Select(i) => {
                // Chip-card band: the segment's leading cell stays bare band as the
                // gutter between cards. It is the leading pad `tab_content_layout`
                // already reserves, and `hit_test` still selects on it. A card
                // narrower than two cells keeps both cells.
                let card_start =
                    if STRIP_CHIP_CARDS && seg.end_col.saturating_sub(seg.start_col) >= 2 {
                        seg.start_col + 1
                    } else {
                        seg.start_col
                    };
                for c in seg.start_col..card_start {
                    put(row, c, ' ', StripRole::Inactive);
                }
                // Background-fill the card in the (in)active colour.
                for c in card_start..seg.end_col {
                    put(row, c, ' ', tab_role);
                }
                // Leading separator for a chrome band without chip cards: a hairline
                // on the leading pad cell (costs no title width, moves no hit
                // target), as the native strip does (`toolbar::TabGeometry::separates`).
                // Unreached while every band uses chip cards.
                if STRIP_IS_CHROME_BAND
                    && !STRIP_CHIP_CARDS
                    && strip_separates(i, active, paint.hovered)
                {
                    put(row, seg.start_col, '│', StripRole::Separator);
                }
                let editing = paint.rename.filter(|edit| edit.tab == i);
                // While a chip is being edited the field takes its whole interior;
                // icon and status canvas are suppressed. Close column and segment
                // bounds are untouched.
                let item = metadata
                    .and_then(|items| items.get(i))
                    .copied()
                    .filter(|_| editing.is_none());
                let layout = tab_content_layout(seg, item.unwrap_or(PLAIN_TAB));
                if let (Some(icon_start), Some(kind)) =
                    (layout.icon_start, item.and_then(|item| item.icon))
                {
                    let color = if is_active {
                        colors.fg
                    } else {
                        colors.inactive_fg
                    };
                    append_icon_images(&mut images, icon_start, kind, color);
                }
                if let (Some(item), Some(col)) = (item, layout.status_col) {
                    let ink = if is_active {
                        colors.fg
                    } else {
                        colors.inactive_fg
                    };
                    let color = status_mark_ink(item, &colors, palette, ink);
                    append_status_image(&mut images, col, item, color);
                }
                // Title region follows the icon/status slots. The close column remains
                // exactly where pure layout put it, so paint can never perturb input.
                let raw = titles.get(i).map(String::as_str).unwrap_or("");
                if let Some(edit) = editing {
                    // The field takes the title span verbatim and scrolls; never
                    // `truncate_title`.
                    rename_caret = paint_rename_field(
                        row,
                        usize::from(layout.title_start)..usize::from(layout.title_end),
                        edit.text,
                        edit.cursor,
                        raw,
                        palette,
                        // Only the active chip keeps its selected seam; a context-menu
                        // rename can target a background tab.
                        is_active.then_some(colors.accent),
                    );
                } else {
                    // The distinct pass's label when it ran; otherwise (solo,
                    // renaming, no band) the title's own cut ([`chip_cut`]).
                    let label = labels.get_mut(i).and_then(Option::take).unwrap_or_else(|| {
                        let avail = layout.title_end.saturating_sub(layout.title_start);
                        chip_cut(raw, avail as usize)
                    });
                    // In display cells, so a title never bleeds into the status
                    // canvas, the `✕` or the next chip.
                    put_text(row, layout.title_start, layout.title_end, &label, tab_role);
                }
                if let Some(cx) = seg.close_col
                    && (paint.hovered == Some(i) || (STRIP_CHIP_CARDS && is_active))
                {
                    // Hover-only on the flat strip, as on the native strip; a
                    // chip-card band keeps the selected card's ✕ resident. The column
                    // stays reserved either way, and `close_col` answers `Close`
                    // regardless of paint. ✕ (U+2715) is East-Asian-Width Neutral, so
                    // it stays single-cell under CJK fonts, unlike × (U+00D7).
                    put(row, cx, '✕', tab_role);
                    if STRIP_CHIP_CARDS && let Some(slot) = row.get_mut(cx as usize) {
                        // The mark is an affordance, not a second label: regular
                        // weight beside the selected card's bold title.
                        slot.bold = false;
                    }
                }
            }
            TabHit::NewTab => {
                // The `+` is a button (full-strength fg), so it meets contrast on
                // every theme. On a chrome band it gives its leading pad cell to the
                // band, since `layout_segments` places it flush against the last tab
                // and the two cards would fuse. Paint only: `hit_test` still routes
                // the whole segment to `TabHit::NewTab`.
                let card = seg
                    .start_col
                    .saturating_add(u16::from(STRIP_IS_CHROME_BAND));
                // The button takes the hover wash under the pointer; the flag is a
                // strip cache-key term.
                let plus_role = if paint.new_tab_hovered {
                    StripRole::NewTabHover
                } else {
                    StripRole::NewTab
                };
                for c in card..seg.end_col {
                    put(row, c, ' ', plus_role);
                }
                // Centre the `+` in the 3-cell ` + ` affordance (on a band the card is
                // the trailing two cells, so this is its leading one).
                put(row, seg.start_col + 1, '+', plus_role);
            }
            TabHit::Update => {
                // The leading `↻` update alert, a raised button. Its trailing pad
                // cell is the band gutter mirroring the `+`'s, so an active tab 0
                // does not fuse with it (not on macOS).
                let end = seg.end_col.saturating_sub(u16::from(STRIP_IS_CHROME_BAND));
                for c in seg.start_col..end {
                    put(row, c, ' ', StripRole::Update);
                }
                // Centre the `↻` (U+21BB clockwise open circle arrow) in the 3-cell slot.
                put(row, seg.start_col + 1, '\u{21bb}', StripRole::Update);
            }
            // The trailing version label in cells (the pixel band, where it draws,
            // paints it in the UI face over these cells): bare band at rest, the
            // hover wash under the pointer, the update CTA's surface while an update
            // is staged. The text is right-aligned with one trailing pad cell and
            // steps down to the short rung, then to no text, when the cells are
            // fewer than the slot measured in the UI face.
            TabHit::Version { rung, attention } => {
                let role = if attention {
                    StripRole::Update
                } else if paint.version_hovered {
                    StripRole::Hover
                } else {
                    StripRole::Inactive
                };
                for c in seg.start_col..seg.end_col {
                    put(row, c, ' ', role);
                }
                let room = usize::from(seg.end_col.saturating_sub(seg.start_col)).saturating_sub(1);
                let label = [
                    crate::menu::strip_version_label(rung, attention),
                    crate::menu::strip_version_label(crate::menu::VersionRung::Short, attention),
                ]
                .into_iter()
                .flatten()
                .find(|label| strip_display_cells(label) <= room);
                if let Some(label) = label {
                    let end = seg.end_col.saturating_sub(1);
                    let width = u16::try_from(strip_display_cells(&label)).unwrap_or(u16::MAX);
                    put_text(row, end.saturating_sub(width), end, &label, role);
                }
            }
            // A WINDOW BUTTON — the cell lane's transitional frame for it (the
            // pixel band draws the round button over these cells once the UI face
            // lands): bare band at rest, the hover wash under the pointer, its
            // mark centred in the slot.
            TabHit::Caption(button) => {
                let role = if paint.caption_hovered == Some(button) {
                    StripRole::Hover
                } else {
                    StripRole::Inactive
                };
                for c in seg.start_col..seg.end_col {
                    put(row, c, ' ', role);
                }
                let mark = match button {
                    CaptionButton::Minimize => '_',
                    CaptionButton::Maximize => '□',
                    CaptionButton::Restore => '❐',
                    CaptionButton::Close => '✕',
                };
                let mid = seg.start_col + seg.end_col.saturating_sub(seg.start_col) / 2;
                put(row, mid, mark, role);
            }
            // `Close` is never a segment `kind` (only a derived hit on
            // `close_col`). The needs pill's regions keep the band's ground
            // here; [`paint_strip_pill`] draws the pill over them.
            TabHit::Close(_) | TabHit::Pill(_) => {}
        }
    }
    // The window's edges ([`strip_overflow`]): painted after the segments, on
    // uncovered columns, in the band's dim ink. Where [`pixel_band`] draws, it
    // covers this column and strokes the same sign centred on the band.
    for (col, mark) in strip_overflow(segments, titles.len()).columns() {
        if let Some(slot) = row.get_mut(usize::from(col)) {
            *slot = strip_cell(mark, &colors, StripRole::Inactive);
        }
    }
    images.sort_unstable_by_key(|(col, _)| *col);
    (images, rename_caret)
}

/// Half the length of the tab's own note's dash ([`ChipLevel::Note`]), in
/// the 16-unit design box, before the four-mark shrink: shorter than a
/// mark's diameter, so it reads as a dash, not a rule. The native chip
/// draws the same one (`toolbar.rs`).
pub(crate) const NOTE_DASH_HALF: f32 = 2.0;

/// The ink a chip's status marks are drawn in (one image, one ink): a need's
/// mark wears the border's tone — the stop tone for a stuck need, the wait
/// tone for a decide need, held to AA on the band (the presence inks), as the
/// native chip's red and amber do (owner surfaces §5) — else a dirty tab's the
/// accent, else `ink`, the label's. The tab's own note (a failed command, a
/// notice, [`ChipLevel::Note`]) is no need and wears no presence tone.
fn status_mark_ink(
    item: TabStripMetadata,
    colors: &StripColors,
    palette: ChromePalette,
    ink: [u8; 3],
) -> [u8; 3] {
    let need = |stuck: bool| {
        let inks = aterm_messages::presence::inks(
            crate::chrome_band::presence_tones(palette),
            colors.band_bg,
        );
        if stuck { inks.stop } else { inks.wait }
    };
    match item.attention {
        ChipLevel::Stop(_) => need(true),
        ChipLevel::Wait => need(false),
        _ if item.dirty => colors.accent,
        _ => ink,
    }
}

/// [`status_mark_ink`] on the default theme's strip, for the host's tests.
#[cfg(test)]
pub(crate) fn status_mark_ink_for_test(item: TabStripMetadata, ink: [u8; 3]) -> [u8; 3] {
    let theme = Theme::default();
    status_mark_ink(
        item,
        &strip_colors_with_active(theme, None),
        theme.into(),
        ink,
    )
}

/// Paint the window's NEEDS PILL into the strip row `row` over the columns
/// its regions took ([`TabHit::Pill`], [`layout_segments_with_pill`]): each
/// piece at its span of the same layout ([`strip_pill_layout`]), so what a
/// click resolves to is what is drawn there. The glyph in the border's tone
/// (the presence inks, AA on the band), the tab's label and the words in the
/// strip's ink, each cut with `…` to its span; the act a bold underlined
/// button in the accent; the `+N` dim. On the band's ground: no card, no
/// wash, nothing that moves. Nothing is drawn where no region was laid out.
pub(crate) fn paint_strip_pill(
    row: &mut [RenderCell],
    segments: &[TabSegment],
    pill: StripPill<'_>,
    palette: ChromePalette,
) {
    use crate::presence::{NeedSeverity, PillKind};
    let Some(start) = segments
        .iter()
        .find(|seg| matches!(seg.kind, TabHit::Pill(_)))
        .map(|seg| seg.start_col)
    else {
        return;
    };
    // The pill's share is of the cells between the window's own buttons, as
    // the layout measured it ([`layout_segments_with_caption_sized`]): from
    // the left end's last button to the right end's first.
    let captions = || {
        segments
            .iter()
            .filter(|seg| matches!(seg.kind, TabHit::Caption(_)))
    };
    let end = captions()
        .filter(|seg| seg.start_col >= start)
        .map(|seg| seg.start_col)
        .min()
        .unwrap_or(u16::try_from(row.len()).unwrap_or(u16::MAX));
    let lead = captions()
        .filter(|seg| seg.end_col <= start)
        .map(|seg| seg.end_col)
        .max()
        .unwrap_or(0);
    let cols = end.saturating_sub(lead);
    let Some(layout) = strip_pill_layout(cols, pill.words) else {
        return;
    };
    let colors = strip_colors_for(palette);
    let inks =
        aterm_messages::presence::inks(crate::chrome_band::presence_tones(palette), colors.band_bg);
    let tone = match pill.kind {
        PillKind::Need(NeedSeverity::Stuck) => inks.stop,
        PillKind::Need(NeedSeverity::Decide) => inks.wait,
        PillKind::Yours => inks.yours,
    };
    let mut put = |span: aterm_messages::presence::Span, text: &str, fg: [u8; 3], button: bool| {
        let from = start + 1 + cell_of(span.x);
        let end = from + cell_of(span.w);
        let text = truncate_title(text, usize::from(end - from));
        let mut col = from;
        for ch in text.chars() {
            let ch = strip_char(ch);
            let w = strip_char_cells(ch);
            if col + w > end {
                break;
            }
            if let Some(slot) = row.get_mut(usize::from(col)) {
                let mut cell = strip_cell(ch, &colors, StripRole::Inactive);
                cell.fg = fg;
                cell.bold = button;
                if button {
                    cell.underline = UnderlineStyle::Single;
                    cell.underline_color = Some(fg);
                }
                *slot = cell;
            }
            if w == 2
                && let Some(slot) = row.get_mut(usize::from(col + 1))
            {
                let mut cell = strip_cell(' ', &colors, StripRole::Inactive);
                cell.wide = true;
                *slot = cell;
            }
            col += w;
        }
    };
    let words = pill.words;
    let mut glyph = [0u8; 4];
    put(
        layout.glyph,
        words.glyph.encode_utf8(&mut glyph),
        tone,
        false,
    );
    put(layout.label, &words.label, colors.fg, false);
    put(layout.words, &words.words, colors.fg, false);
    if let Some((act, span)) = layout.act {
        put(span, act.word(), colors.accent, true);
    }
    if let (Some(span), Some(more)) = (layout.more, words.more.as_deref()) {
        put(span, more, colors.inactive_fg, false);
    }
}

const ICON_RASTER_SIZE: u16 = 32;
const ICON_SUPERSAMPLE: u16 = 4;
fn status_primitives(metadata: TabStripMetadata) -> Vec<TabIconPrimitive> {
    let count = metadata.status_count();
    // Shared shrink for the four-mark packing; 1.0 (bit-exact geometry) below.
    let s = tab_status_mark_scale(count);
    let mut ordinal = 0usize;
    let mut primitives = Vec::with_capacity(count * 4);
    for kind in TAB_STATUS_KINDS {
        if !metadata.has_status_kind(kind) {
            continue;
        }
        let x = tab_status_center(ordinal, count);
        ordinal += 1;
        match kind {
            // Solid dot = an authored document has unsaved changes.
            TabStatusKind::Dirty => primitives.push(TabIconPrimitive::Dot {
                center: [x, 8.0],
                radius: 1.75 * s,
            }),
            // Hollow round = work is in progress. RoundedRect gives the deterministic
            // rasterizer a crisp ring without adding a renderer-only circle primitive.
            TabStatusKind::Busy => primitives.push(TabIconPrimitive::RoundedRect {
                rect: [x - 2.0 * s, 8.0 - 2.0 * s, 4.0 * s, 4.0 * s],
                radius: 2.0 * s,
                width: 1.25 * s,
            }),
            // The need mark: a hollow diamond for a decide need, a filled one
            // for a stuck need, the person's hold a hollow square, a finished
            // task not yet looked at a dot, and the tab's own note (a failed
            // command, a notice) a short dash — so a failed `grep` behind
            // never reads as a finished worker; the same marks the native
            // chip draws (`toolbar.rs`).
            TabStatusKind::Attention => {
                let w = 1.15 * s;
                let r = 2.5 * s;
                match metadata.attention {
                    ChipLevel::Off => continue,
                    ChipLevel::Done => {
                        primitives.push(TabIconPrimitive::Dot {
                            center: [x, 8.0],
                            radius: 1.5 * s,
                        });
                        continue;
                    }
                    ChipLevel::Note => {
                        primitives.push(TabIconPrimitive::Line {
                            from: [x - NOTE_DASH_HALF * s, 8.0],
                            to: [x + NOTE_DASH_HALF * s, 8.0],
                            width: 1.25 * s,
                        });
                        continue;
                    }
                    ChipLevel::Yours => {
                        primitives.push(TabIconPrimitive::RoundedRect {
                            rect: [x - 2.2 * s, 8.0 - 2.2 * s, 4.4 * s, 4.4 * s],
                            radius: 0.8 * s,
                            width: 1.15 * s,
                        });
                        continue;
                    }
                    ChipLevel::Stop(_) => {
                        primitives.extend([
                            TabIconPrimitive::Triangle {
                                points: [[x, 8.0 - r], [x + r, 8.0], [x - r, 8.0]],
                            },
                            TabIconPrimitive::Triangle {
                                points: [[x - r, 8.0], [x + r, 8.0], [x, 8.0 + r]],
                            },
                        ]);
                        continue;
                    }
                    ChipLevel::Wait => {}
                }
                primitives.extend([
                    TabIconPrimitive::Line {
                        from: [x, 8.0 - r],
                        to: [x + r, 8.0],
                        width: w,
                    },
                    TabIconPrimitive::Line {
                        from: [x + r, 8.0],
                        to: [x, 8.0 + r],
                        width: w,
                    },
                    TabIconPrimitive::Line {
                        from: [x, 8.0 + r],
                        to: [x - r, 8.0],
                        width: w,
                    },
                    TabIconPrimitive::Line {
                        from: [x - r, 8.0],
                        to: [x, 8.0 - r],
                        width: w,
                    },
                ]);
            }
            // Connection mark: outbound = filled up-triangle, inbound = hollow
            // down-triangle, both = filled hourglass; the diamond's visual mass.
            TabStatusKind::Connection => {
                let Some(role) = metadata.conn else {
                    // Unreachable behind `has_status_kind`, but the arm stays
                    // total rather than trusting the gate at a distance.
                    continue;
                };
                let (hw, hh) = (2.5 * s, 2.4 * s);
                match role {
                    TabConnRole::Outbound => primitives.push(TabIconPrimitive::Triangle {
                        points: [[x, 8.0 - hh], [x + hw, 8.0 + hh], [x - hw, 8.0 + hh]],
                    }),
                    TabConnRole::Inbound => {
                        let w = 1.15 * s;
                        // Corners inset by the half-stroke so the outer silhouette
                        // matches the filled triangle and round caps never clip.
                        let (cw, ch) = (hw - w * 0.5, hh - w * 0.5);
                        primitives.extend([
                            TabIconPrimitive::Line {
                                from: [x - cw, 8.0 - ch],
                                to: [x + cw, 8.0 - ch],
                                width: w,
                            },
                            TabIconPrimitive::Line {
                                from: [x + cw, 8.0 - ch],
                                to: [x, 8.0 + ch],
                                width: w,
                            },
                            TabIconPrimitive::Line {
                                from: [x, 8.0 + ch],
                                to: [x - cw, 8.0 - ch],
                                width: w,
                            },
                        ]);
                    }
                    // Slightly narrower waisted pair so the double fill does
                    // not read heavier than the single arrows.
                    TabConnRole::Both => {
                        let hw = 2.2 * s;
                        primitives.extend([
                            TabIconPrimitive::Triangle {
                                points: [[x - hw, 8.0 - hh], [x + hw, 8.0 - hh], [x, 8.0]],
                            },
                            TabIconPrimitive::Triangle {
                                points: [[x - hw, 8.0 + hh], [x + hw, 8.0 + hh], [x, 8.0]],
                            },
                        ]);
                    }
                }
            }
        }
    }
    primitives
}

/// Rasterize one icon IR to transparent straight-alpha RGBA. A fixed 4×4 coverage
/// grid per pixel keeps it smooth, deterministic and bounded (32×32×16 samples).
/// CPU and GPU scale the same `RawRgba8` payload to the two-cell footprint.
fn rasterize_icon(primitives: &[TabIconPrimitive], color: [u8; 3]) -> Vec<u8> {
    let side = usize::from(ICON_RASTER_SIZE);
    let samples = u32::from(ICON_SUPERSAMPLE).pow(2);
    let mut rgba = vec![0u8; side * side * 4];
    for py in 0..side {
        for px in 0..side {
            let mut covered = 0u32;
            for sy in 0..ICON_SUPERSAMPLE {
                for sx in 0..ICON_SUPERSAMPLE {
                    let fx = (px as f32 + (f32::from(sx) + 0.5) / f32::from(ICON_SUPERSAMPLE))
                        * TAB_ICON_DESIGN_SIZE
                        / f32::from(ICON_RASTER_SIZE);
                    let fy = (py as f32 + (f32::from(sy) + 0.5) / f32::from(ICON_SUPERSAMPLE))
                        * TAB_ICON_DESIGN_SIZE
                        / f32::from(ICON_RASTER_SIZE);
                    covered += u32::from(
                        primitives
                            .iter()
                            .any(|primitive| primitive_covers(*primitive, fx, fy)),
                    );
                }
            }
            let alpha = ((covered * 255 + samples / 2) / samples) as u8;
            let offset = (py * side + px) * 4;
            rgba[offset..offset + 4].copy_from_slice(&[color[0], color[1], color[2], alpha]);
        }
    }
    rgba
}

fn primitive_covers(primitive: TabIconPrimitive, px: f32, py: f32) -> bool {
    match primitive {
        TabIconPrimitive::Line { from, to, width } => {
            point_segment_distance(px, py, from, to) <= width * 0.5
        }
        TabIconPrimitive::RoundedRect {
            rect,
            radius,
            width,
        } => rounded_rect_signed_distance(px, py, rect, radius).abs() <= width * 0.5,
        TabIconPrimitive::Dot { center, radius } => {
            (px - center[0]).hypot(py - center[1]) <= radius
        }
        TabIconPrimitive::Triangle { points } => {
            // Inside iff every edge cross-product carries one sign (winding-
            // agnostic; boundary points count as covered).
            let edge = |a: [f32; 2], b: [f32; 2]| {
                (px - b[0]).mul_add(a[1] - b[1], -((a[0] - b[0]) * (py - b[1])))
            };
            let d0 = edge(points[0], points[1]);
            let d1 = edge(points[1], points[2]);
            let d2 = edge(points[2], points[0]);
            !((d0 < 0.0 || d1 < 0.0 || d2 < 0.0) && (d0 > 0.0 || d1 > 0.0 || d2 > 0.0))
        }
    }
}

fn point_segment_distance(px: f32, py: f32, from: [f32; 2], to: [f32; 2]) -> f32 {
    let (vx, vy) = (to[0] - from[0], to[1] - from[1]);
    let denom = vx.mul_add(vx, vy * vy);
    if denom <= f32::EPSILON {
        return (px - from[0]).hypot(py - from[1]);
    }
    let t = ((px - from[0]).mul_add(vx, (py - from[1]) * vy) / denom).clamp(0.0, 1.0);
    (px - from[0] - vx * t).hypot(py - from[1] - vy * t)
}

/// Signed distance to a rounded-rect boundary (negative inside, positive outside).
fn rounded_rect_signed_distance(px: f32, py: f32, rect: [f32; 4], radius: f32) -> f32 {
    let radius = radius.clamp(0.0, rect[2].min(rect[3]) * 0.5);
    let cx = rect[0] + rect[2] * 0.5;
    let cy = rect[1] + rect[3] * 0.5;
    let qx = (px - cx).abs() - (rect[2] * 0.5 - radius);
    let qy = (py - cy).abs() - (rect[3] * 0.5 - radius);
    let outside = qx.max(0.0).hypot(qy.max(0.0));
    outside + qx.max(qy).min(0.0) - radius
}

fn image_data(primitives: &[TabIconPrimitive], color: [u8; 3], cols: u16) -> Arc<ImageData> {
    Arc::new(ImageData {
        bytes: rasterize_icon(primitives, color),
        format: ImageFormat::RawRgba8 {
            width: ICON_RASTER_SIZE,
            height: ICON_RASTER_SIZE,
        },
        cols,
        rows: 1,
        z_index: 0,
        band_lift_px: 0,
        cell_offset_px: (0, 0),
        scaling: ImageScaling::Fit,
        source_rect: None,
    })
}

/// Memo key for one rasterized strip glyph.
///
/// [`tab_icon_primitives`] is a `const fn` of the kind; [`status_primitives`] reads
/// `dirty`, `busy`, `attention` and `conn`, but `Status` does not key on `conn`, so
/// connection roles that agree on the other fields share one raster. The variants
/// keep the `cols` split (`ICON_COLS` vs 1) structural; both renderers' image-cache
/// keys embed `cols * cell_w`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum IconKey {
    Icon(TabIconKind, [u8; 3]),
    Status {
        dirty: bool,
        busy: bool,
        attention: ChipLevel,
        color: [u8; 3],
    },
}

/// Hard cap on the memo: 16 × 4 KiB = 64 KiB. FIFO eviction keeps a theme cycle
/// (which mints new colours) bounded.
const ICON_MEMO_CAP: usize = 16;

thread_local! {
    /// Paint is main-thread only, so a thread-local avoids a lock and a signature
    /// change. A `Vec` beats a `HashMap` at this size.
    static ICON_MEMO: RefCell<Vec<(IconKey, Arc<ImageData>)>> = const { RefCell::new(Vec::new()) };
}

/// Reuse the `Arc<ImageData>` of an already-rasterized glyph.
///
/// Stable `Arc` identity across strip rebuilds matters more than skipping the
/// raster: both renderers decode through `aterm_render::ImageCache`, which keys a
/// decode on the source `Arc`'s address, and placements key on `Arc::as_ptr` for the
/// `image_plane` reuse fast path. Nothing mutates through these `Arc`s or branches on
/// their count.
fn memoized_image(key: IconKey, build: impl FnOnce() -> Arc<ImageData>) -> Arc<ImageData> {
    ICON_MEMO.with(|memo| {
        let mut memo = memo.borrow_mut();
        if let Some((_, image)) = memo.iter().find(|(entry, _)| *entry == key) {
            return image.clone();
        }
        let image = build();
        if memo.len() >= ICON_MEMO_CAP {
            memo.remove(0);
        }
        memo.push((key, image.clone()));
        image
    })
}

fn append_icon_images(
    images: &mut Vec<(usize, ImageRef)>,
    start_col: u16,
    kind: TabIconKind,
    color: [u8; 3],
) {
    let image = memoized_image(IconKey::Icon(kind, color), || {
        image_data(tab_icon_primitives(kind), color, ICON_COLS)
    });
    for cell_col in 0..ICON_COLS {
        images.push((
            usize::from(start_col + cell_col),
            ImageRef {
                image: image.clone(),
                cell_row: 0,
                cell_col,
                kitty: None,
            },
        ));
    }
}

fn append_status_image(
    images: &mut Vec<(usize, ImageRef)>,
    col: u16,
    metadata: TabStripMetadata,
    color: [u8; 3],
) {
    // Built lazily so a memo hit skips `status_primitives`' `Vec` as well.
    let image = memoized_image(
        IconKey::Status {
            dirty: metadata.dirty,
            busy: metadata.busy,
            attention: metadata.attention,
            color,
        },
        || image_data(&status_primitives(metadata), color, 1),
    );
    images.push((
        usize::from(col),
        ImageRef {
            image,
            cell_row: 0,
            cell_col: 0,
            kitty: None,
        },
    ));
}

/// Clipped-edge markers for the inline rename field; the find bar's glyphs.
const RENAME_SCROLL_LEFT: char = '‹';
const RENAME_SCROLL_RIGHT: char = '›';

/// Clipped-edge markers for the strip's own window ([`strip_overflow`]): the same
/// glyphs as the rename well, one vocabulary for "there is more this way".
const STRIP_OVERFLOW_LEFT: char = RENAME_SCROLL_LEFT;
const STRIP_OVERFLOW_RIGHT: char = RENAME_SCROLL_RIGHT;

/// Paint the inline session-rename field across `field` (a column range of `row`):
/// a recessed well holding the visible slice of `text`, with a reverse-video block
/// caret on the character at `cursor`, and `underline` keeping the active chip's
/// accent seam under it.
///
/// The same machine as [`crate::find_bar`]'s well: the caret costs no cell, the
/// view scrolls the minimum needed to keep the caret inside, `‹`/`›` mark clipped
/// edges without overwriting the caret, and an empty field shows the fallback label
/// dimmed. The caret is placed by char index into the [`strip_char`] projection
/// (each char at least one cell), so wide or non-BMP characters cannot desync it
/// from the stored byte offset.
///
/// Returns the caret's painted column (where the caller anchors the OS IME
/// candidate window), or `None` on degenerate geometry.
fn paint_rename_field(
    row: &mut [RenderCell],
    field: std::ops::Range<usize>,
    text: &str,
    cursor: usize,
    placeholder: &str,
    palette: ChromePalette,
    underline: Option<[u8; 3]>,
) -> Option<usize> {
    let width = field.len();
    if width == 0 || field.end > row.len() {
        return None;
    }
    let c = crate::chrome_band::band_colors(palette);
    let well = |ch: char, fg: [u8; 3], bg: [u8; 3]| {
        let mut cell = crate::chrome_band::cell(ch, fg, bg, false, false);
        if let Some(accent) = underline {
            cell.underline = UnderlineStyle::Single;
            cell.underline_color = Some(accent);
        }
        cell
    };
    // Each projected char with the display cells it costs (1 or 2).
    let chars: Vec<(char, usize)> = text
        .chars()
        .map(|raw| {
            let ch = strip_char(raw);
            (ch, usize::from(strip_char_cells(ch)))
        })
        .collect();
    // Caret in chars of the projection; the byte offset is always on a char
    // boundary (the field reducer's invariant).
    let caret = text
        .get(..cursor.min(text.len()))
        .map_or(chars.len(), |head| head.chars().count())
        .min(chars.len());
    for cell in &mut row[field.clone()] {
        *cell = well(' ', c.value, c.field_bg);
    }
    // Scroll in chars, budgeted in display cells: walk back from the caret spending
    // the field's width, caret cells first (one cell when past the last char), so
    // the caret stays inside with maximum left context.
    let caret_cells = chars.get(caret).map_or(1, |&(_, w)| w).min(width);
    let mut scroll = caret;
    let mut budget = width - caret_cells;
    while scroll > 0 && chars[scroll - 1].1 <= budget {
        scroll -= 1;
        budget -= chars[scroll].1;
    }
    // Lay the window out left to right. A wide char that would straddle the right
    // edge is dropped whole. Track the edge cells so the `‹`/`›` markers never
    // orphan half a pair.
    let mut col = field.start;
    let mut index = scroll;
    let mut caret_on_last_cell = false;
    let mut pair_ends_field = false;
    let mut caret_col = None;
    while index <= chars.len() && col < field.end {
        let (ch, w) = chars.get(index).copied().unwrap_or((' ', 1));
        if col + w > field.end {
            break;
        }
        let (ink, ground) = if index == caret {
            // Reverse video: the well's background becomes the ink.
            caret_col = Some(col);
            (c.field_bg, c.caret)
        } else {
            (c.value, c.field_bg)
        };
        row[col] = well(ch, ink, ground);
        if w == 2 {
            // The continuation shares the lead's ground, so a caret over a wide
            // char covers both cells.
            let mut tail = well(' ', ink, ground);
            tail.wide = true;
            row[col + 1] = tail;
            pair_ends_field = col + 2 == field.end;
        } else {
            pair_ends_field = false;
        }
        if index == caret && col + w == field.end {
            caret_on_last_cell = true;
        }
        col += w;
        index += 1;
    }
    if chars.is_empty() {
        // An empty field means "fall back down the ladder": show what it falls back
        // to, dimmed, after the caret, never as content, so Return still clears.
        let mut col = field.start + 2;
        for raw in placeholder.chars() {
            let ch = strip_char(raw);
            let w = usize::from(strip_char_cells(ch));
            if col + w > field.end {
                break;
            }
            row[col] = well(ch, c.label, c.field_bg);
            if w == 2 {
                let mut tail = well(' ', c.label, c.field_bg);
                tail.wide = true;
                row[col + 1] = tail;
            }
            col += w;
        }
    }
    // Clipped-edge markers, never over the caret (the caret's cell is the one place
    // the user is looking, and it already proves where the edit position is).
    if scroll > 0 && caret != scroll {
        // The first visible char loses its lead cell to the marker; if it was wide,
        // blank its orphaned continuation too.
        if chars.get(scroll).is_some_and(|&(_, w)| w == 2) && field.start + 1 < field.end {
            row[field.start + 1] = well(' ', c.value, c.field_bg);
        }
        row[field.start] = well(RENAME_SCROLL_LEFT, c.label, c.field_bg);
    }
    if index < chars.len() && !caret_on_last_cell {
        // When a wide pair ends exactly at the field edge, the marker replaces its
        // continuation, so blank the lead as well.
        if pair_ends_field && field.end >= 2 && field.end - 2 >= field.start {
            row[field.end - 2] = well(' ', c.value, c.field_bg);
        }
        row[field.end - 1] = well(RENAME_SCROLL_RIGHT, c.label, c.field_bg);
    }
    caret_col
}

/// Blank cells between the solo band's title and its description.
const SOLO_GAP_COLS: usize = 3;
/// Cells kept clear at each end of the solo band, so the centred group never
/// touches the segment edge (and leaves the icon / status marks their cells).
const SOLO_EDGE_COLS: usize = 2;
/// Below this many cells a description says nothing a reader can use (`~…` is not
/// a path), so the band drops it rather than spend the width on an ellipsis.
const SOLO_MIN_DESC_COLS: usize = 4;

/// Fit the solo band's title and description into `span` cells: the description is
/// compressed, then dropped, before the title gives up a cell. Both strings come
/// back ellipsised by [`truncate_title`]. The native strip does the same in points
/// (`toolbar::relayout_solo`).
#[must_use]
fn solo_band_text(title: &str, subtitle: Option<&str>, span: usize) -> (String, Option<String>) {
    let usable = span.saturating_sub(2 * SOLO_EDGE_COLS);
    // Budgeted in display cells: the painter places wide chars as cell pairs.
    let title_len = strip_display_cells(title).min(usable);
    let title = truncate_title(title, title_len);
    // Whatever the title left, minus the gap — and only if what survives is still
    // readable ([`SOLO_MIN_DESC_COLS`]); a lone "…" says less than nothing.
    let room = usable
        .saturating_sub(strip_display_cells(&title))
        .saturating_sub(SOLO_GAP_COLS);
    let subtitle = subtitle
        .filter(|_| room >= SOLO_MIN_DESC_COLS)
        .map(|text| truncate_title(text, room))
        .filter(|text| !text.is_empty() && text != "…");
    (title, subtitle)
}

/// Truncate `title` to at most `max` display cells ([`strip_char_cells`]),
/// appending `…` when cut; `max == 0` yields "". A width-2 char that would straddle
/// the budget is dropped whole, so the `…` may sit one cell early.
fn truncate_title(title: &str, max: usize) -> String {
    if max == 0 {
        return String::new();
    }
    if strip_display_cells(title) <= max {
        return title.to_string();
    }
    if max == 1 {
        return "…".to_string();
    }
    let keep = max - 1;
    let mut out = String::new();
    let mut used = 0usize;
    for c in title.chars() {
        let w = usize::from(strip_char_cells(strip_char(c)));
        if used + w > keep {
            break;
        }
        out.push(c);
        used += w;
    }
    out.push('…');
    out
}

/// The mark a [`command_cut`] sets between the head it keeps and the end of
/// the title's last argument: the ellipsis with a space on each side, 3 cells.
const MIDDLE_MARK: &str = " \u{2026} ";

/// A chip's cut of a title that does not fit `max` display cells: a command
/// line keeps its last argument ([`command_cut`]), any other title its head
/// ([`truncate_title`]). A title that fits is returned whole.
fn chip_cut(title: &str, max: usize) -> String {
    command_cut(title, max).unwrap_or_else(|| truncate_title(title, max))
}

/// A command line cut in the MIDDLE, so its last argument survives:
/// `vim -n -R … src/platform_win.rs`, `less … CHANGELOG.md`. A head cut lost
/// exactly what tells one such tab from another, vim's file (the 2026-10-18
/// judges' `j3d-long-title-end-cut`); Windows Terminal and VS Code keep a
/// path's meaningful end the same way.
///
/// The shape is two or more words whose last names a file or a path
/// ([`names_a_file`]). The tail is sized first, as it is what names the tab:
/// the last word whole, else its longest tail that starts a path component
/// (`src/platform_win.rs`, then `platform_win.rs`), within what the first word
/// and [`MIDDLE_MARK`] leave. The head then keeps as many whole leading words
/// as the rest of `max` holds, a quoted argument kept whole, and at least the
/// first. Where the first word, the mark and the file's own name do not fit
/// together, the file is cut like a path, its longest such tail behind a
/// leading `…` (`…platform_win.rs`). `None` when the title fits, is not of
/// that shape, or not even the file's own name fits behind the mark: the head
/// cut answers.
///
/// `None` too for a title that ENDS in `…`: its end is already cut off, so its
/// last word is a fragment, not the file. The shell integrations end a command
/// line they cut so (at 256 characters); a 64-character cut with no mark made
/// the tab read `vim … crates/aterm-gui/src/plat` as if `plat` were the file
/// (the 2026-10-18 judges' `J4F-1-cut64-middle-cut-misleads`). The head cut
/// keeps the title's start and ends it with its own mark.
fn command_cut(title: &str, max: usize) -> Option<String> {
    if strip_display_cells(title) <= max {
        return None;
    }
    let title = title.trim();
    // A path with a blank in it is one name, cut as a path ([`path_chip_label`]).
    if is_path_subject(title) || title.ends_with('\u{2026}') {
        return None;
    }
    let (space, gap) = title
        .char_indices()
        .rev()
        .find(|(_, c)| c.is_whitespace())?;
    let last = &title[space + gap.len_utf8()..];
    let head = title[..space].trim_end();
    if head.is_empty() || !names_a_file(last) {
        return None;
    }
    let first = head
        .split_once(char::is_whitespace)
        .map_or(head, |(word, _)| word);
    let mark = strip_display_cells(MIDDLE_MARK);
    // The last word whole, then each tail after one of its separators,
    // longest first; a trailing separator is no component of its own.
    let word = last.trim_end_matches(['/', '\\']);
    let tails: Vec<&str> = std::iter::once(last)
        .chain(
            word.char_indices()
                .filter(|&(_, c)| matches!(c, '/' | '\\'))
                .map(|(at, c)| &last[at + c.len_utf8()..]),
        )
        .filter(|tail| !tail.is_empty())
        .collect();
    let fits = |room: usize| {
        tails
            .iter()
            .copied()
            .find(move |tail| strip_display_cells(tail) <= room)
    };
    let Some(tail) = max
        .checked_sub(strip_display_cells(first) + mark)
        .and_then(&fits)
    else {
        // No room for the program beside the file: the file alone, behind
        // the mark, as a tail cut (`…platform_win.rs`).
        return max
            .checked_sub(1)
            .and_then(&fits)
            .map(|tail| format!("\u{2026}{tail}"));
    };
    let head_room = max - mark - strip_display_cells(tail);
    // Where the head may end: before a blank outside quotes, or its end.
    let mut quote: Option<char> = None;
    let mut kept = first;
    for (at, c) in head.char_indices() {
        match (quote, c) {
            (Some(open), c) if c == open => quote = None,
            (None, '\'' | '"') => quote = Some(c),
            (None, c) if c.is_whitespace() => {
                let words = head[..at].trim_end();
                if strip_display_cells(words) > head_room {
                    break;
                }
                kept = words;
            }
            _ => {}
        }
    }
    if strip_display_cells(head) <= head_room {
        kept = head;
    }
    Some(format!("{kept}{MIDDLE_MARK}{tail}"))
}

/// Whether a title's last word names a file or a path, as a command line's
/// argument does: it holds a `/` or `\` with a name before or after it, or it
/// ends in an extension (`.rs`, `.md`, `.json`: one to eight letters and
/// digits, at least one a letter, after a name). A version (`0.106`), a
/// sentence's last word and `e.g.` do not.
fn names_a_file(word: &str) -> bool {
    let word = word.trim_matches(|c: char| matches!(c, '\'' | '"' | ',' | ';' | ')' | '('));
    if word.contains(['/', '\\']) {
        return word.chars().any(|c| !matches!(c, '/' | '\\' | '.' | '~'));
    }
    word.rsplit_once('.').is_some_and(|(stem, ext)| {
        !stem.is_empty()
            && !stem.ends_with('.')
            && (1..=8).contains(&ext.len())
            && ext.chars().all(|c| c.is_ascii_alphanumeric())
            && ext.chars().any(|c| c.is_ascii_alphabetic())
    })
}

/// [`truncate_title`]'s mirror: keep the tail within `max` display cells, with a
/// leading `…` for the cut head. Same whole-glyph rule. For titles that differ only
/// at the end (`user@host: ~/aterm` vs `user@host: $HOME/trust`).
fn truncate_title_tail(title: &str, max: usize) -> String {
    if max == 0 {
        return String::new();
    }
    if strip_display_cells(title) <= max {
        return title.to_string();
    }
    if max == 1 {
        return "…".to_string();
    }
    let keep = max - 1;
    let mut used = 0usize;
    let mut start = title.len();
    for (idx, c) in title.char_indices().rev() {
        let w = usize::from(strip_char_cells(strip_char(c)));
        if used + w > keep {
            break;
        }
        start = idx;
        used += w;
    }
    // Snap forward to the next word boundary (space or a title separator) when one
    // exists inside the budget and enough text survives, so the tail does not open
    // mid-word. A tail with no boundary (a long path component, a hash) keeps the
    // exact cut.
    let cut = title[start..]
        .char_indices()
        .skip(1)
        .filter(|(_, c)| matches!(c, ' ' | '\u{00b7}' | '/' | ':' | '\u{2014}'))
        .map(|(i, c)| start + i + c.len_utf8())
        .find(|&b| {
            let rest = &title[b..];
            !rest.trim_start().is_empty() && strip_display_cells(rest) * 2 >= used
        })
        .unwrap_or(start);
    let mut out = String::from("…");
    out.push_str(title[cut..].trim_start());
    out
}

/// Resolve every chip's painted label in one pass over all tabs, so a cut keeps
/// what distinguishes a tab ([`STRIP_CHIP_CARDS`] band).
///
/// Indexed by tab; `None` for a tab this pass does not label (solo band, the tab
/// under an inline rename, out-of-range segments), which the painter truncates per
/// tab instead.
///
/// Rules, in order:
/// - Every label is first cut head-first ([`truncate_title`]), a command line
///   whose last word names a file in the middle so that word survives
///   ([`command_cut`]); a title that fits is shown whole.
/// - Cut tabs cluster when their common prefix is at least half the smaller label
///   window. A cluster sheds its common suffix (snapped to a word boundary,
///   [`snap_shed`]), then cuts at the last word boundary inside the common head
///   when the rest fits (`…~/aterm`), else a plain tail cut. Clusters flip
///   together, so one strip speaks one dialect.
/// - Twins (cut tabs with an identical subject) cannot be told apart by text, so
///   each non-active twin is labelled with its 1-based tab-order position
///   ([`ordinal_chip_label`]): `2 · …~/aterm`, or bare `2`. The active twin keeps
///   its subject. A family made only of twins paints context ([`twin_tail`]),
///   never splitting a directory name; a mixed family keeps the tail dialect.
/// - Under pressure (a compressed chip or a windowed strip), once any cluster
///   flips to tails, the remaining cut loners flip too. A twin family that kept
///   its head flips nobody. A roomy strip keeps head cuts.
/// - A path subject takes its own cut ([`path_chip_label`]): its end, told apart
///   from every other path in the window. Neither the cluster arm nor the flip
///   re-cuts it; twins still take an ordinal, a path twin with its path's own
///   cut behind it ([`path_ordinal_label`]).
fn distinct_chip_labels(
    segments: &[TabSegment],
    titles: &[String],
    metadata: Option<&[TabStripMetadata]>,
    active: usize,
    renaming: Option<usize>,
) -> Vec<Option<String>> {
    distinct_chip_labels_in(segments, titles, metadata, active, renaming, 1.0)
}

/// A chip's title budget, in display cells, for `cells` cell columns of title
/// space set at `widen` display cells per column ([`distinct_chip_labels_in`]).
fn widened_budget(cells: usize, widen: f32) -> usize {
    if widen.is_finite() && widen > 1.0 {
        (cells as f32 * widen).floor() as usize
    } else {
        cells
    }
}

/// [`distinct_chip_labels`] with every chip's title budget widened to `widen`
/// display cells per cell column. The cell lane passes 1.0. The pixel band sets
/// its titles in the UI face, whose average character is narrower than a
/// terminal cell ([`pixel_band::ui_cells_per_cell`]), so cutting there in cell
/// columns left a 127 px card reading `Settin…` and half-empty labels (the
/// 2026-10-17 audit's `tab-label-cut-by-cell-count`); it cuts in UI-sized cells
/// and its pixel fit trims what the average overestimated. Pressure is still
/// read off the segments' own widths.
fn distinct_chip_labels_in(
    segments: &[TabSegment],
    titles: &[String],
    metadata: Option<&[TabStripMetadata]>,
    active: usize,
    renaming: Option<usize>,
    widen: f32,
) -> Vec<Option<String>> {
    let mut labels: Vec<Option<String>> = vec![None; titles.len()];
    // Clamp `active` as `layout_segments` does, so the twin exemption applies to
    // the tab the layout treated as selected.
    let active = active.min(titles.len().saturating_sub(1));
    // The strip is under pressure when any chip was compressed below the floor
    // (only the pressure branch emits one) or when it seated a window
    // ([`STRIP_SEATS_FLOOR`]): windowing replaces compression, so the one-dialect
    // rule below must apply there too.
    let seated = segments
        .iter()
        .filter(|seg| matches!(seg.kind, TabHit::Select(_)) && !seg.solo)
        .count();
    let pressure = seated < titles.len()
        || segments.iter().any(|seg| {
            matches!(seg.kind, TabHit::Select(_))
                && !seg.solo
                && seg.end_col.saturating_sub(seg.start_col) < PREFERRED_MIN_TAB_COLS
        });
    // (tab, its title width budget) for every label the first cut shortened.
    let mut cut: Vec<(usize, usize)> = Vec::new();
    // Every tab's title, seated or not: what a path chip's cut is told apart from
    // ([`path_chip_label`]).
    let subjects: Vec<&str> = titles.iter().map(String::as_str).collect();
    // Cut tabs whose subject is a path. Their first cut is already final
    // ([`path_chip_label`]), so neither the cluster arm nor the flip re-cuts them
    // and their label does not depend on neighbours.
    let mut path_cut = vec![false; titles.len()];
    for seg in segments {
        let TabHit::Select(i) = seg.kind else {
            continue;
        };
        if seg.solo || renaming == Some(i) || i >= titles.len() {
            continue;
        }
        let item = metadata.and_then(|items| items.get(i)).copied();
        let layout = tab_content_layout(seg, item.unwrap_or(PLAIN_TAB));
        let avail = widened_budget(
            usize::from(layout.title_end.saturating_sub(layout.title_start)),
            widen,
        );
        let raw = titles[i].as_str();
        let over = strip_display_cells(raw) > avail;
        // A path keeps its end: its head is ancestry every tab shares. The path's
        // own cut is the same whichever branch labels it, so its chip holds still.
        path_cut[i] = over && is_path_subject(raw);
        let path = path_cut[i].then(|| path_chip_label(i, &subjects, avail));
        labels[i] = Some(path.unwrap_or_else(|| chip_cut(raw, avail)));
        if over {
            cut.push((i, avail));
        }
    }
    // Union-find over the cut tabs (tab counts are small; O(n²) pairs).
    let mut parent: Vec<usize> = (0..cut.len()).collect();
    fn find(parent: &mut [usize], mut x: usize) -> usize {
        while parent[x] != x {
            parent[x] = parent[parent[x]];
            x = parent[x];
        }
        x
    }
    for a in 0..cut.len() {
        for b in a + 1..cut.len() {
            let (i, avail_i) = cut[a];
            let (j, avail_j) = cut[b];
            let lcp = common_prefix_bytes(&titles[i], &titles[j]);
            let lcp_cells = strip_display_cells(&titles[i][..lcp]);
            if lcp_cells * 2 >= avail_i.min(avail_j) {
                let (ra, rb) = (find(&mut parent, a), find(&mut parent, b));
                parent[ra] = rb;
            }
        }
    }
    // Which cut tabs a cluster relabelled (parallel to `cut`) — the pressure
    // dialect flip below only touches the loners the clusters never caught.
    let mut relabelled = vec![false; cut.len()];
    // Clusters (by union-find root) that are twin families which kept their head
    // ([`twin_tail`]): they give the loners no tail dialect to flip into.
    let mut kept_head = vec![false; cut.len()];
    for a in 0..cut.len() {
        let root = find(&mut parent, a);
        let members: Vec<usize> = (0..cut.len())
            .filter(|&b| find(&mut parent, b) == root)
            .map(|b| cut[b].0)
            .collect();
        if members.len() < 2 {
            continue;
        }
        relabelled[a] = true;
        let (i, avail) = cut[a];
        // Shed the cluster's common raw-title suffix. Byte-identical titles share
        // the whole title, so this contributes nothing there and the twin rule
        // below answers.
        let mut suffix = usize::MAX;
        for window in members.windows(2) {
            suffix = suffix.min(common_suffix_bytes(&titles[window[0]], &titles[window[1]]));
        }
        // Snapped to a word boundary, so the shed takes whole words
        // ([`snap_shed`]).
        let suffix = snap_shed(&titles[i], suffix);
        let core = cut_core(&titles[i], suffix);
        // Prefer the cut at the last word boundary inside the shared head
        // (`…~/aterm`) over a raw tail that keeps a fragment of the shared prompt.
        let mut prefix = usize::MAX;
        for window in members.windows(2) {
            prefix = prefix.min(common_prefix_bytes(&titles[window[0]], &titles[window[1]]));
        }
        // Both offsets are char-aligned, so the min slices safely even when head
        // and tail overlap on a short member.
        let head = &core[..prefix.min(core.len())];
        let boundary = head.rfind(char::is_whitespace).map_or(head.len(), |p| {
            p + head[p..].chars().next().map_or(1, char::len_utf8)
        });
        let remainder = &core[boundary..];
        // Twins by subject: no cut tells this title from members whose shed title
        // matches its own, so a non-active twin is labelled by ordinal. The active
        // twin keeps its subject.
        let twins = members
            .iter()
            .filter(|&&m| cut_core(&titles[m], suffix) == core)
            .count();
        // A family made only of twins has no sibling ending to be told from, so it
        // paints context ([`twin_tail`]). A mixed family keeps the tail dialect
        // ([`family_tail`]), where a twin's ending distinguishes it.
        let family_of_twins = twins == members.len();
        // The ending the survivor check and the ordinal's tail shed
        // ([`shared_furniture`]). A cluster's own members show what is furniture
        // among them: a tail one chip outside it happens to end with (`aterm` of
        // `building: aterm`, which `checking: aterm` in another cluster shares)
        // is the very name that tells its members apart. A family of twins shares
        // its whole title, so inside it no ending is furniture; only the strip's
        // other cut chips can show that its ending (`· Ready`) names none of
        // them, and, as for a loner below, one match is coincidence.
        let furniture = if family_of_twins {
            let outside: Vec<&str> = cut
                .iter()
                .map(|&(m, _)| m)
                .filter(|m| !members.contains(m))
                .map(|m| titles[m].as_str())
                .collect();
            // Snapped to whole words (both arms), like the survivor rule's own
            // shed: an ending others share (`ing: aterm` of `checking: aterm` and
            // `linting: aterm`) must not leave `build` of `building:` or `check`.
            snap_shed(&titles[i], shared_furniture(&titles[i], &outside, 2))
        } else {
            let siblings: Vec<&str> = members
                .iter()
                .filter(|&&m| m != i)
                .map(|&m| titles[m].as_str())
                .collect();
            snap_shed(&titles[i], shared_furniture(&titles[i], &siblings, 1))
        };
        let tail_cut: fn(&str, &str, usize) -> String = if family_of_twins {
            twin_tail
        } else {
            family_tail
        };
        // A path twin's ordinal with the path's own cut behind it, where the
        // window affords one: the subject the active twin shows
        // (`2 · …\work\sub dir`). A family of identical paths used to leave the
        // quiet twin a word-tail (`2 · …dir`, cut at the space in `sub dir`).
        let path_ordinal = (twins >= 2 && i != active && path_cut[i])
            .then(|| path_ordinal_label(i, &subjects, avail))
            .filter(|label| !family_of_twins || label.contains(" · "));
        let label = if twins >= 2 && i != active {
            if let Some(label) = path_ordinal {
                // Among other members the path's own cut, bare or not: the
                // family cut would split the directory name. In a family of
                // twins only a cut with a tail; a bare number leaves the
                // family's own context ([`twin_tail`]) to speak.
                label
            } else {
                ordinal_chip_label(i, &titles[i], core, remainder, avail, furniture, tail_cut)
            }
        } else if path_cut[i] {
            // A path keeps the label its first cut gave it ([`path_chip_label`]),
            // independent of which neighbours are cut or seated. Twins by subject
            // still take their ordinal, above.
            labels[i].clone().unwrap_or_default()
        } else {
            furniture_survivor_recut(
                &titles[i],
                furniture,
                avail,
                tail_cut(core, remainder, avail),
            )
        };
        if family_of_twins {
            let ordinal = format!("{} · ", i + 1);
            let text = if i == active {
                label.as_str()
            } else {
                label.strip_prefix(ordinal.as_str()).unwrap_or("")
            };
            kept_head[root] |= is_head_cut(core, text);
        }
        labels[i] = Some(label);
    }
    // A cluster that relabelled in the tail dialect: every cluster but a family
    // of twins that kept its head.
    let mut any_cluster = false;
    for a in 0..cut.len() {
        if relabelled[a] && !kept_head[find(&mut parent, a)] {
            any_cluster = true;
        }
    }
    // One dialect per strip under pressure: cut loners join a flipped cluster's
    // tail dialect. Roomy strips keep head cuts for distinct heads.
    if pressure && any_cluster {
        // Resolve every loner's tail candidate before writing any: a candidate that
        // byte-collides with another label keeps its head cut (e.g. `README.md` and
        // `cargo build` both tail-cut to `…d` in a one-cell window).
        let mut flips: Vec<(usize, String)> = Vec::new();
        for (a, &(i, avail)) in cut.iter().enumerate() {
            // A path's first cut is already its own.
            if relabelled[a] || path_cut[i] {
                continue;
            }
            // A loner can share its ending without sharing a head (a program's
            // ` - zsh` on every idle chip). Shed the longest tail this loner
            // shares with at least two other cut tabs (the second-largest pairwise
            // suffix; one match is coincidence). Shed only when that furniture
            // fills the whole visible keep; distinguishing characters outrank it.
            let mut shared: Vec<usize> = cut
                .iter()
                .filter(|&&(j, _)| j != i)
                .map(|&(j, _)| common_suffix_bytes(&titles[i], &titles[j]))
                .collect();
            shared.sort_unstable_by(|a, b| b.cmp(a));
            let furniture = shared.get(1).copied().unwrap_or(0);
            let mut suffix = furniture;
            if suffix >= titles[i].len()
                || strip_display_cells(&titles[i][titles[i].len() - suffix..])
                    < avail.saturating_sub(1)
            {
                suffix = 0;
            }
            let core = cut_core(&titles[i], suffix);
            let mut cand = truncate_title_tail(core, avail);
            // Survivor check: a word-boundary snap can move the cut past the
            // distinguishing characters, leaving only shared furniture
            // (`…- zsh`). Then shed the furniture and re-cut.
            if suffix == 0 && furniture > 0 && furniture < titles[i].len() {
                let painted = cand.trim_start_matches('…').trim_start();
                let shed_tail = titles[i][titles[i].len() - furniture..].trim_start();
                if !painted.is_empty() && shed_tail.ends_with(painted) {
                    let recut = truncate_title_tail(cut_core(&titles[i], furniture), avail);
                    if !recut.trim_start_matches('…').trim().is_empty() {
                        cand = recut;
                    }
                }
            }
            flips.push((i, cand));
        }
        for (n, (i, cand)) in flips.iter().enumerate() {
            let collides = flips
                .iter()
                .enumerate()
                .any(|(m, (_, other))| m != n && other == cand)
                || labels
                    .iter()
                    .enumerate()
                    .any(|(j, l)| j != *i && l.as_deref() == Some(cand));
            if !collides {
                labels[*i] = Some(cand.clone());
            }
        }
    }
    labels
}

/// Re-cut a cluster label that kept only shared furniture against the tab's own
/// text.
///
/// The cluster sheds the suffix shared by every member, so one member with a
/// different ending collapses it and the cut can keep the furniture (`…- zsh`).
/// When the painted text is only the tail this title shares as `furniture` (in
/// bytes, from [`shared_furniture`]), shed that suffix and cut again; an empty
/// re-cut is declined.
fn furniture_survivor_recut(title: &str, furniture: usize, budget: usize, label: String) -> String {
    let painted = label.trim_start_matches('…').trim_start().to_string();
    if painted.is_empty() {
        return label;
    }
    if furniture > 0
        && furniture < title.len()
        && title[title.len() - furniture..]
            .trim_start()
            .ends_with(&painted)
    {
        let shed = &title[..title.len() - furniture];
        let recut = truncate_title_tail(shed, budget);
        if !recut.trim_start_matches('…').trim().is_empty() {
            return recut;
        }
    }
    label
}

/// The longest ending `title` shares with at least `quorum` of `others`, in bytes:
/// what [`furniture_survivor_recut`] sheds. That is the `quorum`-th largest
/// pairwise suffix, since shared suffixes nest. Byte-identical twins are skipped
/// (`n < title.len()`): they share everything and get an ordinal instead.
fn shared_furniture(title: &str, others: &[&str], quorum: usize) -> usize {
    let mut shared: Vec<usize> = others
        .iter()
        .map(|other| common_suffix_bytes(title, other))
        .filter(|&n| n > 0 && n < title.len())
        .collect();
    shared.sort_unstable_by(|a, b| b.cmp(a));
    quorum
        .checked_sub(1)
        .and_then(|nth| shared.get(nth))
        .copied()
        .unwrap_or(0)
}

/// The label a twin paints: its 1-based position, with a tail of the title when
/// the window affords one (`2 · …oml`), bare (`2`) when not. `core`/`remainder`
/// are the cluster's suffix-shed title and word-boundary tail; `tail_cut` is
/// [`family_tail`] for a twin among other members or [`twin_tail`] for a family of
/// twins. `furniture` feeds the same [`furniture_survivor_recut`] rule the family
/// cut uses.
///
/// The number is the tab's position in the window's tab order, not among painted
/// chips, so it stays put while a windowed strip ([`STRIP_SEATS_FLOOR`]) pages. For
/// the first nine it is also the `switch_tab_<n>` index: the actions parse on every
/// platform, no built-in default binds them, and the hardcoded Super+digit chords
/// are off on Linux (`app_input::HARDCODED_SUPER_CHORDS`), so only on Windows does a
/// keystroke reach the number out of the box. macOS paints no band
/// ([`STRIP_IS_CHROME_BAND`]), so these ordinals never appear there.
///
/// When even the digits do not fit `avail`, the label is the bare `…`: a clipped
/// ordinal would name the wrong tab and a title cut would be a shared stub.
fn ordinal_chip_label(
    tab: usize,
    title: &str,
    core: &str,
    remainder: &str,
    avail: usize,
    furniture: usize,
    tail_cut: fn(&str, &str, usize) -> String,
) -> String {
    let digits = (tab + 1).to_string();
    let digit_cells = strip_display_cells(&digits);
    if digit_cells > avail {
        return if avail == 0 {
            String::new()
        } else {
            "…".to_string()
        };
    }
    // ` · ` costs 3 cells; a tail below 2 cells (one char and its mark) says
    // nothing worth the space.
    let room = avail.saturating_sub(digit_cells + 3);
    if room >= 2 {
        let tail = tail_cut(core, remainder, room);
        let tail = furniture_survivor_recut(title, furniture, room, tail);
        // A width-2 glyph can leave the tail cut with a bare `…` — worth
        // nothing; the bare ordinal reads better than `2 · …`.
        if !tail.is_empty() && tail != "…" {
            return format!("{digits} · {tail}");
        }
    }
    digits
}

/// The family's cut of a clustered title in `room` cells (the pressure strip's tail
/// dialect): past the last word boundary of the shared head, behind the mark, when
/// that fits (`…~/aterm`); else the plain tail cut ([`truncate_title_tail`]).
/// `core` is the member's shed title ([`cut_core`]); `remainder` is the text past
/// that boundary, `""` when there is none.
fn family_tail(core: &str, remainder: &str, room: usize) -> String {
    if !remainder.is_empty() && strip_display_cells(remainder) < room {
        format!("…{remainder}")
    } else {
        truncate_title_tail(core, room)
    }
}

/// What a family of twins paints of its subject in `room` cells, after each ordinal
/// and alone on the active twin: the family cut ([`family_tail`]) where it keeps a
/// directory's name whole; where it would split a path component:
///
/// 1. the path's trailing components, whole, behind the mark, as many as fit
///    (`…work\api`, then `…api`);
/// 2. else the head of `core`, cut by [`truncate_title`] (`~\at…`).
///
/// A directory's name identifies the place, so it is never shown as a fragment.
/// Both `/` and `\` separate components; a subject with no separator keeps its
/// family cut (`…toml`). The head is `core`'s, so a prompt head (`user@host: `) is
/// never dropped without its mark.
fn twin_tail(core: &str, remainder: &str, room: usize) -> String {
    // Twins share their whole title, so `remainder` is past the title's last
    // blank. Where that blank is inside the last path component (`…/work/ünï 日本`,
    // `…\work\sub dir`) it is no boundary of the subject: the subject is the
    // whole core, cut at its separators below.
    let remainder = if !remainder.is_empty()
        && !remainder.contains(['/', '\\'])
        && core.contains(['/', '\\'])
    {
        ""
    } else {
        remainder
    };
    let family = family_tail(core, remainder, room);
    let subject = if remainder.is_empty() {
        core
    } else {
        remainder
    };
    if !subject.contains(['/', '\\']) || !splits_a_component(core, &family) {
        return family;
    }
    // The first separator leaves the longest tail, so this finds the most
    // whole components that fit behind the mark (`…` costs one cell).
    let components = subject
        .char_indices()
        .filter(|&(_, c)| matches!(c, '/' | '\\'))
        .map(|(at, c)| &subject[at + c.len_utf8()..])
        .find(|tail| !tail.is_empty() && strip_display_cells(tail) < room);
    match components {
        Some(tail) => format!("…{tail}"),
        None => truncate_title(core, room),
    }
}

/// Whether the tail cut `cut` (`…<text>`) of `core` starts inside a path component
/// or word (`…term` of `~\aterm`) rather than at its start. A cut with no mark, or
/// nothing behind it, splits nothing.
fn splits_a_component(core: &str, cut: &str) -> bool {
    let Some(text) = cut.strip_prefix('…').filter(|text| !text.is_empty()) else {
        return false;
    };
    let Some(before) = core.strip_suffix(text) else {
        return false;
    };
    !before.is_empty()
        && !text.starts_with(['/', '\\'])
        && !before.ends_with(|c: char| matches!(c, '/' | '\\') || c.is_whitespace())
}

/// Whether `text` is a head cut of `core` ([`truncate_title`]): a non-empty start
/// of it followed by the mark; not `core` whole, a tail cut, or the bare mark.
fn is_head_cut(core: &str, text: &str) -> bool {
    text != core
        && text.strip_suffix('…').is_some_and(|head| {
            !head.is_empty() && !head.starts_with('…') && core.starts_with(head)
        })
}

/// What one chip's tail cut is taken from: its title minus the shared ending
/// computed for it ([`distinct_chip_labels`]).
///
/// Every tail cut goes through here (the cluster arm with the family's common
/// suffix, the one-dialect flip with the loner's shared furniture), so a chip's
/// label does not depend on which path reached it. A tail cut of the raw title
/// would spend the window on the ending every sibling shares.
///
/// `shared` is the pairwise suffix in bytes. When it covers the whole title
/// (byte-identical twins) it is ignored and the whole title is the core.
fn cut_core(title: &str, shared: usize) -> &str {
    let shed = if shared >= title.len() { 0 } else { shared };
    &title[..title.len() - shed]
}

/// The characters a label may be cut after without splitting a name: the two
/// path separators, a space, and the punctuation names are joined with.
fn is_token_boundary(c: char) -> bool {
    matches!(c, '/' | '\\' | '-' | '_' | '.') || c.is_whitespace()
}

/// `shared` (a common suffix of `title`, in bytes) narrowed to start at a token
/// boundary ([`is_token_boundary`]), so the shed takes whole words: `…gui\src` and
/// `…cli\src` share `\src`, not `i\src`. A suffix covering the whole title is
/// returned as is ([`cut_core`] treats it as nothing to shed).
fn snap_shed(title: &str, shared: usize) -> usize {
    if shared == 0 || shared >= title.len() {
        return shared;
    }
    let tail = &title[title.len() - shared..];
    tail.char_indices()
        .find(|&(_, c)| is_token_boundary(c))
        .map_or(0, |(at, _)| tail.len() - at)
}

/// Whether a chip's subject is a filesystem path: `~`, `~/…`, `~\…`, `/…`, `C:\…`,
/// `\\host\…`. Its end, the directory, is its most telling part.
fn is_path_subject(subject: &str) -> bool {
    matches!(
        subject.as_bytes(),
        [b'~'] | [b'~', b'/' | b'\\', ..] | [b'/', ..] | [b'\\', b'\\', ..]
    ) || matches!(
        subject.as_bytes(),
        [drive, b':', b'/' | b'\\', ..] if drive.is_ascii_alphabetic()
    )
}

/// The label a cut path chip paints in `room` cells: its own tail ([`path_tail`]),
/// else the head of its directory's name or its position ([`path_fallback`]).
/// `subjects` are every tab's title in the window; `tab` indexes them.
///
/// Its kin are every other tab whose subject is a different path, seated or not,
/// cut or not, so the label is a function of the subject and the window's subjects
/// and holds still while the strip pages. A tab in the same directory is a twin,
/// told apart by position ([`ordinal_chip_label`]). Only paths are kin.
fn path_chip_label(tab: usize, subjects: &[&str], room: usize) -> String {
    let subject = subjects[tab];
    let kin = path_kin(subject, subjects);
    path_tail(subject, room, &kin).unwrap_or_else(|| path_fallback(tab, subject, room, &kin))
}

/// The paths a path chip's cut is told apart from: every subject in the window
/// that is a path and a different one ([`path_chip_label`]).
fn path_kin<'a>(subject: &str, subjects: &[&'a str]) -> Vec<&'a str> {
    subjects
        .iter()
        .copied()
        .filter(|other| *other != subject && is_path_subject(other))
        .collect()
}

/// A path twin's ordinal among a family of other paths: its position, with the
/// path's own cut ([`path_tail`]) behind ` · ` where the window affords one, bare
/// where not ([`family_tail`] would split the directory name). The number's meaning
/// and the bare-mark case are [`ordinal_chip_label`]'s.
fn path_ordinal_label(tab: usize, subjects: &[&str], avail: usize) -> String {
    let digits = (tab + 1).to_string();
    let digit_cells = strip_display_cells(&digits);
    if digit_cells > avail {
        return if avail == 0 {
            String::new()
        } else {
            "…".to_string()
        };
    }
    // ` · ` costs 3 cells, and a tail below 2 says nothing worth the space.
    let room = avail.saturating_sub(digit_cells + 3);
    let subject = subjects[tab];
    let tail = if strip_display_cells(subject) <= room {
        Some(subject.to_string())
    } else {
        path_tail(subject, room, &path_kin(subject, subjects))
    };
    match tail {
        Some(tail) if room >= 2 => format!("{digits} · {tail}"),
        _ => digits,
    }
}

/// Whether `text` ends `name` at the start of a token (`gui` ends `~\x\aterm-gui`,
/// not `~\x\argui`), so a chip labelled `…text` could be `name`'s.
fn ends_with_token(name: &str, text: &str) -> bool {
    name.strip_suffix(text)
        .is_some_and(|before| before.is_empty() || before.ends_with(is_token_boundary))
}

/// The cut a path `subject` takes when it does not fit `room` cells: the mark and
/// the longest tail that starts at a token boundary ([`is_token_boundary`]) and fits
/// (`…aterm-gui\src`, `…gui\src`, `…Temp`), never a name cut from inside.
///
/// `kin` are the other paths this chip must be told from ([`path_chip_label`]). A
/// tail a kin also ends with is passed over; when no tail both fits and differs,
/// the search reruns over the subject less the trailing whole components it shares
/// with a kin, shortest shed first, keeping the component that differs
/// (`…aterm-gui`, `…gui`).
///
/// Never a stub: a tail under three cells that does not begin a component says
/// nothing. Past every marked tail, the leaf alone, whole, where `…` and it do not
/// both fit. `None` when not even that fits and differs.
fn path_tail(subject: &str, room: usize, kin: &[&str]) -> Option<String> {
    let whole = subject.len();
    let mut sheds: Vec<usize> = kin
        .iter()
        .filter_map(|other| {
            let shared = &subject[whole - common_suffix_bytes(subject, other)..];
            shared.find(['/', '\\']).map(|at| shared.len() - at)
        })
        .filter(|&shed| shed < whole)
        .collect();
    sheds.sort_unstable();
    sheds.dedup();
    let bodies: Vec<(&str, &str)> = std::iter::once(0)
        .chain(sheds)
        .map(|shed| subject.split_at(whole - shed))
        .collect();
    // A text a kin also ends with — whole, or less the same shed — names
    // neither of them.
    let names_kin = |text: &str, shed: &str| {
        kin.iter().any(|other| {
            ends_with_token(other, text)
                || (!shed.is_empty()
                    && other
                        .strip_suffix(shed)
                        .is_some_and(|rest| ends_with_token(rest, text)))
        })
    };
    for &(body, shed) in &bodies {
        for (at, c) in body.char_indices() {
            if !is_token_boundary(c) {
                continue;
            }
            let text = &body[at + c.len_utf8()..];
            let component = matches!(c, '/' | '\\');
            let Some(first) = text.chars().next() else {
                continue;
            };
            // Skip a boundary right after a boundary (`a--b`, `x\ \y`): the tail
            // would start with punctuation. A component may start with one
            // (`.claude`), a word inside one may not.
            if (!component && is_token_boundary(first))
                || matches!(first, '/' | '\\')
                || first.is_whitespace()
            {
                continue;
            }
            let cells = strip_display_cells(text);
            if cells + 1 > room || (cells < 3 && !component) {
                continue;
            }
            if !names_kin(text, shed) {
                return Some(format!("…{text}"));
            }
        }
    }
    bodies.iter().find_map(|&(body, shed)| {
        let leaf = body.rsplit(['/', '\\']).next()?;
        (!leaf.is_empty()
            && leaf.len() < body.len()
            && strip_display_cells(leaf) <= room
            && !names_kin(leaf, shed))
        .then(|| leaf.to_string())
    })
}

/// What a cut path chip paints when no tail or leaf both fits and differs
/// ([`path_tail`]): the head of its directory's own name (`System…`), the same
/// answer whichever branch labels the chip.
///
/// A head keeping fewer than three of the name's cells, or one a kin's directory
/// name also reads as, names nothing; the chip then takes its bare position like a
/// twin ([`ordinal_chip_label`]), or the lone mark where the digits do not fit.
fn path_fallback(tab: usize, subject: &str, room: usize, kin: &[&str]) -> String {
    let leaf = |path: &str| {
        path.rsplit(['/', '\\'])
            .next()
            .unwrap_or_default()
            .to_string()
    };
    let name = leaf(subject);
    let head = truncate_title(&name, room);
    let (kept, marked) = match head.strip_suffix('…') {
        Some(kept) => (kept, true),
        None => (head.as_str(), false),
    };
    let reads_as_kin = kin.iter().any(|other| {
        let other = leaf(other);
        if marked {
            other.starts_with(kept)
        } else {
            other == kept
        }
    });
    if !kept.is_empty()
        && strip_display_cells(kept) >= strip_display_cells(&name).min(3)
        && !reads_as_kin
    {
        return head;
    }
    let digits = (tab + 1).to_string();
    if room == 0 {
        String::new()
    } else if strip_display_cells(&digits) <= room {
        digits
    } else {
        "…".to_string()
    }
}

/// Byte length of the longest common prefix of `a` and `b`, aligned to char
/// boundaries; [`common_suffix_bytes`]'s mirror.
fn common_prefix_bytes(a: &str, b: &str) -> usize {
    let mut n = 0;
    let mut ai = a.chars();
    let mut bi = b.chars();
    while let (Some(x), Some(y)) = (ai.next(), bi.next()) {
        if x != y {
            break;
        }
        n += x.len_utf8();
    }
    n
}

/// Byte length of the longest common suffix of `a` and `b`, compared char by char
/// so multi-byte chars are shared whole or not at all.
fn common_suffix_bytes(a: &str, b: &str) -> usize {
    let mut n = 0;
    let mut ai = a.chars().rev();
    let mut bi = b.chars().rev();
    while let (Some(x), Some(y)) = (ai.next(), bi.next()) {
        if x != y {
            break;
        }
        n += x.len_utf8();
    }
    n
}

/// The strip in the UI font: a pixel-space band composited over the cell strip
/// (Windows and Linux, where the in-grid strip is the window's only chrome).
///
/// `RenderCell` has no face selector, so the band's text and surfaces (band fill,
/// rounded cards, hover wash, the `+` button) become pixels. [`raster_band`]
/// rasterizes the strip with the settings tray's `DrawPrim` rasterizer into one
/// straight-alpha RGBA image (one GPU cache entry; not a `FreeSprite` band, whose
/// atlas the effects engine owns per frame) and returns per-row [`ImageRef`] slices
/// the splice lays over the strip. The canvas is the full optical band
/// (`band_top_px` of chrome lip plus the strip's rows), placed by
/// [`aterm_core::grid::extra::ImageData::band_lift_px`] so cards centre in the band.
/// At `z >= 0` it draws over the cells' glyphs and under line decorations, so the
/// seam hairline still paints over it.
///
/// `layout_segments` stays the geometry authority: this converts the same segments
/// to pixels via `cell_w`, so paint and hit-testing agree. Mono ink is suppressed
/// only by image coverage, so a segment this painter cannot render falls back to the
/// cell paint by staying uncovered:
///   * the inline rename field (the find bar's cell-based machinery);
///   * a solo title band (macOS-only policy; kept total here);
///   * any title with a char neither the UI face nor the chrome cascade covers.
///
/// The label is centred in the band the viewer sees, from live metrics, so the
/// synthetic head (`App::synthetic_strip_head_px`, config `tab_band_height`) needs
/// nothing here: it is inside `band_top_px`.
#[cfg(any(windows, target_os = "linux"))]
pub(crate) mod pixel_band {
    use super::*;
    use crate::tray_raster::UnicodeChars;
    use crate::type_scale::TypeStep;
    use crate::widget::{DrawPrim, TextFace, TextWeight, rgba, text_prim};

    /// The band owns its surfaces: the raster paints the band fill, rounded cards,
    /// the hover wash and the `+` button, as an opaque bar with cards centred
    /// between the band top and the seam, px gutters and rounded corners.
    ///
    /// This module compiles exactly where the strip uses chip cards
    /// ([`STRIP_CHIP_CARDS`]); otherwise a declined raster would expose square
    /// whole-cell chips under rounded cards. Asserted at compile time below.
    const _: () = assert!(
        STRIP_CHIP_CARDS,
        "the pixel band paints chip-card surfaces; the cell lane beneath it must \
         speak the same language (STRIP_CHIP_CARDS)"
    );

    /// Device-pixel geometry of the strip band for one window (mixed-DPI: every
    /// term is this window's own).
    #[derive(Clone, Copy, PartialEq, Debug)]
    pub(crate) struct BandGeometry {
        /// Grid columns (the strip row's width in cells).
        pub cols: usize,
        /// Cell width in device px.
        pub cell_w: usize,
        /// Cell height in device px.
        pub cell_h: usize,
        /// Rows the strip occupies (`tab_strip_rows`).
        pub strip_rows: usize,
        /// Band pixels above the grid (`pad_top + head`, less the stats Deck
        /// over the head's top: `App::strip_band_top_px`): the lip `ChromeBleed`
        /// paints in the band tone. Part of the optical band and of the canvas
        /// (via the chrome-band lift).
        pub band_top_px: usize,
        /// The window's scale factor (drives the label size).
        pub scale: f32,
        /// Canvas y (device px from the optical band's top) of the seam's top
        /// row. On Windows ([`STRIP_WINDOWS_PAINT`]) the canvas's last
        /// [`strip_seam_px`] rows, `band_top_px + strip_rows·cell_h −
        /// strip_seam_px(scale)`, where the strip's rail ([`strip_seam_rail`])
        /// runs too, and the raster draws the rule there; on Linux the seam
        /// underline's top row, `band_top_px + (strip_rows − 1)·cell_h +
        /// DecoMetrics::underline_y` through the renderer's deco law
        /// (`Backend::deco_metrics_for`), which the cell lane stamps over the
        /// raster ([`seal_strip_bottom`]). Cards are centred in
        /// `[0, seam_top_px)` and kept clear of the rule; `None` (test fixtures)
        /// clears a hairline off the canvas bottom and draws no rule. On Windows
        /// it sits [`Self::foot_px`] above the canvas's last row.
        pub seam_top_px: Option<usize>,
        /// The band's FOOT on Windows: the canvas's last rows, under the seam,
        /// in the terminal's ground ([`StripColors::ground`]), so the grid's
        /// first row keeps air under the rule ([`strip_foot_px`]). 0 elsewhere
        /// and on a compact band.
        pub foot_px: usize,
    }

    /// Everything the band raster depends on. The caller caches the result keyed
    /// on the strip fingerprint + [`BandGeometry`] + the chrome-font epoch
    /// ([`crate::tray_raster::strip_band_font_epoch`]), so this runs only on a
    /// strip rebuild.
    pub(crate) struct BandInput<'a> {
        pub segments: &'a [TabSegment],
        pub titles: &'a [String],
        pub metadata: &'a [TabStripMetadata],
        pub paint: StripPaint<'a>,
        pub active: usize,
        /// The header's palette, its authored `active_tab_color` included.
        pub palette: ChromePalette,
        pub geometry: BandGeometry,
        /// What lies under the seam this frame ([`StripFloor`]): the selected
        /// tab's surface and the foot's tone.
        pub floor: StripFloor,
    }

    impl BandInput<'_> {
        /// The selected tab, clamped into range exactly as `layout_segments` and
        /// [`distinct_chip_labels`] clamp it, so every band reader (paint,
        /// separators, labels, fit) agrees on which tab is selected.
        fn selected(&self) -> usize {
            self.active.min(self.titles.len().saturating_sub(1))
        }
    }

    /// Rasterize the strip band and return one `Vec<(col, ImageRef)>` per strip row,
    /// sorted by column (the renderer binary-searches). `None` means paint the
    /// strip with cells (no UI face yet, degenerate geometry, or nothing qualified).
    ///
    /// `icon_images` is the cell painter's icon/status list for this build: entries
    /// under a fallback segment are kept; the rest are dropped because the band
    /// draws those marks itself.
    pub(crate) fn raster_band(
        input: &BandInput<'_>,
        icon_images: &[(usize, ImageRef)],
    ) -> Option<Vec<Vec<(usize, ImageRef)>>> {
        let geometry = input.geometry;
        let (cols, cell_w, cell_h) = (geometry.cols, geometry.cell_w, geometry.cell_h);
        if cols == 0
            || cell_w == 0
            || cell_h == 0
            || geometry.strip_rows == 0
            || input.segments.is_empty()
            // No UI face yet (before `set_chrome_fonts`, and in tests that never
            // install one): the whole strip stays cells.
            || !crate::tray_raster::strip_band_ui_ready()
        {
            return None;
        }
        let img_w = cols.checked_mul(cell_w)?;
        // The raster canvas is the whole optical band; `ImageData::band_lift_px`
        // makes both renderers place its top row at the window's top edge.
        let cells_h = geometry.strip_rows.checked_mul(cell_h)?;
        let lift = geometry.band_top_px;
        let img_h = cells_h.checked_add(lift)?;
        if img_w > usize::from(u16::MAX) || img_h > usize::from(u16::MAX) {
            return None;
        }
        // The band's one reading of the selection ([`BandInput::selected`]),
        // resolved before anything paints or measures.
        let active = input.selected();
        let colors = strip_colors_for_on(input.palette, input.floor);
        let scale = if geometry.scale.is_finite() && geometry.scale > 0.0 {
            geometry.scale
        } else {
            1.0
        };
        // The optical band: the lip above the grid plus the cell rows.
        let band_h = (geometry.band_top_px + cells_h) as f32;
        let label_px = band_label_px(band_h, scale);
        // The resolved design: every px the card row is drawn from.
        let design = BandDesign::resolve(&geometry, img_h, label_px);
        // Cap-centred baseline in the design's content region, from the window's
        // top edge to the seam rule ([`BandDesign::resolve`]); the clamp is
        // defensive. Lowering the ascender floor instead would clip `l`/`d`/`\`.
        let baseline = design.baseline;
        // The optical centre every non-text mark aligns to (the label's cap-box
        // midline, so ✕ / + / icons and the text read as one centred row).
        let cy = baseline - label_px * 0.35;
        let cw = cell_w as f32;
        // The variable UI face at wght 600 for the active label (Segoe UI Variable
        // Semibold), drawn by the varied rasterizer into an overlay after the
        // `DrawPrim` pass; `None` falls back to the static `UiBold` slot. Parsed
        // once per band.
        let variable = crate::tray_raster::strip_band_variable_semibold();
        let variable_pen = variable.as_ref().and_then(VariablePen::new);

        let mut prims: Vec<DrawPrim> = Vec::new();
        let mut overlays: Vec<LabelOverlay> = Vec::new();
        // Per-segment fallback ranges (half-open cols) left to the cell painter.
        let mut fallback: Vec<(u16, u16)> = Vec::new();
        let mut drew_any = false;

        // The canvas is an opaque bar, band tone edge to edge, with every card
        // painted here in px, so cards can round their corners over a clean
        // ground. Uncovered (fallback) columns still show the cell lane's cards.
        prims.push(DrawPrim::Panel {
            x: 0.0,
            y: 0.0,
            w: img_w as f32,
            h: img_h as f32,
            radius: 0.0,
            fill: rgba(colors.band_bg, 255),
        });
        // The seam on the canvas's last rows, where the strip's rail runs in the
        // gutters ([`strip_seam_rail`]): this opaque canvas covers the rail in
        // the grid columns, so it draws the same rule itself, and under it the
        // band's foot ([`BandGeometry::foot_px`]) in the floor's tone
        // ([`StripColors::foot`]). On Linux the cell lane stamps its underline
        // seam over this raster ([`seal_strip_bottom`]).
        if STRIP_WINDOWS_PAINT
            && let (Some(seam_top), Some(seam)) = (geometry.seam_top_px, colors.seam)
            && seam_top < img_h
        {
            let foot_top = img_h.saturating_sub(geometry.foot_px).max(seam_top);
            prims.push(DrawPrim::Panel {
                x: 0.0,
                y: seam_top as f32,
                w: img_w as f32,
                h: (foot_top - seam_top) as f32,
                radius: 0.0,
                fill: rgba(seam, 255),
            });
            if foot_top < img_h {
                prims.push(DrawPrim::Panel {
                    x: 0.0,
                    y: foot_top as f32,
                    w: img_w as f32,
                    h: (img_h - foot_top) as f32,
                    radius: 0.0,
                    fill: rgba(colors.foot, 255),
                });
            }
        }
        // The selected tab opens through the seam where it IS what lies under
        // it ([`StripFloor`]: the grid's ground, the pane headers, a native
        // page); over a band it cannot take, an `active_tab_color`, or High
        // Contrast's selection, it stands on the seam instead.
        let opens = design.tabs && colors.active_bg == colors.foot;
        // The seam's thickness, which a selected tab's rim takes, and the
        // canvas row below the rule.
        let seam_rows = strip_seam_px(scale).max(1);
        let seam_bottom = geometry
            .seam_top_px
            .map(|top| (top + seam_rows).min(img_h) as f32);
        // Every tab segment in order, `(start, end, lit, band)`: lit when it is
        // selected or hovered, band when this raster draws it rather than the
        // cell lane. The hairlines between quiet neighbours read it.
        let mut drawn_tabs: Vec<(u16, u16, bool, bool)> = Vec::new();

        // Resolve all chip labels in one pass (the cell painter's rule), and the
        // pixel fit with them ([`resolve_band_labels`]): a second truncation done
        // per chip would undo the first pass. The fit measures with the face the
        // pen will draw, which depends on whether the selection is semibold.
        let wants_semibold = design.active_semibold;
        let mut labels = resolve_band_labels(input, active, cols, cw, label_px, wants_semibold);
        for seg in input.segments {
            let end = seg.end_col.min(cols as u16);
            if seg.start_col >= end {
                continue;
            }
            let is_active = matches!(seg.kind, TabHit::Select(i) if i == active);
            let is_hovered = !is_active
                && !seg.solo
                && matches!(seg.kind, TabHit::Select(i) if input.paint.hovered == Some(i));
            match seg.kind {
                // The needs pill's regions go to the cell painter, like solo:
                // the cell lane draws the pill ([`paint_strip_pill`]) by the
                // layout its hit regions are cut from, so the band never
                // re-measures it in another face.
                TabHit::Pill(_) => {
                    fallback.push((seg.start_col, end));
                }
                TabHit::Select(i) if seg.solo => {
                    // Solo is macOS policy ([`SOLO_TITLE_BAND`]), unreachable here;
                    // kept total: the cell painter owns it.
                    let _ = i;
                    fallback.push((seg.start_col, end));
                }
                TabHit::Select(i) => {
                    if input.paint.rename.is_some_and(|edit| edit.tab == i) {
                        // The inline rename well (field, caret, ‹› markers, IME
                        // preedit splice) stays the cell machinery.
                        fallback.push((seg.start_col, end));
                        drawn_tabs.push((seg.start_col, end, is_active || is_hovered, false));
                        continue;
                    }
                    let item = input.metadata.get(i).copied();
                    let layout = band_content_layout(seg, item, end);
                    let avail = layout.title_end.saturating_sub(layout.title_start);
                    // Resolved for the whole strip above and final: the cell
                    // painter's truncation, the pixel fit and the distinctness
                    // repair. This loop never re-cuts it, except the active chip's
                    // glyph-level fit ([`fit_labels_distinctly`]).
                    let label: String =
                        labels.get_mut(i).and_then(Option::take).unwrap_or_default();
                    // The face the label is set in: semibold only for the
                    // selected tab, where the design says the selection earns
                    // it. Its East Asian faces are that weight's.
                    let label_face = if is_active && design.active_semibold {
                        TextFace::UiBold
                    } else {
                        TextFace::Ui
                    };
                    // A title the UI stack cannot draw whole is drawn here too,
                    // its colour emoji inline ([`emoji_label`]), in the tab like
                    // every other; only a cluster that is no colour emoji and
                    // no face maps leaves the title to the cell lane (a CJK
                    // title is not one: the faces it needs at the label's
                    // weight are parsed for it, `strip_band_run_coverable_in`).
                    let pieces =
                        if crate::tray_raster::strip_band_run_coverable_in(label_face, &label) {
                            None
                        } else {
                            let title = input.titles.get(i).map_or("", String::as_str);
                            // Windows only (STRIP_WINDOWS_PAINT): Linux keeps its
                            // strip as it was until that lane is reviewed.
                            let Some(pieces) = STRIP_WINDOWS_PAINT
                                .then(|| emoji_label(title, label_px, label_face))
                                .flatten()
                            else {
                                fallback.push((seg.start_col, end));
                                drawn_tabs.push((
                                    seg.start_col,
                                    end,
                                    is_active || is_hovered,
                                    false,
                                ));
                                continue;
                            };
                            Some(pieces)
                        };
                    // This tab's card: a rounded rect inset from the segment's cell
                    // span by the design's gutter, centred between the band top and
                    // the seam. The whole segment stays the hit region.
                    {
                        let card_bg = if is_active {
                            Some(colors.active_bg)
                        } else if is_hovered {
                            Some(colors.hover_bg)
                        } else if design.quiet_cards {
                            Some(colors.chip_bg)
                        } else {
                            None
                        };
                        if let Some(bg) = card_bg {
                            let x0 = f32::from(seg.start_col).mul_add(cw, design.gap_h);
                            let x1 = f32::from(end).mul_add(cw, -design.gap_h);
                            if x1 > x0 && design.tabs {
                                // A tab: its top corners round, it stands on the
                                // seam, and the selected one runs on through the
                                // seam and the foot into the grid. Drawn a radius
                                // past its foot under a clip, so only the top
                                // corners show.
                                let foot = if is_active && opens {
                                    img_h as f32
                                } else {
                                    design.card_bot
                                };
                                prims.push(DrawPrim::ClipPush {
                                    x: x0,
                                    y: 0.0,
                                    w: x1 - x0,
                                    h: foot,
                                });
                                prims.push(DrawPrim::Panel {
                                    x: x0,
                                    y: design.card_top,
                                    w: x1 - x0,
                                    h: foot - design.card_top + design.radius,
                                    radius: design.radius,
                                    fill: rgba(bg, 255),
                                });
                                prims.push(DrawPrim::ClipPop);
                                // A selected tab its fill alone would leave
                                // faint on the band ([`TAB_RIM_BELOW`]) wears
                                // the seam round its sides and top: an open
                                // tab's sides run down through the seam rows
                                // to meet the rule either side of it.
                                if is_active && let Some(rim) = colors.active_rim {
                                    let t = seam_rows as f32;
                                    let bottom = if opens {
                                        seam_bottom.unwrap_or(design.card_bot)
                                    } else {
                                        design.card_bot
                                    };
                                    prims.push(DrawPrim::ClipPush {
                                        x: x0,
                                        y: 0.0,
                                        w: x1 - x0,
                                        h: bottom,
                                    });
                                    prims.push(DrawPrim::Stroke {
                                        x: x0 + t * 0.5,
                                        y: design.card_top + t * 0.5,
                                        w: x1 - x0 - t,
                                        h: foot - design.card_top + design.radius,
                                        radius: (design.radius - t * 0.5).max(0.0),
                                        width: t,
                                        color: rgba(rim, 255),
                                    });
                                    prims.push(DrawPrim::ClipPop);
                                }
                            } else if x1 > x0 {
                                prims.push(DrawPrim::Panel {
                                    x: x0,
                                    y: design.card_top,
                                    w: x1 - x0,
                                    h: design.card_bot - design.card_top,
                                    radius: design.radius,
                                    fill: rgba(bg, 255),
                                });
                            }
                        }
                        drawn_tabs.push((seg.start_col, end, is_active || is_hovered, true));
                    }
                    // The tab's load meter (design §12.4), a capsule above the
                    // card's foot: the Tab ink on the selected tab, the aterm
                    // ink at 70 % on the others, solved on what each sits on.
                    if let Some(item) = item {
                        let x0 = f32::from(seg.start_col).mul_add(cw, design.gap_h);
                        let x1 = f32::from(end).mul_add(cw, -design.gap_h);
                        if let Some(len) = chip_load_len_pt((x1 - x0) / scale, item.load) {
                            let ground = if is_active {
                                colors.active_bg
                            } else {
                                colors.band_bg
                            };
                            let inks =
                                crate::stats_deck::cached_inks(ground, colors.fg, colors.accent);
                            let fill = if is_active {
                                rgba(inks.tab, 255)
                            } else {
                                rgba(inks.aterm, 178)
                            };
                            let h = (CHIP_LOAD_H_PT * scale).max(1.0);
                            prims.push(DrawPrim::Panel {
                                x: CHIP_LOAD_INSET_PT.mul_add(scale, x0).round(),
                                y: (CHIP_LOAD_LIFT_PT + CHIP_LOAD_H_PT)
                                    .mul_add(-scale, design.card_bot)
                                    .round(),
                                w: (len * scale).round().max(1.0),
                                h,
                                radius: h * 0.5,
                                fill,
                            });
                        }
                    }
                    // Inks follow the surface each label sits on (selected card,
                    // hover wash, quiet chip, or bare band), in `label_face`.
                    let (ink, face) = {
                        let ink = if is_active {
                            colors.active_fg
                        } else if is_hovered {
                            colors.hover_fg
                        } else if design.quiet_cards {
                            colors.chip_fg
                        } else {
                            colors.inactive_fg
                        };
                        (ink, label_face)
                    };
                    // No leading `│` separator: cards and gutters carry the
                    // structure.
                    if let (Some(icon_start), Some(kind)) =
                        (layout.icon_start, item.and_then(|it| it.icon))
                    {
                        // The icon takes the label's ink, so it is never louder than
                        // the title.
                        draw_icon_prims(
                            &mut prims,
                            tab_icon_primitives(kind),
                            (f32::from(icon_start) + f32::from(ICON_COLS) * 0.5) * cw,
                            cy,
                            ((2.0 * cw).min(band_h) * 0.85).max(6.0),
                            ink,
                        );
                    }
                    if let (Some(item), Some(col)) = (item, layout.status_col) {
                        // One ink per surface (see the icon above), unless a
                        // need's mark wears the border's tone.
                        let color = status_mark_ink(item, &colors, input.palette, ink);
                        // The marks fill about a third of their 16-unit design box,
                        // so on Windows ([`STRIP_WINDOWS_PAINT`]) the box is sized
                        // off the label (21 px at 100%): a busy ring about 5 px
                        // across, where a cell-sized box drew it at under 3 px,
                        // dust beside the ✕. It still sits between the gap cells
                        // that flank the status cell. Linux keeps the cell box.
                        let mark_box = if STRIP_WINDOWS_PAINT {
                            label_px * 1.6
                        } else {
                            cw * 1.2
                        };
                        draw_icon_prims(
                            &mut prims,
                            &status_primitives(item),
                            (f32::from(col) + 0.5) * cw,
                            cy,
                            mark_box.min(band_h * 0.8).max(5.0),
                            color,
                        );
                    }
                    // The title, clipped to its span so it never bleeds into the
                    // status canvas, the ✕ or the next chip, placed by
                    // [`label_x`]: leading on Windows, centred elsewhere.
                    let x0 = f32::from(layout.title_start) * cw;
                    let span_px = f32::from(avail) * cw - 1.0;
                    if let Some(pieces) = pieces {
                        if span_px > 1.0 {
                            draw_emoji_label(
                                &mut prims,
                                &mut overlays,
                                &pieces,
                                EmojiLabelAt {
                                    x0,
                                    span_px,
                                    baseline,
                                    cy,
                                    label_px,
                                    height: img_h,
                                },
                                face,
                                variable_pen
                                    .as_ref()
                                    .filter(|_| is_active && wants_semibold),
                                ink,
                                colors.active_bg,
                            );
                        }
                    } else if span_px > 1.0 && !label.trim().is_empty() {
                        // The active label through the wght axis when the host has a
                        // variable UI face and the lane wants semibold. The whole
                        // label must shape on that face first; any char its cmap
                        // lacks sends the whole run to the static `UiBold` cascade
                        // below (per-char fallback), never a mixed-weight word or a
                        // fit that drops the uncoverable char. Only a run that
                        // shaped whole is fitted, at the glyph level.
                        let varied = if is_active && wants_semibold {
                            variable_pen.as_ref().and_then(|pen| {
                                let mut run = pen.shape(&label, label_px)?;
                                // The pen's advances can overrun the static
                                // semibold's the cut was measured with; a cut
                                // that keeps its end (a path's, a command's
                                // file) is re-cut by its own rule against this
                                // pen, never head-cut by `fit`.
                                if run.width > span_px && cut_keeps_its_end(&label) {
                                    let width = |s: &str| {
                                        pen.shape(s, label_px).map_or(f32::INFINITY, |r| r.width)
                                    };
                                    run = pen.shape(
                                        &fit_label(label.clone(), span_px, width),
                                        label_px,
                                    )?;
                                }
                                let run = pen.fit(run, span_px, label_px)?;
                                let x = label_x(x0, span_px, run.width);
                                Some(variable_label_overlay(
                                    &run,
                                    x,
                                    baseline,
                                    img_h,
                                    span_px,
                                    ink,
                                    colors.active_bg,
                                ))
                            })
                        } else {
                            None
                        };
                        match varied {
                            Some(overlay) => overlays.push(overlay),
                            None => {
                                // No second fit: the string is already cut to this
                                // span with this face ([`resolve_band_labels`]), and a
                                // per-chip fit could make two chips identical.
                                // Placed by the same measure the pen draws with.
                                let width =
                                    crate::tray_raster::ui_text_width_for(face, &label, label_px);
                                let x = label_x(x0, span_px, width);
                                prims.push(DrawPrim::ClipPush {
                                    x: x0,
                                    y: 0.0,
                                    w: span_px,
                                    h: img_h as f32,
                                });
                                prims.push(text_prim(
                                    x,
                                    baseline,
                                    label,
                                    TypeStep::Body.px(label_px),
                                    TextWeight::Regular,
                                    face,
                                    rgba(ink, 255),
                                ));
                                prims.push(DrawPrim::ClipPop);
                            }
                        }
                    }
                    // The close mark: two round-capped strokes, not a font's ✕.
                    // Revealed on hover and resident on the selected card.
                    if let Some(cx) = seg.close_col
                        && (input.paint.hovered == Some(i) || is_active)
                    {
                        let center = (f32::from(cx) + 0.5) * cw;
                        let r = (label_px * 0.26).max(2.0);
                        let w = (label_px / 9.0).max(1.0);
                        let ink = rgba(ink, 255);
                        prims.push(DrawPrim::Line {
                            x1: center - r,
                            y1: cy - r,
                            x2: center + r,
                            y2: cy + r,
                            width: w,
                            color: ink,
                        });
                        prims.push(DrawPrim::Line {
                            x1: center - r,
                            y1: cy + r,
                            x2: center + r,
                            y2: cy - r,
                            width: w,
                            color: ink,
                        });
                    }
                    drew_any = true;
                }
                TabHit::NewTab => {
                    // The `+`: code-native strokes, crisp at every DPI.
                    //
                    // The button is a fixed square quiet card centred on the
                    // segment's true px centre, so glyph, card and hit region share
                    // one centre, sized and aligned to the card row. Under the
                    // pointer it takes the band's hover wash with its own
                    // full-strength ink ([`StripColors::button_hover_fg`]).
                    let (center, ink) = {
                        let seg0 = f32::from(seg.start_col) * cw;
                        let seg1 = f32::from(end) * cw;
                        let center = (seg0 + seg1) * 0.5;
                        let card_h = design.card_bot - design.card_top;
                        let hot = input.paint.new_tab_hovered;
                        if design.tabs {
                            // A bare `+` on a tab row; under the pointer a
                            // rounded square of the hover wash centred on it.
                            let side = (label_px * 1.85)
                                .round()
                                .min(card_h)
                                .min(seg1 - seg0 - 2.0 * design.gap_h);
                            if hot && side > 4.0 {
                                prims.push(DrawPrim::Panel {
                                    x: center - side * 0.5,
                                    y: (cy - side * 0.5)
                                        .clamp(design.card_top, design.card_bot - side),
                                    w: side,
                                    h: side,
                                    radius: (4.0 * scale).round().min(side * 0.5),
                                    fill: rgba(colors.hover_bg, 255),
                                });
                            }
                        } else {
                            let side = card_h.min(seg1 - seg0 - 2.0 * design.gap_h);
                            if design.quiet_button && side > 4.0 {
                                prims.push(DrawPrim::Panel {
                                    x: center - side * 0.5,
                                    y: design.card_top + (card_h - side) * 0.5,
                                    w: side,
                                    h: side,
                                    radius: design.radius.min(side * 0.5),
                                    fill: rgba(
                                        if hot { colors.hover_bg } else { colors.chip_bg },
                                        255,
                                    ),
                                });
                            }
                        }
                        let ink = if hot && (design.quiet_button || design.tabs) {
                            colors.button_hover_fg
                        } else if design.quiet_button {
                            colors.chip_button_fg
                        } else {
                            colors.fg
                        };
                        (center, rgba(ink, 255))
                    };
                    let r = (label_px * 0.30).max(2.5);
                    let w = (label_px / 8.5).max(1.2);
                    prims.push(DrawPrim::Line {
                        x1: center - r,
                        y1: cy,
                        x2: center + r,
                        y2: cy,
                        width: w,
                        color: ink,
                    });
                    prims.push(DrawPrim::Line {
                        x1: center,
                        y1: cy - r,
                        x2: center,
                        y2: cy + r,
                        width: w,
                        color: ink,
                    });
                    drew_any = true;
                }
                TabHit::Update => {
                    // The `↻` update alert keeps its terminal-face glyph (the Mono
                    // arm of the tray pen), vertically centred on the update CTA's
                    // own card.
                    {
                        let x0 = f32::from(seg.start_col).mul_add(cw, design.gap_h);
                        let x1 = f32::from(end).mul_add(cw, -design.gap_h);
                        if x1 > x0 {
                            prims.push(DrawPrim::Panel {
                                x: x0,
                                y: design.card_top,
                                w: x1 - x0,
                                h: design.card_bot - design.card_top,
                                radius: design.radius,
                                fill: rgba(colors.update_bg, 255),
                            });
                        }
                    }
                    // Centred on the card just painted (the full segment span,
                    // gutter-inset symmetrically).
                    let x = {
                        let seg0 = f32::from(seg.start_col) * cw;
                        let seg1 = f32::from(end) * cw;
                        (seg0 + seg1) * 0.5 - label_px * 0.3
                    };
                    prims.push(text_prim(
                        x,
                        baseline,
                        "\u{21bb}".to_string(),
                        TypeStep::Body.px(label_px),
                        TextWeight::Bold,
                        TextFace::Mono,
                        rgba(colors.update_fg, 255),
                    ));
                    drew_any = true;
                }
                // The trailing version label in the UI face at the chip titles' size
                // and baseline, right-aligned `pad` px inside the slot: the face, size
                // and pad [`version_slot_cols`] measured the slot with, so the label
                // fills its hit region. Each state's card and ink are
                // [`version_label_material`]'s. A label the UI face and the chrome
                // cascade cannot cover goes to the cell painter, like any such title.
                TabHit::Version { rung, attention } => {
                    let label =
                        crate::menu::strip_version_label(rung, attention).unwrap_or_default();
                    if label.is_empty() || !crate::tray_raster::strip_band_run_coverable(&label) {
                        fallback.push((seg.start_col, end));
                        continue;
                    }
                    let seg0 = f32::from(seg.start_col) * cw;
                    let seg1 = f32::from(end) * cw;
                    let (card, ink) =
                        version_label_material(&colors, attention, input.paint.version_hovered);
                    if let Some(bg) = card {
                        let x0 = seg0 + design.gap_h;
                        let x1 = seg1 - design.gap_h;
                        if x1 > x0 {
                            prims.push(DrawPrim::Panel {
                                x: x0,
                                y: design.card_top,
                                w: x1 - x0,
                                h: design.card_bot - design.card_top,
                                radius: design.radius,
                                fill: rgba(bg, 255),
                            });
                        }
                    }
                    let width =
                        crate::tray_raster::ui_text_width_for(TextFace::Ui, &label, label_px);
                    let x = (seg1 - version_pad_px(scale) - width).max(seg0);
                    prims.push(DrawPrim::ClipPush {
                        x: seg0,
                        y: 0.0,
                        w: seg1 - seg0,
                        h: img_h as f32,
                    });
                    prims.push(text_prim(
                        x,
                        design.baseline,
                        label,
                        TypeStep::Body.px(label_px),
                        TextWeight::Regular,
                        TextFace::Ui,
                        rgba(ink, 255),
                    ));
                    prims.push(DrawPrim::ClipPop);
                    drew_any = true;
                }
                TabHit::Caption(button) => {
                    draw_caption_button(
                        &mut prims,
                        button,
                        (f32::from(seg.start_col) * cw, f32::from(end) * cw),
                        &design,
                        scale,
                        input.paint.caption_hovered == Some(button),
                        &colors,
                    );
                    drew_any = true;
                }
                TabHit::Close(_) => {}
            }
        }
        // A tab row's hairline between two quiet neighbours (WinUI's tab
        // separator): where neither side is selected or hovered, a seam-ink line
        // on the boundary, centred on the labels, so a row of bare titles still
        // reads as separate tabs — whichever lane draws each label
        // ([`hairline_x`]).
        if design.tabs
            && let Some(seam) = colors.seam
        {
            let tab_h = design.card_bot - design.card_top;
            let len = (16.0 * scale).round().min(tab_h * 0.6).max(4.0);
            let w = scale.floor().max(1.0);
            let y = (cy - len * 0.5).clamp(design.card_top, design.card_bot - len);
            for pair in drawn_tabs.windows(2) {
                let ((_, a_end, a_lit, a_band), (b_start, _, b_lit, b_band)) = (pair[0], pair[1]);
                if a_lit || b_lit || a_end != b_start {
                    continue;
                }
                let Some(x) = hairline_x(f32::from(a_end) * cw, w, a_band, b_band) else {
                    continue;
                };
                prims.push(DrawPrim::Panel {
                    x,
                    y: y.round(),
                    w,
                    h: len,
                    radius: 0.0,
                    fill: rgba(seam, 255),
                });
            }
        }
        // The window's edge marks ([`strip_overflow`]), drawn here on the same
        // optical centre (`cy`) as every other piece of furniture; the cell lane
        // can only place them on the cell baseline. Code-native strokes like the
        // `✕` and `+`. The cell lane still writes its glyph on these columns for
        // lanes this raster does not reach; here the band covers it.
        for (col, mark) in strip_overflow(input.segments, input.titles.len()).columns() {
            let center = (f32::from(col) + 0.5) * cw;
            let arm_y = (label_px * 0.30).max(2.5);
            // The apex points at the edge the strip continues past; the arms reach
            // about half as far across as down.
            let apex = if mark == STRIP_OVERFLOW_LEFT {
                -arm_y * 0.5
            } else {
                arm_y * 0.5
            };
            let ink = rgba(colors.inactive_fg, 255);
            let width = (label_px / 9.0).max(1.0);
            for dy in [-arm_y, arm_y] {
                prims.push(DrawPrim::Line {
                    x1: center + apex,
                    y1: cy,
                    x2: center - apex,
                    y2: cy + dy,
                    width,
                    color: ink,
                });
            }
        }
        if !drew_any {
            return None;
        }

        let mut bytes = crate::tray_raster::rasterize_tray_pixels(
            &prims,
            img_w as u32,
            img_h as u32,
            // Geometry above is already in device px (converted through the
            // window's own `cell_w`), so the rasterizer must not scale again.
            1.0,
            [0, 0, 0, 0],
        );
        // Variable-instance labels composite over the prim pass with the same
        // linear-light source-over the prim pen uses, in the band's pixel space
        // (band at (0, 0), each overlay at its title span's x).
        for overlay in &overlays {
            crate::tray_raster::composite_rgba_surface(
                &mut bytes,
                (img_w as u32, img_h as u32),
                (0, 0),
                &overlay.rgba,
                (overlay.width as u32, img_h as u32),
                (overlay.x as u32, 0),
            );
        }
        let image = Arc::new(ImageData {
            bytes,
            format: ImageFormat::RawRgba8 {
                width: img_w as u16,
                height: img_h as u16,
            },
            cols: cols as u16,
            rows: geometry.strip_rows as u16,
            // Over the covered cells' (mono) glyphs, under the decorations: the
            // suppression contract the band keys on.
            z_index: 0,
            // The canvas starts at the window's top edge: its first `lift` rows
            // land on the chrome lip above the grid. 0 where there is no lip.
            band_lift_px: lift as u16,
            cell_offset_px: (0, 0),
            scaling: ImageScaling::Fit,
            source_rect: None,
        });

        // Coverage: every column except the fallback segments'. A covered cell's
        // mono glyph is suppressed by the image (`image_covers`).
        let mut covered = vec![true; cols];
        for &(a, b) in &fallback {
            covered[usize::from(a)..usize::from(b).min(cols)].fill(false);
        }
        let rows = (0..geometry.strip_rows)
            .map(|r| {
                let mut row: Vec<(usize, ImageRef)> = Vec::with_capacity(cols);
                for (col, &is_covered) in covered.iter().enumerate() {
                    if is_covered {
                        row.push((
                            col,
                            ImageRef {
                                image: Arc::clone(&image),
                                cell_row: r as u16,
                                cell_col: col as u16,
                                kitty: None,
                            },
                        ));
                    } else if r + 1 == geometry.strip_rows {
                        // A fallback segment keeps its icon/status rasters (on the
                        // last strip row). Band refs and these never share a column,
                        // so the sort order holds.
                        for (icon_col, image) in icon_images {
                            if *icon_col == col {
                                row.push((col, image.clone()));
                            }
                        }
                    }
                }
                row
            })
            .collect();
        Some(rows)
    }

    /// Whether chip titles start at their span's leading edge rather than centre
    /// in it: Windows, where Fluent's TabView (Windows Terminal, Edge) leads the
    /// title after the icon, and a centred title in a wide chip floated with
    /// empty card on both sides. Linux keeps the centred design.
    const LABELS_LEAD: bool = cfg!(windows);

    /// Where a `width`-px title starts in its `span_px` span at `x0`
    /// ([`LABELS_LEAD`]); a title wider than its span starts at `x0` either way.
    fn label_x(x0: f32, span_px: f32, width: f32) -> f32 {
        if LABELS_LEAD {
            x0
        } else {
            ((span_px - width) * 0.5).max(0.0) + x0
        }
    }

    /// Where the `w`-px hairline between two quiet tabs that meet at device x
    /// `boundary` starts. Centred on the boundary when the band draws both
    /// tabs. When the cell lane draws one of them (a title no face maps, the
    /// rename well), that tab's columns are not the band's and would drop half
    /// the line or all of it, so the line lies wholly on the band's side,
    /// against the boundary. `None` when the cell lane draws both: the band
    /// owns no pixel there.
    fn hairline_x(boundary: f32, w: f32, a_band: bool, b_band: bool) -> Option<f32> {
        let boundary = boundary.round();
        match (a_band, b_band) {
            (true, true) => Some(boundary - (w * 0.5).floor()),
            (true, false) => Some(boundary - w),
            (false, true) => Some(boundary),
            (false, false) => None,
        }
    }

    /// A sample of what chip titles are made of (words, a path, a prompt's
    /// punctuation, digits), whose mean advance stands for a title's.
    const TITLE_SAMPLE: &str = "Claude Code: fix tab labels ~/src/aterm-gui MINGW64 pwsh 0.106";

    /// How many characters of a title the UI face sets in one terminal cell
    /// column (`cell_w` px) at `label_px`: the cell width over [`TITLE_SAMPLE`]'s
    /// mean advance, in `[1, 2]`. The chip labels are cut with this many display
    /// cells per column ([`distinct_chip_labels_in`]); the pixel fit then trims
    /// whatever a title of wider letters overruns. 1 before the UI face lands,
    /// and on Linux, which keeps cutting in terminal cells
    /// ([`STRIP_WINDOWS_PAINT`]).
    pub(super) fn ui_cells_per_cell(cell_w: f32, label_px: f32) -> f32 {
        if !STRIP_WINDOWS_PAINT || !crate::tray_raster::strip_band_ui_ready() || cell_w <= 0.0 {
            return 1.0;
        }
        let chars = TITLE_SAMPLE.chars().count() as f32;
        let mean =
            crate::tray_raster::ui_text_width_for(TextFace::Ui, TITLE_SAMPLE, label_px) / chars;
        if mean.is_finite() && mean > 0.0 {
            (cell_w / mean).clamp(1.0, 2.0)
        } else {
            1.0
        }
    }

    /// Each tab's wanted width in columns ([`TabSizing::wants`]) on a band
    /// `band_h` px tall at `scale` with `cell_w`-px columns: the narrowest chip
    /// ([`chip_title_cols`], the tab's own chrome) whose title span seats the
    /// whole title in the UI face at the band's label size, by its pixels and by
    /// the cells the label cut counts ([`ui_cells_per_cell`]), plus a column of
    /// slack for the selected label's variable pen. Measured semibold, the
    /// selected weight, so selecting a tab never resizes it; a leading status
    /// glyph before a space (a spinner frame: Claude Code's `✳ `, a braille
    /// frame) counts as a fixed em, so a spinner's frames never move the `✕`.
    /// 0 (the cap) for a title the band cannot draw (the cell lane's) and before
    /// the UI face lands.
    pub(crate) fn tab_wants(
        titles: &[String],
        metadata: &[TabStripMetadata],
        cell_w: usize,
        band_h: usize,
        scale: f32,
    ) -> Vec<u16> {
        if !STRIP_WINDOWS_PAINT || !crate::tray_raster::strip_band_ui_ready() || cell_w == 0 {
            return Vec::new();
        }
        let label_px = band_label_px(band_h as f32, scale);
        let cw = cell_w as f32;
        let widen = ui_cells_per_cell(cw, label_px);
        titles
            .iter()
            .enumerate()
            .map(|(i, title)| {
                if title.is_empty() {
                    return 0;
                }
                let measure =
                    |s: &str| crate::tray_raster::ui_text_width_for(TextFace::UiBold, s, label_px);
                // Asked semibold too: the faces measured are the selected
                // weight's (its East Asian faces Yu Gothic UI Semibold and
                // Malgun Gothic Bold), parsed for it.
                let (px, by_cells) =
                    if crate::tray_raster::strip_band_run_coverable_in(TextFace::UiBold, title) {
                        let cells = (strip_display_cells(title) as f32 / widen).ceil();
                        (measure(&steady_title(title)), cells)
                    } else if let Some(pieces) = emoji_label(title, label_px, TextFace::UiBold) {
                        // A title with colour emoji ([`emoji_label`]), drawn from its
                        // pieces, never cut by the cell pass.
                        let px = pieces
                            .iter()
                            .map(|piece| match piece {
                                LabelPiece::Text(text) => measure(text),
                                LabelPiece::Emoji(_) => emoji_advance_px(label_px),
                            })
                            .sum();
                        (px, 0.0)
                    } else {
                        return 0;
                    };
                let by_px = ((px + 1.0) / cw).ceil();
                let need = (by_px.max(by_cells) + 1.0).clamp(1.0, 4096.0) as u16;
                let chrome = metadata.get(i).copied().unwrap_or(PLAIN_TAB);
                (need..need.saturating_add(16))
                    .find(|&width| chip_title_cols(width, chrome) >= need)
                    .unwrap_or(need.saturating_add(16))
            })
            .collect()
    }

    /// `title` as [`tab_wants`] measures it: a leading status glyph before a
    /// space (a spinner frame: Claude Code's `✳ `, `·`, a braille frame) stood
    /// in for by `M`, a wide letter, so every frame of a spinning title wants
    /// one width and never moves the `✕`; any other title unchanged.
    fn steady_title(title: &str) -> std::borrow::Cow<'_, str> {
        let mut chars = title.chars();
        match (chars.next(), chars.next()) {
            (Some(mark), Some(' ')) if !mark.is_alphanumeric() => {
                std::borrow::Cow::Owned(format!("M{}", &title[mark.len_utf8()..]))
            }
            _ => std::borrow::Cow::Borrowed(title),
        }
    }

    /// The label size for a `band_h`-px optical band at `scale`: the chrome
    /// label base ([`chrome_label_base_px`]).
    fn band_label_px(band_h: f32, scale: f32) -> f32 {
        chrome_label_base_px(band_h, scale)
    }

    /// The trailing version label's slot, in columns, for each rung: the label
    /// measured in the band's UI face at the chip titles' size ([`band_label_px`] for
    /// this `band_h`-px optical band at `scale`, normalised as [`raster_band`] does),
    /// plus [`version_pad_px`] on each side, rounded up to whole cells of `cell_w` px.
    /// The painter draws with the same face, size and pad, so the hit region and the
    /// painted label are the same columns.
    ///
    /// [`VersionSlotCols::NONE`] until the UI face is installed (or with no cell
    /// width), so no slot is measured in a face that is not there yet. The chips
    /// narrow once when the face lands, on the same frame the strip switches from the
    /// cell lane to the pixel band.
    pub(crate) fn version_slot_cols(
        cell_w: usize,
        band_h: usize,
        scale: f32,
        attention: bool,
    ) -> VersionSlotCols {
        if cell_w == 0 || !crate::tray_raster::strip_band_ui_ready() {
            return VersionSlotCols::NONE;
        }
        let scale = if scale.is_finite() && scale > 0.0 {
            scale
        } else {
            1.0
        };
        let label_px = band_label_px(band_h as f32, scale);
        let pad = version_pad_px(scale);
        let cols_for = |rung: crate::menu::VersionRung| {
            crate::menu::strip_version_label(rung, attention).map_or(0, |label| {
                let px = crate::tray_raster::ui_text_width_for(TextFace::Ui, &label, label_px)
                    + 2.0 * pad;
                let cols = (px / cell_w as f32).ceil();
                if cols.is_finite() && cols > 0.0 {
                    cols.min(f32::from(u16::MAX)) as u16
                } else {
                    0
                }
            })
        };
        VersionSlotCols {
            full: cols_for(crate::menu::VersionRung::Full),
            short: cols_for(crate::menu::VersionRung::Short),
            attention,
        }
    }

    /// The version label's inset from each end of its slot, in device px: 8 logical
    /// px, leaving the text `8 − gap_h` px (5 at 96 dpi) inside the hover card, about
    /// the 6 px a chip title sits inside its own card.
    fn version_pad_px(scale: f32) -> f32 {
        (8.0 * scale).round()
    }

    /// One WINDOW BUTTON ([`TabHit::Caption`]) in its `[x0, x1)` px slot: Adwaita's
    /// round button ([`crate::client_frame::BUTTON_LOGICAL_PX`], no taller than the
    /// card row) centred on the slot and the card row, in the `+`'s materials —
    /// the quiet chip at rest, the band's hover wash under the pointer — with its
    /// symbolic mark in code-native strokes: a low bar (minimize), a square
    /// (maximize), two offset squares (restore), a cross (close). The marks are
    /// sctk-adwaita's geometry (8 logical px boxes, a 7 px cross), so the header
    /// reads as the one it replaces.
    fn draw_caption_button(
        prims: &mut Vec<DrawPrim>,
        button: CaptionButton,
        (x0, x1): (f32, f32),
        design: &BandDesign,
        scale: f32,
        hovered: bool,
        colors: &StripColors,
    ) {
        let card_h = design.card_bot - design.card_top;
        let d = (crate::client_frame::BUTTON_LOGICAL_PX * scale)
            .round()
            .min(card_h)
            .min(x1 - x0);
        if d < 4.0 {
            return;
        }
        let cx = ((x0 + x1) * 0.5).round();
        let cy = (design.card_top + card_h * 0.5).round();
        let (fill, ink) = if hovered {
            (colors.hover_bg, colors.button_hover_fg)
        } else {
            (colors.chip_bg, colors.chip_button_fg)
        };
        prims.push(DrawPrim::Dot {
            cx,
            cy,
            r: d * 0.5,
            color: rgba(fill, 255),
        });
        let ink = rgba(ink, 255);
        // The marks scale with the button, so a button the card row clamped
        // keeps its proportions.
        let u = d / crate::client_frame::BUTTON_LOGICAL_PX;
        let stroke = u.max(1.0);
        let half = 4.0 * u;
        match button {
            CaptionButton::Minimize => prims.push(DrawPrim::Line {
                x1: cx - half,
                y1: cy + half - stroke * 0.5,
                x2: cx + half,
                y2: cy + half - stroke * 0.5,
                width: stroke,
                color: ink,
            }),
            CaptionButton::Maximize => prims.push(DrawPrim::Stroke {
                x: cx - half,
                y: cy - half,
                w: 2.0 * half,
                h: 2.0 * half,
                radius: 0.0,
                width: stroke,
                color: ink,
            }),
            CaptionButton::Restore => {
                let off = 2.0 * u;
                // The front window…
                prims.push(DrawPrim::Stroke {
                    x: cx - half,
                    y: cy - half + off,
                    w: 2.0 * half - off,
                    h: 2.0 * half - off,
                    radius: 0.0,
                    width: stroke,
                    color: ink,
                });
                // …and the corner of the one behind it.
                prims.push(DrawPrim::Line {
                    x1: cx - half + off,
                    y1: cy - half,
                    x2: cx + half,
                    y2: cy - half,
                    width: stroke,
                    color: ink,
                });
                prims.push(DrawPrim::Line {
                    x1: cx + half,
                    y1: cy - half,
                    x2: cx + half,
                    y2: cy + half - off,
                    width: stroke,
                    color: ink,
                });
            }
            CaptionButton::Close => {
                let r = 3.5 * u;
                let width = (1.1 * u).max(1.0);
                for dy in [-r, r] {
                    prims.push(DrawPrim::Line {
                        x1: cx - r,
                        y1: cy - dy,
                        x2: cx + r,
                        y2: cy + dy,
                        width,
                        color: ink,
                    });
                }
            }
        }
    }

    /// The band's resolved metrics: every px the card row is drawn from, derived once
    /// per raster, in canvas coordinates (0 = the window's top edge; the canvas
    /// carries the chrome lip via the image lift).
    struct BandDesign {
        /// The labels' baseline: cap-centred in the content region, or on
        /// Windows ink-centred in the tab ([`DESCENDER_EM`]).
        baseline: f32,
        /// Top edge of the card row.
        card_top: f32,
        /// Bottom edge of the card row — clear of the seam rule on the canvas's
        /// last rows ([`BandGeometry::seam_top_px`]); on Windows the seam's top,
        /// which a tab stands on.
        card_bot: f32,
        /// Windows tabs, not chips ([`SELECTED_TAB_IS_GROUND`]): cards round
        /// their top corners only and stand on the seam, quiet tabs have none,
        /// and two quiet neighbours are parted by a hairline.
        tabs: bool,
        /// Card corner radius.
        radius: f32,
        /// Horizontal inset from a segment's cell edge to its card's painted edge;
        /// adjacent cards sit `2·gap_h` apart.
        gap_h: f32,
        /// Whether quiet (unfocused, unhovered) tabs paint a chip card rather than
        /// bare labels on the band. True: at ten tabs bare labels dissolve into a
        /// row of text fragments, while cards keep every tab a visible, clickable
        /// object (band → chip → hover → selection).
        quiet_cards: bool,
        /// Whether the `+` sits on a quiet chip square rather than a bare glyph.
        /// True: the square gives it a fixed hit-target footprint.
        quiet_button: bool,
        /// Whether the active label is semibold. True: on light schemes the
        /// selected card's raise is gentle (0.14), and weight keeps the selection
        /// legible at a glance.
        active_semibold: bool,
    }

    impl BandDesign {
        fn resolve(geometry: &BandGeometry, img_h: usize, label_px: f32) -> Self {
            let s = if geometry.scale.is_finite() && geometry.scale > 0.0 {
                geometry.scale
            } else {
                1.0
            };
            // The content region: the canvas down to the seam rule on its last
            // rows, or a hairline off the canvas bottom without a resolved seam.
            // The card row is centred in it, so the cards sit as far below the
            // window's top edge as above the seam.
            let content_h = geometry
                .seam_top_px
                .map(|y| y as f32)
                .unwrap_or_else(|| img_h as f32 - (2.0 * s).round())
                .clamp(label_px, img_h as f32);
            let tabs = SELECTED_TAB_IS_GROUND;
            // The card row: ~3.5 logical px of band above and below the cards,
            // centred in the content region. On a compact band the inset shrinks
            // first, down to a hairline of interior, before `band_label_px`'s cap
            // shrinks the type. A Windows tab keeps the inset above it and stands
            // on the seam.
            let inset_v = ((3.5 * s).round())
                .max(3.0)
                .min(content_h * 0.2)
                .min(((content_h - label_px - 2.0) * 0.5).max(1.0));
            let card_top = inset_v;
            let card_bot = if tabs {
                content_h.max(card_top + 1.0)
            } else {
                (content_h - inset_v).max(card_top + 1.0)
            };
            let card_h = card_bot - card_top;
            let baseline = if tabs {
                tab_baseline(card_top, card_bot, label_px, s)
            } else {
                crate::tray_raster::row_baseline(0.0, content_h, label_px).clamp(
                    label_px * 0.9,
                    (img_h as f32 - label_px * 0.28).max(label_px * 0.9),
                )
            };
            // Corners round like a native tab's (6 logical px; a Windows tab's
            // top corners WinUI's 8), never past a pill's semicircle.
            let radius = (if tabs { 8.0 } else { 6.0 } * s).round().min(card_h * 0.5);
            // Half the inter-card gutter (6 logical px between neighbours).
            let gap_h = (3.0 * s).round().max(2.0);
            BandDesign {
                baseline,
                card_top,
                card_bot,
                tabs,
                radius,
                gap_h,
                // The bake-off's verdict, hardwired — see each field's doc. A
                // Windows tab row has no quiet cards and a bare `+`, as Windows
                // Terminal's: the selected tab is the one surface, the hover wash
                // and the hairlines between quiet tabs keep each one an object.
                quiet_cards: !tabs,
                quiet_button: !tabs,
                active_semibold: true,
            }
        }
    }

    /// A title's descender (`g`, `p`, `y`) below its baseline, in em: Segoe
    /// UI's is about 0.21. A run's ink spans its cap height above the baseline
    /// and this below, so a tab label is centred on that box, not on its caps.
    const DESCENDER_EM: f32 = 0.22;

    /// The baseline of a `label_px` title in a Windows tab spanning
    /// `[top, bottom)`: its ink box (caps over descenders, [`DESCENDER_EM`])
    /// centred in the tab, then held a logical pixel inside it at each end,
    /// the descenders' end first. Centred on the caps alone, the descenders of
    /// a 13 px title sat on the last row of a 16 px card at 100% (the
    /// 2026-10-18 judges' `tab-title-descender-clearance`).
    fn tab_baseline(top: f32, bottom: f32, label_px: f32, scale: f32) -> f32 {
        let cap_centred = crate::tray_raster::row_baseline(top, bottom - top, label_px);
        // The cap height the centring used: the baseline sits half of it below
        // the tab's middle.
        let cap = (2.0 * (cap_centred - (top + bottom) * 0.5)).max(0.0);
        let descent = label_px * DESCENDER_EM;
        let air = scale.max(1.0);
        let lowest = bottom - air - descent;
        let highest = top + air + cap;
        (cap_centred - descent * 0.5)
            .min(lowest)
            .max(highest.min(lowest))
    }

    /// The content geometry of one band segment: [`tab_content_layout`] with
    /// metadata, the plain ` title ✕ ` fallback without. Shared by the label pass
    /// and the paint loop so fitted string and span use the same columns.
    fn band_content_layout(
        seg: &TabSegment,
        item: Option<TabStripMetadata>,
        end: u16,
    ) -> TabContentLayout {
        item.map_or_else(
            || TabContentLayout {
                icon_start: None,
                title_start: seg.start_col + 1,
                title_end: match seg.close_col {
                    Some(cx) => cx.saturating_sub(1),
                    None => end.saturating_sub(1),
                },
                status_col: None,
            },
            |item| tab_content_layout(seg, item),
        )
    }

    /// Every chip label the band paints, resolved together: the cell pass's rule
    /// ([`distinct_chip_labels`]) carried through the pixel fit.
    ///
    /// The cell pass proves distinctness in (UI-widened) cell columns
    /// ([`distinct_chip_labels_in`]); [`fit_label`] then cuts again against the
    /// pixel span, which done per chip can collapse distinct labels
    /// (`service-alpha` and `service-beta` both to `service-…`). So the fit runs
    /// here over the whole strip, and a collision it creates is re-cut by ordinal
    /// ([`fit_labels_distinctly`]).
    ///
    /// A run the band cannot cover keeps its cell label (the cell painter draws
    /// that segment) and only counts as a taken name.
    ///
    /// The result is final. Only the active label can still shorten, through
    /// [`VariablePen::fit`], and it is exempt from the collision repair.
    /// `active` is [`BandInput::selected`], passed in so exemption, measuring face
    /// and painted chip agree.
    fn resolve_band_labels(
        input: &BandInput<'_>,
        active: usize,
        cols: usize,
        cw: f32,
        label_px: f32,
        wants_semibold: bool,
    ) -> Vec<Option<String>> {
        // Measure with the face each label will be drawn with. The active label may
        // also go through [`VariablePen::fit`] (same candidates, marginally
        // different advances); the repair rewrites it only when the fit left it
        // naming nothing. The coverage questions below ask in that face too, so
        // a label's East Asian faces are its own weight's before it is measured.
        let face_of = |tab: usize| {
            if tab == active && wants_semibold {
                TextFace::UiBold
            } else {
                TextFace::Ui
            }
        };
        let measure =
            |tab: usize, s: &str| crate::tray_raster::ui_text_width_for(face_of(tab), s, label_px);
        let coverable =
            |tab: usize, s: &str| crate::tray_raster::strip_band_run_coverable_in(face_of(tab), s);
        // Every chip this pass labels, `(tab, title cells, span px)`: every skip
        // the paint loop makes, made here too (a segment it hands to the cell
        // painter has no pixel label to fit).
        let seated: Vec<(usize, u16, f32)> = input
            .segments
            .iter()
            .filter_map(|seg| {
                let end = seg.end_col.min(cols as u16);
                let TabHit::Select(i) = seg.kind else {
                    return None;
                };
                if seg.start_col >= end
                    || seg.solo
                    || i >= input.titles.len()
                    || input.paint.rename.is_some_and(|edit| edit.tab == i)
                {
                    return None;
                }
                let layout = band_content_layout(seg, input.metadata.get(i).copied(), end);
                let avail = layout.title_end.saturating_sub(layout.title_start);
                Some((i, avail, f32::from(avail) * cw - 1.0))
            })
            .collect();
        // The cell pass at `widen` display cells per column, mapped to strip
        // glyphs: `None` for a tab it does not label.
        let labels_at = |widen: f32| -> Vec<Option<String>> {
            let mut cell = if STRIP_DISTINCT_LABELS {
                distinct_chip_labels_in(
                    input.segments,
                    input.titles,
                    Some(input.metadata),
                    active,
                    input.paint.rename.map(|edit| edit.tab),
                    widen,
                )
            } else {
                Vec::new()
            };
            let mut out = vec![None; input.titles.len()];
            for &(i, avail, _) in &seated {
                let raw = input.titles[i].as_str();
                let text: String = cell
                    .get_mut(i)
                    .and_then(Option::take)
                    .unwrap_or_else(|| chip_cut(raw, widened_budget(usize::from(avail), widen)))
                    .chars()
                    .map(strip_char)
                    .collect();
                out[i] = Some(text);
            }
            out
        };
        // Cut in UI-face-sized cells, not terminal cells ([`ui_cells_per_cell`]),
        // and narrow the step, tab by tab, until each label the cell pass
        // resolved fits its own span ([`widest_fitting_cuts`]): the per-chip fit
        // below then only trims glyph-level slack, and a cut keeps the cell
        // pass's own rule (a path keeps its end) instead of losing its tail to a
        // pixel trim. Every tab's step is the one its label fits at in the
        // selected weight, as [`tab_wants`] sizes every tab, so selecting a tab
        // never changes its words: a quiet `vim … platform_win.rs` would turn
        // into `…platform_win.rs` as it took the wider semibold.
        let selected_face = if wants_semibold {
            TextFace::UiBold
        } else {
            TextFace::Ui
        };
        let spans: Vec<(usize, f32)> = seated.iter().map(|&(i, _, span)| (i, span)).collect();
        let mut cell = widest_fitting_cuts(
            &spans,
            ui_cells_per_cell(cw, label_px),
            labels_at,
            |i, text, span_px| {
                coverable(i, text)
                    && crate::tray_raster::ui_text_width_for(selected_face, text, label_px)
                        > span_px
            },
        );
        let mut resolved: Vec<Option<String>> = vec![None; input.titles.len()];
        let mut entries: Vec<BandLabel> = Vec::new();
        for &(i, _, span_px) in &seated {
            let text = cell[i].take().unwrap_or_default();
            if !coverable(i, &text) {
                // The cell painter's segment: hand the paint loop the string it
                // checked coverage with, so it takes the same fallback branch.
                resolved[i] = Some(text);
                continue;
            }
            entries.push(BandLabel {
                tab: i,
                span_px,
                text,
            });
        }
        let taken: Vec<&str> = resolved.iter().filter_map(Option::as_deref).collect();
        for (tab, text) in fit_labels_distinctly(&entries, active, &taken, &measure) {
            resolved[tab] = Some(text);
        }
        resolved
    }

    /// Each seated tab's cell-pass label at the WIDEST step at which that tab's
    /// own label fits its span ([`resolve_band_labels`]). `seated` is
    /// `(tab, span px)` per chip; `labels_at(widen)` is the cell pass at `widen`
    /// display cells per column, indexed by tab; `overflows(tab, label, span)`
    /// asks whether a label overruns its span (the band asks in the selected
    /// weight for every tab). The step narrows by 0.1 from `widen` down to
    /// 1.0, where every label still waiting is taken as it is.
    ///
    /// Tab by tab: a neighbour that needs a narrower step costs this tab no
    /// characters. One step for the whole strip let one wide label shorten
    /// every other cut, so a tab read differently with its neighbour's title:
    /// at 200% the selected Claude Code task read `Claude Code: rev…` beside
    /// a `C:\Users\m6-an\aterm` tab and `Claude Code: revi…` beside
    /// `C:\Users\m6-an\aterm-winperfect`, a run in another folder read as a
    /// regression (the 2026-10-18 verifier's `A11-hidpi-selected-label-fit`),
    /// and at 100% a selected `less` or `vim` command cost the quiet Claude
    /// Code tab a letter. Each step's pass is still the whole
    /// strip's, so its cut rules (a cluster's shed, a twin's ordinal) hold at
    /// that step, and the fit's collision repair ([`fit_labels_distinctly`])
    /// renumbers any two labels taken at different steps that read alike.
    fn widest_fitting_cuts(
        seated: &[(usize, f32)],
        mut widen: f32,
        labels_at: impl Fn(f32) -> Vec<Option<String>>,
        overflows: impl Fn(usize, &str, f32) -> bool,
    ) -> Vec<Option<String>> {
        let mut chosen: Vec<Option<String>> = Vec::new();
        let mut waiting: Vec<(usize, f32)> = seated.to_vec();
        loop {
            let mut at = labels_at(widen);
            if chosen.len() < at.len() {
                chosen.resize(at.len(), None);
            }
            let floor = widen.is_nan() || widen <= 1.0;
            waiting.retain(|&(tab, span_px)| {
                let text = at.get_mut(tab).and_then(Option::take);
                if !floor && text.as_deref().is_some_and(|t| overflows(tab, t, span_px)) {
                    return true;
                }
                if let Some(slot) = chosen.get_mut(tab) {
                    *slot = text;
                }
                false
            });
            if waiting.is_empty() {
                return chosen;
            }
            widen = (widen - 0.1).max(1.0);
        }
    }

    /// One chip's label on the way into the pixel fit: the cell pass's resolved
    /// string and the span it has to survive.
    struct BandLabel {
        tab: usize,
        span_px: f32,
        text: String,
    }

    /// Fit every band label to its own span and keep the strip distinct across the
    /// fit: [`resolve_band_labels`]'s rule, pure, so it is testable without a font.
    ///
    /// `entries` are in strip order; `reserved` are labels already resolved (the
    /// cell painter's fallback segments) that a fitted label must not collide with.
    /// The active chip claims its fitted string first and is exempt from the
    /// collision repair (the other chip moves), but not from naming nothing
    /// ([`label_says_nothing`]): then it takes its ordinal too, before `taken` is
    /// seeded from it.
    fn fit_labels_distinctly(
        entries: &[BandLabel],
        active: usize,
        reserved: &[&str],
        measure: &dyn Fn(usize, &str) -> f32,
    ) -> Vec<(usize, String)> {
        let mut fitted: Vec<(usize, String)> = entries
            .iter()
            .map(|entry| {
                (
                    entry.tab,
                    fit_label(entry.text.clone(), entry.span_px, |s| measure(entry.tab, s)),
                )
            })
            .collect();
        // One chip's position, spelled as far as its span affords: the repair's
        // only replacement.
        let ordinal = |n: usize| {
            let tab = entries[n].tab;
            pixel_ordinal_label(tab, &entries[n].text, entries[n].span_px, &|s| {
                measure(tab, s)
            })
        };
        // The selection's floor, resolved before anything is claimed: a cut that
        // names nothing takes the ordinal (or the mark when not even the number
        // fits).
        if let Some(n) = fitted.iter().position(|(tab, _)| *tab == active)
            && label_says_nothing(&fitted[n].1)
        {
            let recut = ordinal(n);
            if !recut.is_empty() {
                fitted[n].1 = recut;
            }
        }
        let mut taken: Vec<String> = reserved.iter().map(|s| (*s).to_string()).collect();
        taken.extend(
            fitted
                .iter()
                .filter(|(tab, _)| *tab == active)
                .map(|(_, text)| text.clone()),
        );
        // Every holder of a name shared by quiet chips takes its position, not all
        // but the first: a shared name names neither. The selection keeps its own.
        let shared: Vec<bool> = fitted
            .iter()
            .enumerate()
            .map(|(n, (tab, text))| {
                *tab != active
                    && !text.is_empty()
                    && fitted.iter().enumerate().any(|(m, (other_tab, other))| {
                        m != n && *other_tab != active && other == text
                    })
            })
            .collect();
        for n in 0..fitted.len() {
            let tab = fitted[n].0;
            if tab == active {
                continue;
            }
            // An empty label claims nothing and collides with nothing
            // (`ordinal_chip_label` documents that floor).
            if fitted[n].1.is_empty() {
                continue;
            }
            // A label loses its name when another quiet chip shares it, when the fit
            // collapsed it onto one already on the strip, or when it was cut to
            // nothing but elision marks (which can undo a cell-pass ordinal).
            let lost = shared[n]
                || label_says_nothing(&fitted[n].1)
                || taken.iter().any(|other| *other == fitted[n].1);
            if !lost {
                taken.push(fitted[n].1.clone());
                continue;
            }
            // The twins' answer in the pixel domain: the strip position with as
            // much of the label trailing as the span affords.
            let recut = ordinal(n);
            if recut.is_empty() {
                // Not even the number fits; keep whatever the fit left.
                continue;
            }
            let mut text = recut;
            if taken.contains(&text) {
                // The bare number, claimed only when it is free or its one holder is
                // a band label this pass may still rewrite (which then takes its own,
                // different position, so this cannot cascade).
                let digits = (tab + 1).to_string();
                let holders = taken.iter().filter(|other| **other == digits).count();
                // The single holder, when it is a band label resolved ahead of this
                // one. `None` (a cell fallback or the active chip's title) leaves the
                // ordinal-with-tail standing instead.
                let impostor = if holders == 1 {
                    (0..n).find(|&m| fitted[m].0 != active && fitted[m].1 == digits)
                } else {
                    None
                };
                if holders == 0 {
                    text = digits;
                } else if let Some(m) = impostor {
                    let swap = ordinal(m);
                    if !swap.is_empty() && !taken.contains(&swap) {
                        if let Some(p) = taken.iter().position(|other| *other == digits) {
                            taken.remove(p);
                        }
                        taken.push(swap.clone());
                        fitted[m].1 = swap;
                        text = digits;
                    }
                }
            }
            fitted[n].1 = text.clone();
            taken.push(text);
        }
        fitted
    }

    /// Whether this label names anything or is only elision marks. Structural
    /// rather than `label == "…"`: [`fit_label`] only drops a trailing mark, so a
    /// leading-mark tail cut can shrink to `……`.
    fn label_says_nothing(label: &str) -> bool {
        label
            .trim_matches(|c: char| c == '…' || c.is_whitespace())
            .is_empty()
    }

    /// [`ordinal_chip_label`] in the pixel domain: the chip's 1-based strip
    /// position, with as much of `label` as the span affords after ` · `.
    ///
    /// Empty when the span cannot seat the number (the label is clipped to its
    /// span, and a clipped `10` would read as tab 1). The caller then keeps what
    /// the fit left.
    fn pixel_ordinal_label(
        tab: usize,
        label: &str,
        span_px: f32,
        measure: &dyn Fn(&str) -> f32,
    ) -> String {
        let digits = (tab + 1).to_string();
        if measure(&digits) > span_px {
            return String::new();
        }
        let head = format!("{digits} · ");
        let room = span_px - measure(&head);
        if room > 0.0 {
            let tail = fit_label(label.to_string(), room, measure);
            // A tail of only elision marks after the number says nothing; use the
            // bare number ([`label_says_nothing`]). The composed string is measured
            // too, since kerning can push it past the span and this answer is final.
            let composed = head + &tail;
            if !label_says_nothing(&tail) && measure(&composed) <= span_px {
                return composed;
            }
        }
        digits
    }

    /// Shrink `label` (already display-cell-truncated) until it measures inside
    /// `span_px` under `measure`, the pen that will draw it. Usually a no-op; a wide
    /// title loses chars to the ellipsis instead of clipping mid-glyph.
    /// [`VariablePen::fit`] applies the same candidate order to a shaped run.
    ///
    /// Per chip, so only [`resolve_band_labels`] (which repairs collisions via
    /// [`fit_labels_distinctly`]) and its ordinal fallback call it, and the paint
    /// loop only for a selected label that keeps its end and overruns its span
    /// under the variable pen ([`cut_keeps_its_end`]).
    ///
    /// A tail cut (`…work\sub dir`, or one behind an ordinal, `2 · …sub dir`)
    /// keeps its END, which is what tells the chip apart: it sheds characters
    /// right after its mark instead, never below three (a shorter stub names
    /// nothing, as [`path_tail`] rules). Its floor is the mark alone, which the
    /// repair detects ([`label_says_nothing`]). Only a LEADING mark is a tail
    /// cut: a title that says `…` itself (`Thinking… (esc to interrupt)`) is
    /// head-cut like any other. A command cut (`vim -n -R … platform_win.rs`,
    /// [`middle_cut_parts`]) keeps its end as well: its head sheds characters
    /// before the mark, and with the head gone it is a tail cut.
    fn fit_label(label: String, span_px: f32, measure: impl Fn(&str) -> f32) -> String {
        if measure(&label) <= span_px {
            return label;
        }
        // A command cut ([`command_cut`]) keeps its end too: its head gives way
        // first, a character at a time, then the tail cut's rule below.
        if let Some((head, tail)) = middle_cut_parts(&label) {
            let mut stem: Vec<char> = head.chars().collect();
            while stem.pop().is_some() {
                let kept: String = stem.iter().collect();
                let kept = kept.trim_end();
                if kept.is_empty() {
                    break;
                }
                let candidate = format!("{kept}{MIDDLE_MARK}{tail}");
                if measure(&candidate) <= span_px {
                    return candidate;
                }
            }
            return fit_label(format!("…{tail}"), span_px, measure);
        }
        if let Some(mark) = tail_cut_mark(&label) {
            let (head, tail) = label.split_at(mark + '…'.len_utf8());
            let mut rest = tail.chars();
            while rest.next().is_some() && rest.as_str().chars().count() >= 3 {
                let candidate = format!("{head}{}", rest.as_str());
                if measure(&candidate) <= span_px {
                    return candidate;
                }
            }
            return head.to_string();
        }
        let mut stem: Vec<char> = label.chars().collect();
        // Drop a trailing ellipsis from the cell truncation before re-fitting.
        if stem.last() == Some(&'…') {
            stem.pop();
        }
        while stem.pop().is_some() {
            let candidate: String = stem.iter().collect::<String>() + "…";
            if measure(&candidate) <= span_px {
                return candidate;
            }
        }
        "…".to_string()
    }

    /// Where a tail cut's mark sits in `label` ([`fit_label`]): first
    /// (`…sub dir`), or right behind an ordinal (`12 · …sub dir`); `None` for
    /// any other label, and for one that also ends in `…` (a head cut).
    fn tail_cut_mark(label: &str) -> Option<usize> {
        if label.ends_with('…') {
            return None;
        }
        if label.starts_with('…') {
            return Some(0);
        }
        label.split_once(" · …").and_then(|(digits, _)| {
            (!digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit()))
                .then(|| digits.len() + " · ".len())
        })
    }

    /// A command cut's head and tail ([`command_cut`]): the text either side of
    /// its last [`MIDDLE_MARK`], both non-empty, in a label that neither starts
    /// nor ends with `…`.
    fn middle_cut_parts(label: &str) -> Option<(&str, &str)> {
        if label.starts_with('…') || label.ends_with('…') {
            return None;
        }
        label
            .rsplit_once(MIDDLE_MARK)
            .filter(|(head, tail)| !head.trim().is_empty() && !tail.trim().is_empty())
    }

    /// Whether a label's cut keeps its END ([`tail_cut_mark`],
    /// [`middle_cut_parts`]), so a pen that would cut its end off
    /// ([`VariablePen::fit`]) must re-cut it by [`fit_label`] instead.
    fn cut_keeps_its_end(label: &str) -> bool {
        tail_cut_mark(label).is_some() || middle_cut_parts(label).is_some()
    }

    /// One glyph of a [`ShapedRun`]: its coverage raster and the pen position where
    /// it was placed (run space; vertical placement is baseline-relative via
    /// `ymin`). `pen` and `xmin` are separate so a glyph can be re-placed without
    /// re-rastering.
    struct ShapedGlyph {
        pen: f32,
        xmin: i32,
        w: usize,
        h: usize,
        ymin: i32,
        cov: Vec<u8>,
    }

    impl ShapedGlyph {
        /// The fontdue pen's placement, term for term: pen + bearing, truncated
        /// to the device column.
        fn x(&self) -> i32 {
            (self.pen + self.xmin as f32).floor() as i32
        }
    }

    /// Where one char of a [`ShapedRun`] ended: the pen after its advance and the
    /// glyph count so far; the cut points [`VariablePen::fit`] truncates at.
    #[derive(Clone, Copy)]
    struct CharCut {
        ch: char,
        pen: f32,
        glyphs: usize,
    }

    /// A label shaped through the variable semibold instance: per-glyph rasters at
    /// the resolved `wght`, advanced by the instance's HVAR-varied advances plus the
    /// regular's `kern` pairs and the UI tracking.
    struct ShapedRun {
        glyphs: Vec<ShapedGlyph>,
        /// Total advance in px (the measure the fit uses).
        width: f32,
        /// One entry per char of the shaped text, in order.
        cuts: Vec<CharCut>,
    }

    /// A finished label raster to composite over the band at `x`.
    struct LabelOverlay {
        x: usize,
        width: usize,
        rgba: Vec<u8>,
    }

    /// The variable semibold ready to draw: the instance parsed once with its
    /// coords applied, beside the regular's cmap/kern. One per band raster.
    struct VariablePen<'a> {
        vf: &'a crate::tray_raster::UiVariableSemibold,
        face: aterm_render::variation::VariedFace<'a>,
    }

    impl<'a> VariablePen<'a> {
        /// `None` when the retained bytes no longer parse; the caller then takes
        /// the static `UiBold` path.
        fn new(vf: &'a crate::tray_raster::UiVariableSemibold) -> Option<Self> {
            let face = aterm_render::variation::VariedFace::parse(&vf.bytes, vf.index, &vf.coords)?;
            Some(Self { vf, face })
        }

        /// Shape `text` at `px` through the variable semibold. `None` when any char
        /// is outside the face's cmap or a glyph fails to raster; the caller then
        /// draws the whole run through the `UiBold` path, whose per-char cascade
        /// covers it. One raster per char.
        fn shape(&self, text: &str, px: f32) -> Option<ShapedRun> {
            let mut pen = 0.0f32;
            let mut prev: Option<char> = None;
            let mut glyphs = Vec::new();
            let mut cuts = Vec::with_capacity(text.len());
            for ch in text.chars() {
                let gid = self.vf.cmap.unicode_glyph_index(ch);
                if gid == 0 {
                    return None;
                }
                if let Some(previous) = prev {
                    pen += self.vf.cmap.char_kern(previous, ch, px).unwrap_or(0.0);
                }
                let (w, h, xmin, ymin, advance, cov) = self.face.glyph_raster(gid, px)?;
                if w > 0 && h > 0 {
                    glyphs.push(ShapedGlyph {
                        pen,
                        xmin,
                        w,
                        h,
                        ymin,
                        cov,
                    });
                }
                pen += advance + crate::tray_raster::UI_TRACKING_EM * px;
                prev = Some(ch);
                cuts.push(CharCut {
                    ch,
                    pen,
                    glyphs: glyphs.len(),
                });
            }
            Some(ShapedRun {
                glyphs,
                width: pen.max(0.0),
                cuts,
            })
        }

        /// [`fit_label`] for an already-shaped run: the same candidate order (drop a
        /// trailing `…`, then every shorter prefix + `…`, longest first; the bare `…`
        /// ships even when it overflows, bounded by the span clip), applied by
        /// cutting glyphs at a char boundary and placing one ellipsis raster at the
        /// prefix's pen plus kern. Placement equals shaping the fitted string. A run
        /// that fits is returned untouched. `None` only when `…` is outside the face.
        fn fit(&self, run: ShapedRun, span_px: f32, px: f32) -> Option<ShapedRun> {
            if run.width <= span_px {
                return Some(run);
            }
            let ellipsis = self.shape("…", px)?;
            let ShapedRun {
                mut glyphs,
                mut cuts,
                ..
            } = run;
            if cuts.last().is_some_and(|cut| cut.ch == '…') {
                cuts.pop();
            }
            // `(pen the ellipsis starts at, glyphs the prefix keeps)` for a
            // `k`-char prefix — the prefix's own pen plus the kern into `…`.
            let origin = |k: usize| -> (f32, usize) {
                match k.checked_sub(1).map(|i| cuts[i]) {
                    None => (0.0, 0),
                    Some(last) => (
                        last.pen + self.vf.cmap.char_kern(last.ch, '…', px).unwrap_or(0.0),
                        last.glyphs,
                    ),
                }
            };
            let keep = (0..cuts.len())
                .rev()
                .find(|&k| origin(k).0 + ellipsis.width <= span_px)
                .unwrap_or(0);
            let (pen, glyph_end) = origin(keep);
            glyphs.truncate(glyph_end);
            glyphs.extend(ellipsis.glyphs.into_iter().map(|glyph| ShapedGlyph {
                pen: pen + glyph.pen,
                ..glyph
            }));
            cuts.truncate(keep);
            let width = (pen + ellipsis.width).max(0.0);
            cuts.push(CharCut {
                ch: '…',
                pen: width,
                glyphs: glyphs.len(),
            });
            Some(ShapedRun {
                glyphs,
                width,
                cuts,
            })
        }
    }

    /// Shape `text` at `px` through the variable semibold — [`VariablePen::shape`]
    /// over a face parsed for this one call.
    #[cfg(test)]
    fn shape_variable_label(
        vf: &crate::tray_raster::UiVariableSemibold,
        text: &str,
        px: f32,
    ) -> Option<ShapedRun> {
        VariablePen::new(vf)?.shape(text, px)
    }

    /// Lay a [`ShapedRun`] into a straight-alpha RGBA overlay `height` px tall, at
    /// most `span_px` wide, inked in `color` on a transparent ground, for the
    /// `ground` it will be composited over (the selected tab's surface).
    ///
    /// The overlay's alpha is the glyph coverage after the perceptual remap every
    /// other chrome glyph gets ([`aterm_render::correct_alpha`], the tray pen's
    /// `blend_glyph_texel`), keyed on `color` over `ground`: the overlay is
    /// composited by a plain linear-light source-over
    /// ([`crate::tray_raster::composite_rgba_surface`]), which thins dark ink on
    /// a light ground and thickens light ink on a dark one. Uncorrected, the
    /// selected tab's semibold title measured 0.54 mean coverage on Default
    /// Light against 0.68 on Default, lighter than the regular quiet titles
    /// beside it (the 2026-10-18 judges' `light-selected-tab-label-uncorrected-alpha`).
    fn variable_label_overlay(
        run: &ShapedRun,
        x: f32,
        baseline: f32,
        height: usize,
        span_px: f32,
        color: [u8; 3],
        ground: [u8; 3],
    ) -> LabelOverlay {
        let width = (run.width.ceil() as usize + 2)
            .min(span_px.floor().max(1.0) as usize)
            .max(1);
        let mut rgba = vec![0u8; width * height * 4];
        let baseline_i = baseline.round() as i32;
        for glyph in &run.glyphs {
            let gy = baseline_i - (glyph.h as i32 + glyph.ymin);
            for yy in 0..glyph.h {
                let y = gy + yy as i32;
                if y < 0 || y >= height as i32 {
                    continue;
                }
                for xx in 0..glyph.w {
                    let px = glyph.x() + xx as i32;
                    if px < 0 || px >= width as i32 {
                        continue;
                    }
                    let a = glyph.cov[yy * glyph.w + xx];
                    if a == 0 {
                        continue;
                    }
                    let i = (y as usize * width + px as usize) * 4;
                    rgba[i..i + 3].copy_from_slice(&color);
                    rgba[i + 3] = rgba[i + 3].max(a);
                }
            }
        }
        // The remap per coverage byte, once per label; 0 and 255 stay exact.
        let pack = |c: [u8; 3]| u32::from(c[0]) << 16 | u32::from(c[1]) << 8 | u32::from(c[2]);
        let (ink_l, ground_l) = (
            aterm_render::linear_luminance(pack(color)),
            aterm_render::linear_luminance(pack(ground)),
        );
        let mut remap = [0u8; 256];
        for (coverage, slot) in remap.iter_mut().enumerate() {
            *slot = match coverage {
                0 => 0,
                255 => 255,
                c => (aterm_render::correct_alpha(ink_l, ground_l, c as f32 / 255.0) * 255.0)
                    .round()
                    .clamp(0.0, 255.0) as u8,
            };
        }
        for alpha in rgba.iter_mut().skip(3).step_by(4) {
            *alpha = remap[usize::from(*alpha)];
        }
        LabelOverlay {
            x: x.max(0.0) as usize,
            width,
            rgba,
        }
    }

    /// A colour emoji's square in a band label, as a share of the label's size:
    /// a touch over it, as Windows' own text sets an emoji beside its letters.
    const EMOJI_EM: f32 = 1.2;

    /// One piece of a band label that carries colour emoji ([`emoji_label`]).
    pub(super) enum LabelPiece {
        /// A run the UI stack draws.
        Text(String),
        /// A cluster it cannot, as its colour emoji.
        Emoji(Arc<crate::tray_raster::ChromeEmoji>),
    }

    /// The square, in device px, a colour emoji takes in a `label_px` label.
    fn emoji_box_px(label_px: f32) -> f32 {
        (label_px * EMOJI_EM).round().max(4.0)
    }

    /// The advance of a colour emoji in a `label_px` label: its square and a
    /// hair of air before the next letter.
    fn emoji_advance_px(label_px: f32) -> f32 {
        emoji_box_px(label_px) + (label_px * 0.12).round()
    }

    /// `title` as pieces the band draws when the UI stack cannot draw every
    /// char of it: runs of text it can, and each grapheme it cannot as its
    /// colour emoji ([`crate::tray_raster::chrome_colour_emoji`], the
    /// terminal's own colour face). `None` when such a grapheme is no colour
    /// emoji either and no face maps it: then the cell lane draws the title.
    /// The text is asked in `face`, the face it is drawn in, so its East
    /// Asian faces are that weight's.
    pub(super) fn emoji_label(
        title: &str,
        label_px: f32,
        face: TextFace,
    ) -> Option<Vec<LabelPiece>> {
        let side = emoji_box_px(label_px);
        let mut pieces = Vec::new();
        let mut run = String::new();
        for grapheme in aterm_grapheme::split_graphemes(title) {
            let cluster = grapheme.text;
            if crate::tray_raster::strip_band_run_coverable_in(face, cluster) {
                run.push_str(cluster);
                continue;
            }
            // Only an emoji is asked of the colour face; anything else no face
            // maps is the cell lane's title.
            if !grapheme.is_emoji {
                return None;
            }
            let emoji = crate::tray_raster::chrome_colour_emoji(cluster, side)?;
            if !run.is_empty() {
                pieces.push(LabelPiece::Text(std::mem::take(&mut run)));
            }
            pieces.push(LabelPiece::Emoji(emoji));
        }
        if !run.is_empty() {
            pieces.push(LabelPiece::Text(run));
        }
        Some(pieces)
    }

    /// Where a label with colour emoji is drawn ([`draw_emoji_label`]): its
    /// span from `x0`, the band's baseline and optical centre `cy`, the label
    /// size and the canvas height.
    struct EmojiLabelAt {
        x0: f32,
        span_px: f32,
        baseline: f32,
        cy: f32,
        label_px: f32,
        height: usize,
    }

    /// Draw `pieces` ([`emoji_label`]) as one label in its span: the text runs
    /// in the UI face on the baseline (the selected title's through the
    /// variable semibold `pen` when there is one, as every selected title is,
    /// alpha-corrected on `ground`), each emoji in its square centred on the
    /// label's optical centre, leading like every Windows title, and cut at the
    /// end with `…` where the whole does not fit, never inside a cluster.
    #[allow(
        clippy::too_many_arguments,
        reason = "the label's pieces, the two outputs it draws into, where, and its face, \
                  pen, ink and ground"
    )]
    fn draw_emoji_label(
        prims: &mut Vec<DrawPrim>,
        overlays: &mut Vec<LabelOverlay>,
        pieces: &[LabelPiece],
        at: EmojiLabelAt,
        face: TextFace,
        pen: Option<&VariablePen<'_>>,
        ink: [u8; 3],
        ground: [u8; 3],
    ) {
        use aterm_grapheme::GraphemeClusters as _;
        let shaped = |s: &str| pen.and_then(|pen| pen.shape(s, at.label_px));
        let measure = |s: &str| {
            shaped(s).map_or_else(
                || crate::tray_raster::ui_text_width_for(face, s, at.label_px),
                |run| run.width,
            )
        };
        let advance = emoji_advance_px(at.label_px);
        // The label as units, each a whole cluster: what a cut may drop.
        enum Unit<'a> {
            Text(String),
            Emoji(&'a Arc<crate::tray_raster::ChromeEmoji>),
        }
        let mut units: Vec<Unit<'_>> = Vec::new();
        for piece in pieces {
            match piece {
                LabelPiece::Text(text) => {
                    units.extend(text.graphemes().map(|g| Unit::Text(g.to_string())));
                }
                LabelPiece::Emoji(emoji) => units.push(Unit::Emoji(emoji)),
            }
        }
        let width = |units: &[Unit<'_>]| -> f32 {
            let mut text = String::new();
            let mut total = 0.0;
            for unit in units {
                match unit {
                    Unit::Text(s) => text.push_str(s),
                    Unit::Emoji(_) => {
                        total += measure(&std::mem::take(&mut text)) + advance;
                    }
                }
            }
            total + measure(&text)
        };
        let mut keep = units.len();
        let mut cut = false;
        while keep > 0 && width(&units[..keep]) > at.span_px {
            keep -= 1;
            cut = true;
        }
        if cut {
            let ellipsis = measure("…");
            while keep > 0 && width(&units[..keep]) + ellipsis > at.span_px {
                keep -= 1;
            }
            units.truncate(keep);
            units.push(Unit::Text("…".to_string()));
        }
        let total = width(&units);
        let mut x = label_x(at.x0, at.span_px, total);
        let end = at.x0 + at.span_px;
        let side = emoji_box_px(at.label_px);
        prims.push(DrawPrim::ClipPush {
            x: at.x0,
            y: 0.0,
            w: at.span_px,
            h: at.height as f32,
        });
        let mut text = String::new();
        let flush = |prims: &mut Vec<DrawPrim>,
                     overlays: &mut Vec<LabelOverlay>,
                     text: &mut String,
                     x: &mut f32| {
            if text.is_empty() {
                return;
            }
            let run = std::mem::take(text);
            if let Some(shaped) = shaped(&run) {
                let width = shaped.width;
                overlays.push(variable_label_overlay(
                    &shaped,
                    *x,
                    at.baseline,
                    at.height,
                    (end - *x).max(1.0),
                    ink,
                    ground,
                ));
                *x += width;
                return;
            }
            let width = measure(&run);
            prims.push(text_prim(
                *x,
                at.baseline,
                run,
                TypeStep::Body.px(at.label_px),
                TextWeight::Regular,
                face,
                rgba(ink, 255),
            ));
            *x += width;
        };
        for unit in &units {
            match unit {
                Unit::Text(s) => text.push_str(s),
                Unit::Emoji(emoji) => {
                    flush(prims, overlays, &mut text, &mut x);
                    if x + side <= end + 0.5 {
                        overlays.push(emoji_overlay(emoji, x, at.cy, side, at.height));
                    }
                    x += advance;
                }
            }
        }
        flush(prims, overlays, &mut text, &mut x);
        prims.push(DrawPrim::ClipPop);
    }

    /// `emoji` fitted into a `side`-px square whose left edge is `x` and whose
    /// centre is at `cy`, as an overlay the canvas's height: a box filter over
    /// its premultiplied texels when it scales (the raster is asked for at
    /// about this size, so it rarely does by much).
    fn emoji_overlay(
        emoji: &crate::tray_raster::ChromeEmoji,
        x: f32,
        cy: f32,
        side: f32,
        height: usize,
    ) -> LabelOverlay {
        let side_px = side.max(1.0) as usize;
        let scale = side / emoji.width.max(emoji.height) as f32;
        let tw = ((emoji.width as f32 * scale).round() as usize).clamp(1, side_px);
        let th = ((emoji.height as f32 * scale).round() as usize).clamp(1, side_px);
        let left = (side_px - tw) / 2;
        let top = ((cy - th as f32 * 0.5).round().max(0.0)) as usize;
        let mut rgba = vec![0u8; side_px * height * 4];
        // Sample a 4x4 grid of each target pixel's footprint in the source.
        const SUB: usize = 4;
        for ty in 0..th {
            let y = top + ty;
            if y >= height {
                break;
            }
            for tx in 0..tw {
                let mut acc = [0f32; 4];
                for sy in 0..SUB {
                    let src_y = (((ty * SUB + sy) as f32 + 0.5) / (th * SUB) as f32
                        * emoji.height as f32) as usize;
                    for sx in 0..SUB {
                        let src_x = (((tx * SUB + sx) as f32 + 0.5) / (tw * SUB) as f32
                            * emoji.width as f32) as usize;
                        let at = (src_y.min(emoji.height - 1) * emoji.width
                            + src_x.min(emoji.width - 1))
                            * 4;
                        let a = f32::from(emoji.rgba[at + 3]) / 255.0;
                        for (k, sum) in acc.iter_mut().take(3).enumerate() {
                            *sum += f32::from(emoji.rgba[at + k]) * a;
                        }
                        acc[3] += a;
                    }
                }
                let n = (SUB * SUB) as f32;
                let a = acc[3] / n;
                if a <= 0.0 {
                    continue;
                }
                let i = (y * side_px + left + tx) * 4;
                for k in 0..3 {
                    rgba[i + k] = (acc[k] / n / a).round().clamp(0.0, 255.0) as u8;
                }
                rgba[i + 3] = (a * 255.0).round().clamp(0.0, 255.0) as u8;
            }
        }
        LabelOverlay {
            x: x.round().max(0.0) as usize,
            width: side_px,
            rgba,
        }
    }

    /// Map one 16-unit icon IR box ([`TabIconPrimitive`]) into `DrawPrim`s centred
    /// at `(cx, cy)` with side `side` px: the cell path's shapes, drawn at band
    /// resolution and centred in the band.
    fn draw_icon_prims(
        prims: &mut Vec<DrawPrim>,
        primitives: &[TabIconPrimitive],
        cx: f32,
        cy: f32,
        side: f32,
        color: [u8; 3],
    ) {
        let k = side / TAB_ICON_DESIGN_SIZE;
        let ox = cx - side * 0.5;
        let oy = cy - side * 0.5;
        let ink = rgba(color, 255);
        for primitive in primitives {
            match *primitive {
                TabIconPrimitive::Line { from, to, width } => {
                    prims.push(DrawPrim::Line {
                        x1: from[0].mul_add(k, ox),
                        y1: from[1].mul_add(k, oy),
                        x2: to[0].mul_add(k, ox),
                        y2: to[1].mul_add(k, oy),
                        width: (width * k).max(1.0),
                        color: ink,
                    });
                }
                TabIconPrimitive::RoundedRect {
                    rect,
                    radius,
                    width,
                } => {
                    prims.push(DrawPrim::Stroke {
                        x: rect[0].mul_add(k, ox),
                        y: rect[1].mul_add(k, oy),
                        w: rect[2] * k,
                        h: rect[3] * k,
                        radius: radius * k,
                        width: (width * k).max(1.0),
                        color: ink,
                    });
                }
                // The connection mark's arrow. The band has no polygon fill, so the
                // triangle is its three edges at the icon's stroke weight, which
                // close into a solid mark at chip size.
                TabIconPrimitive::Triangle { points } => {
                    for pair in [(0usize, 1usize), (1, 2), (2, 0)] {
                        let (a, b) = (points[pair.0], points[pair.1]);
                        prims.push(DrawPrim::Line {
                            x1: a[0].mul_add(k, ox),
                            y1: a[1].mul_add(k, oy),
                            x2: b[0].mul_add(k, ox),
                            y2: b[1].mul_add(k, oy),
                            width: (0.9 * k).max(1.0),
                            color: ink,
                        });
                    }
                }
                TabIconPrimitive::Dot { center, radius } => {
                    prims.push(DrawPrim::Dot {
                        cx: center[0].mul_add(k, ox),
                        cy: center[1].mul_add(k, oy),
                        r: (radius * k).max(1.0),
                        color: ink,
                    });
                }
            }
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        const CELL_W: usize = 9;
        const CELL_H: usize = 21;
        const BAND_TOP: usize = 2;

        fn geometry(cols: usize, strip_rows: usize) -> BandGeometry {
            BandGeometry {
                cols,
                cell_w: CELL_W,
                cell_h: CELL_H,
                strip_rows,
                band_top_px: BAND_TOP,
                scale: 1.0,
                // No resolved seam (the hairline fallback); the seam tests pass one
                // (`win_geometry`).
                seam_top_px: None,
                foot_px: 0,
            }
        }

        fn plain(n: usize) -> Vec<TabStripMetadata> {
            vec![
                TabStripMetadata {
                    icon: None,
                    dirty: false,
                    busy: false,
                    attention: ChipLevel::Off,
                    conn: None,
                    closable: true,
                    team: TabTeam::NONE,
                    load: 0,
                };
                n
            ]
        }

        fn band<'a>(
            segments: &'a [TabSegment],
            titles: &'a [String],
            metadata: &'a [TabStripMetadata],
            paint: StripPaint<'a>,
            geometry: BandGeometry,
        ) -> BandInput<'a> {
            BandInput {
                segments,
                titles,
                metadata,
                paint,
                active: 0,
                palette: Theme::default().into(),
                geometry,
                floor: StripFloor::Ground,
            }
        }

        /// Decode the band image behind a ref list: `(rgba, width, height)`.
        fn image_of(rows: &[Vec<(usize, ImageRef)>]) -> (Vec<u8>, usize, usize) {
            let image = &rows[0][0].1.image;
            let ImageFormat::RawRgba8 { width, height } = image.format else {
                panic!("the band is a RawRgba8 raster");
            };
            (image.bytes.clone(), usize::from(width), usize::from(height))
        }

        /// Inked bounding box `(x0, y0, x1, y1)` (exclusive) over `[px0, px1)`.
        fn ink_bbox(
            rgba: &[u8],
            w: usize,
            h: usize,
            px0: usize,
            px1: usize,
        ) -> Option<(usize, usize, usize, usize)> {
            let mut bbox: Option<(usize, usize, usize, usize)> = None;
            for y in 0..h {
                for x in px0..px1.min(w) {
                    if rgba[(y * w + x) * 4 + 3] > 0 {
                        bbox = Some(match bbox {
                            None => (x, y, x + 1, y + 1),
                            Some((a, b, c, d)) => (a.min(x), b.min(y), c.max(x + 1), d.max(y + 1)),
                        });
                    }
                }
            }
            bbox
        }

        /// [`ink_bbox`] for the opaque band canvas: a pixel is ink when its
        /// distance from every listed surface tone exceeds a threshold above the
        /// largest surface-to-surface AA blend (~34/channel band→card), so rounded
        /// corners never read as ink.
        fn ink_bbox_off_surfaces(
            rgba: &[u8],
            w: usize,
            h: usize,
            px0: usize,
            px1: usize,
            surfaces: &[[u8; 3]],
        ) -> Option<(usize, usize, usize, usize)> {
            let far = |px: &[u8], s: &[u8; 3]| {
                px[0]
                    .abs_diff(s[0])
                    .max(px[1].abs_diff(s[1]))
                    .max(px[2].abs_diff(s[2]))
                    > 40
            };
            let mut bbox: Option<(usize, usize, usize, usize)> = None;
            for y in 0..h {
                for x in px0..px1.min(w) {
                    let px = &rgba[(y * w + x) * 4..(y * w + x) * 4 + 3];
                    if surfaces.iter().all(|s| far(px, s)) {
                        bbox = Some(match bbox {
                            None => (x, y, x + 1, y + 1),
                            Some((a, b, c, d)) => (a.min(x), b.min(y), c.max(x + 1), d.max(y + 1)),
                        });
                    }
                }
            }
            bbox
        }

        /// Install the host UI faces for this thread: `false` when the host has no
        /// system UI face, where the band declines and the test ends COULD NOT RUN.
        fn with_ui_faces() -> bool {
            crate::tray_raster::prepare_ui_fonts_for_direct_view_test();
            crate::tray_raster::strip_band_ui_ready()
        }

        #[test]
        fn declines_without_a_ui_face_so_the_cell_strip_is_untouched() {
            crate::tray_raster::clear_ui_fonts_for_test();
            let metadata = plain(2);
            let segments = layout_segments_with_metadata(80, 2, &metadata, 0, false);
            let titles = vec!["alpha".to_string(), "beta".to_string()];
            let input = band(
                &segments,
                &titles,
                &metadata,
                StripPaint::default(),
                geometry(80, 1),
            );
            assert!(raster_band(&input, &[]).is_none());
        }

        /// A two-tab strip at `cols` with the version label measured at this
        /// module's fixture metrics, and the label's segment.
        fn version_strip(
            cols: u16,
            attention: bool,
        ) -> (Vec<TabSegment>, Vec<TabStripMetadata>, TabSegment) {
            let metadata = plain(2);
            let slot = version_slot_cols(CELL_W, CELL_H + BAND_TOP, 1.0, attention);
            let segments = layout_segments_with_version(cols, 2, &metadata, 0, false, slot);
            let label = segments
                .iter()
                .copied()
                .find(|s| matches!(s.kind, TabHit::Version { .. }))
                .expect("the label fits beside two tabs at this width");
            (segments, metadata, label)
        }

        /// The slot is the label's measured width rounded up to cells: label plus
        /// pad on each side fits in `w` cells and not in `w - 1`, at every scale, for
        /// both rungs, with and without the update mark. No face or no cell width,
        /// no slot.
        #[test]
        fn version_slot_cols_cover_the_painted_width() {
            use crate::menu::{VersionRung, strip_version_label};
            if !with_ui_faces() {
                aterm_spec::host::could_not_run("no system UI face here for the strip band");
            }
            for scale in [1.0f32, 1.25, 1.5, 2.0] {
                let cell_w = (CELL_W as f32 * scale).round() as usize;
                let band_h = ((CELL_H + BAND_TOP) as f32 * scale).round() as usize;
                let label_px = band_label_px(band_h as f32, scale);
                let pad = version_pad_px(scale);
                for attention in [false, true] {
                    let slot = version_slot_cols(cell_w, band_h, scale, attention);
                    assert_eq!(slot.attention, attention);
                    for (rung, w) in [
                        (VersionRung::Full, slot.full),
                        (VersionRung::Short, slot.short),
                    ] {
                        let label = strip_version_label(rung, attention).expect("a dev build");
                        let need =
                            crate::tray_raster::ui_text_width_for(TextFace::Ui, &label, label_px)
                                + 2.0 * pad;
                        let px = |cells: u16| f32::from(cells) * cell_w as f32;
                        assert!(
                            px(w) >= need,
                            "{rung:?} at {scale}x: {w} cells ({} px) < {need} px",
                            px(w)
                        );
                        assert!(
                            px(w - 1) < need,
                            "{rung:?} at {scale}x: {w} cells is more than the ceiling of {need} px"
                        );
                    }
                    // The short rung drops the full one's `.<commits>`, so it
                    // never takes more cells (a short label that grew past the
                    // full one would). Not always fewer: this binary's stamp
                    // counts one commit, and `.1` can round into the same
                    // cells (Cantarell, Linux's face without Noto, at 1.5×:
                    // 98.1 and 111.3 px are both 8 cells of 14). Each rung's
                    // own count is pinned above.
                    assert!(slot.short <= slot.full, "the short rung is never longer");
                }
            }
            assert_eq!(
                version_slot_cols(0, CELL_H + BAND_TOP, 1.0, false),
                VersionSlotCols::NONE,
                "no cell width, no slot"
            );
            crate::tray_raster::clear_ui_fonts_for_test();
            assert_eq!(
                version_slot_cols(CELL_W, CELL_H + BAND_TOP, 1.0, false),
                VersionSlotCols::NONE,
                "no UI face, no slot — the first frames keep today's strip"
            );
        }

        /// THE BAND DRAWS THE LABEL: it covers the slot's columns and inks them.
        #[test]
        fn the_band_draws_the_version_label_in_the_ui_face() {
            if !with_ui_faces() {
                aterm_spec::host::could_not_run("no system UI face here for the strip band");
            }
            let (segments, metadata, label) = version_strip(132, false);
            let titles = vec!["Hello".to_string(), "World".to_string()];
            let input = band(
                &segments,
                &titles,
                &metadata,
                StripPaint::default(),
                geometry(132, 1),
            );
            let rows = raster_band(&input, &[]).expect("UI faces installed ⇒ a band");
            let covered: Vec<usize> = rows[0].iter().map(|(c, _)| *c).collect();
            assert!(
                (label.start_col..label.end_col).all(|c| covered.contains(&usize::from(c))),
                "the band owns the label's columns"
            );
            let (rgba, w, h) = image_of(&rows);
            let colors = strip_colors_with_active(Theme::default(), None);
            let (seg0, seg1) = (
                usize::from(label.start_col) * CELL_W,
                usize::from(label.end_col) * CELL_W,
            );
            assert!(
                ink_bbox_off_surfaces(&rgba, w, h, seg0, seg1, &[colors.band_bg]).is_some(),
                "the label inks its slot"
            );
            crate::tray_raster::clear_ui_fonts_for_test();
        }

        /// The pixel at `(x, y)` of a band canvas.
        fn pixel(rgba: &[u8], w: usize, x: usize, y: usize) -> [u8; 3] {
            let i = (y * w + x) * 4;
            [rgba[i], rgba[i + 1], rgba[i + 2]]
        }

        fn near(a: [u8; 3], b: [u8; 3]) -> bool {
            a.iter().zip(b).all(|(p, q)| p.abs_diff(q) <= 1)
        }

        /// THE WINDOW BUTTONS (`crate::client_frame`, LW.5): each a round button
        /// on the quiet chip's surface at rest and the band's hover wash under the
        /// pointer, centred in its slot, its mark at the centre in the button's
        /// ink; outside the disc the slot is bare band.
        #[test]
        fn window_buttons_are_round_and_take_the_hover_wash() {
            if !with_ui_faces() {
                aterm_spec::host::could_not_run("no system UI face here for the strip band");
            }
            let metadata = plain(1);
            let frame = crate::client_frame::ClientFrame {
                minimize: true,
                maximize: true,
                window_menu: true,
                maximized: false,
                edges: true,
                header: true,
                layout: crate::client_frame::ButtonLayout::FALLBACK,
            };
            let caption = CaptionSlot::measure(Some(frame), CELL_W, 1.0);
            let segments = layout_segments_with_caption(
                80,
                1,
                &metadata,
                0,
                false,
                VersionSlotCols::NONE,
                caption,
            );
            let close = segments.last().copied().expect("the close button");
            assert_eq!(close.kind, TabHit::Caption(CaptionButton::Close));
            let titles = vec!["one".to_string()];
            let colors = strip_colors_with_active(Theme::default(), None);
            for (hovered, want) in [
                (None, colors.chip_bg),
                (Some(CaptionButton::Close), colors.hover_bg),
                // Another button's hover leaves this one at rest.
                (Some(CaptionButton::Minimize), colors.chip_bg),
            ] {
                let paint = StripPaint {
                    caption_hovered: hovered,
                    ..StripPaint::default()
                };
                let input = band(&segments, &titles, &metadata, paint, geometry(80, 1));
                let rows = raster_band(&input, &[]).expect("band");
                let (rgba, w, h) = image_of(&rows);
                let cx = (usize::from(close.start_col) + usize::from(close.end_col)) * CELL_W / 2;
                // Inside the disc, clear of the cross: the button's surface.
                let got = pixel(&rgba, w, cx - 5, h / 2);
                assert!(near(got, want), "{hovered:?}: {got:?} vs {want:?}");
                // The cross's centre: the button's ink, not its surface.
                let mark = pixel(&rgba, w, cx, h / 2);
                assert!(!near(mark, want), "{hovered:?}: no mark at the centre");
                // The slot's edge, outside the disc: bare band.
                let edge = pixel(&rgba, w, usize::from(close.start_col) * CELL_W + 1, h / 2);
                assert!(
                    near(edge, colors.band_bg),
                    "{edge:?} vs {:?}",
                    colors.band_bg
                );
            }
            crate::tray_raster::clear_ui_fonts_for_test();
        }

        #[test]
        fn covers_every_column_with_one_band_image_of_the_strip_footprint() {
            if !with_ui_faces() {
                aterm_spec::host::could_not_run("no system UI face here for the strip band");
            }
            let metadata = plain(2);
            let segments = layout_segments_with_metadata(80, 2, &metadata, 0, false);
            let titles = vec!["alpha".to_string(), "beta".to_string()];
            let input = band(
                &segments,
                &titles,
                &metadata,
                StripPaint::default(),
                geometry(80, 1),
            );
            let rows = raster_band(&input, &[]).expect("UI faces installed ⇒ a band");
            assert_eq!(rows.len(), 1, "one ref list per strip row");
            let cols: Vec<usize> = rows[0].iter().map(|(c, _)| *c).collect();
            assert_eq!(
                cols,
                (0..80).collect::<Vec<_>>(),
                "every column covered, sorted"
            );
            let first = &rows[0][0].1.image;
            assert!(
                rows[0].iter().all(|(_, r)| Arc::ptr_eq(&r.image, first)),
                "one shared image Arc for the whole band"
            );
            assert!(
                rows[0]
                    .iter()
                    .all(|(c, r)| usize::from(r.cell_col) == *c && r.cell_row == 0),
                "each ref names its own tile"
            );
            let (rgba, w, h) = image_of(&rows);
            // The canvas is the cell footprint plus the chrome lip, carried as the
            // image's band lift so the renderers seat it at the window top.
            let lift = BAND_TOP;
            assert_eq!((w, h), (80 * CELL_W, CELL_H + lift), "canvas vs footprint");
            assert_eq!((first.cols, first.rows, first.z_index), (80, 1, 0));
            assert_eq!(usize::from(first.band_lift_px), lift);
            // Design lane: opaque, so no cell background shows under a rounded card.
            assert!(
                (0..w * h).all(|i| rgba[i * 4 + 3] == 255),
                "an opaque bar has no see-through pixels"
            );

            // A two-row strip: one ref list per row over one taller image.
            let one = plain(1);
            let segments = layout_segments_with_metadata(60, 1, &one, 0, false);
            let titles = vec!["only".to_string()];
            let input = band(
                &segments,
                &titles,
                &one,
                StripPaint::default(),
                geometry(60, 2),
            );
            let rows = raster_band(&input, &[]).expect("band");
            assert_eq!(rows.len(), 2);
            assert!(rows[1].iter().all(|(_, r)| r.cell_row == 1));
            let (_, w, h) = image_of(&rows);
            assert_eq!((w, h), (60 * CELL_W, 2 * CELL_H + lift));
            crate::tray_raster::clear_ui_fonts_for_test();
        }

        #[test]
        fn rename_segment_is_left_uncovered_for_the_cell_well() {
            if !with_ui_faces() {
                aterm_spec::host::could_not_run("no system UI face here for the strip band");
            }
            let metadata = plain(2);
            let segments = layout_segments_with_metadata(80, 2, &metadata, 0, false);
            let titles = vec!["alpha".to_string(), "beta".to_string()];
            let paint = StripPaint {
                hovered: None,
                new_tab_hovered: false,
                version_hovered: false,
                caption_hovered: None,
                subtitle: None,
                rename: Some(StripRenameField {
                    tab: 0,
                    text: "alp",
                    cursor: 3,
                }),
            };
            let input = band(&segments, &titles, &metadata, paint, geometry(80, 1));
            let rows = raster_band(&input, &[]).expect("band");
            let edited = segments[0];
            let covered: Vec<usize> = rows[0].iter().map(|(c, _)| *c).collect();
            for col in usize::from(edited.start_col)..usize::from(edited.end_col) {
                assert!(
                    !covered.contains(&col),
                    "col {col} belongs to the rename well"
                );
            }
            let other = segments[1];
            assert!(covered.contains(&usize::from(other.start_col + 1)));
            crate::tray_raster::clear_ui_fonts_for_test();
        }

        #[test]
        fn uncoverable_title_falls_back_per_segment_and_keeps_its_icon_rasters() {
            if !with_ui_faces() {
                aterm_spec::host::could_not_run("no system UI face here for the strip band");
            }
            let metadata = plain(2);
            let segments = layout_segments_with_metadata(80, 2, &metadata, 0, false);
            let titles = vec!["\u{1F680} rocket".to_string(), "beta".to_string()];
            let input = band(
                &segments,
                &titles,
                &metadata,
                StripPaint::default(),
                geometry(80, 1),
            );
            // A cell-lane icon raster under the fallback segment must survive; one
            // under a pixel segment is dropped (the band bakes those itself).
            let icon = Arc::new(ImageData {
                bytes: vec![0; 4],
                format: ImageFormat::RawRgba8 {
                    width: 1,
                    height: 1,
                },
                cols: 1,
                rows: 1,
                z_index: 0,
                band_lift_px: 0,
                cell_offset_px: (0, 0),
                scaling: ImageScaling::Fit,
                source_rect: None,
            });
            let mk = |col: u16| {
                (
                    usize::from(col),
                    ImageRef {
                        image: Arc::clone(&icon),
                        cell_row: 0,
                        cell_col: 0,
                        kitty: None,
                    },
                )
            };
            let kept_col = segments[0].start_col + 1;
            let dropped_col = segments[1].start_col + 1;
            let legacy = vec![mk(kept_col), mk(dropped_col)];
            let rows = raster_band(&input, &legacy).expect("band");
            let find = |col: u16| {
                rows[0]
                    .iter()
                    .find(|(c, _)| *c == usize::from(col))
                    .map(|(_, r)| Arc::ptr_eq(&r.image, &icon))
            };
            assert_eq!(
                find(kept_col),
                Some(true),
                "fallback segment keeps its icon"
            );
            assert_eq!(
                find(dropped_col),
                Some(false),
                "pixel segment is band-covered"
            );
            let mut sorted = rows[0].iter().map(|(c, _)| *c).collect::<Vec<_>>();
            sorted.dedup();
            assert!(
                sorted.windows(2).all(|p| p[0] < p[1]),
                "refs stay sorted by column"
            );
            crate::tray_raster::clear_ui_fonts_for_test();
        }

        /// A CJK title stays in the pixel band: with the terminal's parsed chain
        /// face admitted as the chrome's coverage rung, it is one image over its
        /// columns, ideographs centred with their Latin neighbour and clear of the
        /// card's bottom edge (not dropped to the cell baseline under the seam).
        #[test]
        fn a_cjk_title_stays_in_the_band_and_centres_with_its_latin_neighbour() {
            if !crate::tray_raster::install_chrome_fonts_with_cjk_chain_for_test()
                || !with_ui_faces()
            {
                crate::tray_raster::clear_ui_fonts_for_test();
                aterm_spec::host::could_not_run("no system UI face here for the strip band");
            }
            let title = "日本語 shell";
            assert!(
                crate::tray_raster::strip_band_run_coverable(title),
                "the chrome stack draws every char of the title"
            );
            let metadata = plain(2);
            let segments = layout_segments_with_metadata(80, 2, &metadata, 0, false);
            let titles = vec!["alpha".to_string(), title.to_string()];
            let input = band(
                &segments,
                &titles,
                &metadata,
                StripPaint::default(),
                geometry(80, 1),
            );
            let rows = raster_band(&input, &[]).expect("band");
            let (rgba, w, h) = image_of(&rows);
            let seg = segments[1];
            let probe = usize::from(seg.start_col) + 1;
            assert!(
                rows[0]
                    .iter()
                    .any(|(c, r)| *c == probe && Arc::ptr_eq(&r.image, &rows[0][0].1.image)),
                "the CJK segment is band-covered, not a fallback range"
            );
            // Where the pen put the label: the band places the run in its span
            // ([`label_x`]) by the very measure the pen advances with.
            let layout = tab_content_layout(&seg, metadata[1]);
            let label_px = band_label_px(h as f32, 1.0);
            let measure =
                |s: &str| crate::tray_raster::ui_text_width_for(TextFace::Ui, s, label_px);
            let x0 = f32::from(layout.title_start) * CELL_W as f32;
            let span_px = f32::from(layout.title_end - layout.title_start) * CELL_W as f32 - 1.0;
            let x = label_x(x0, span_px, measure(title));
            let cjk_end = x + measure("日本語");
            let latin_start = x + measure("日本語 ");
            let colors = strip_colors_with_active(Theme::default(), None);
            let surfaces = [colors.band_bg, colors.active_bg, colors.chip_bg];
            let cjk = ink_bbox_off_surfaces(
                &rgba,
                w,
                h,
                x.floor() as usize,
                cjk_end.ceil() as usize,
                &surfaces,
            )
            .expect("the ideographs ink");
            let latin = ink_bbox_off_surfaces(
                &rgba,
                w,
                h,
                latin_start.floor() as usize,
                usize::from(layout.title_end) * CELL_W,
                &surfaces,
            )
            .expect("the Latin inks");
            let centre = |b: (usize, usize, usize, usize)| (b.1 + b.3) as f32 * 0.5;
            assert!(
                (centre(cjk) - centre(latin)).abs() <= 2.0,
                "CJK ink {cjk:?} centres at {} while the Latin's {latin:?} centres at {}",
                centre(cjk),
                centre(latin)
            );
            let d = BandDesign::resolve(&input.geometry, h, label_px);
            assert!(
                (cjk.3 as f32) <= d.card_bot - 1.0,
                "CJK ink {cjk:?} reaches the card's bottom edge {}",
                d.card_bot
            );
            assert!(
                (cjk.1 as f32) >= d.card_top,
                "CJK ink {cjk:?} escapes the card's top edge {}",
                d.card_top
            );
            crate::tray_raster::clear_ui_fonts_for_test();
        }

        /// A COLOUR-EMOJI TITLE IS A BAND TAB ([`emoji_label`]): the band covers
        /// its whole segment, draws its tab (on Windows the selected tab opening
        /// through the seam into the grid) and its label, the emoji in colour
        /// from the terminal's own colour face beside the UI-face letters.
        /// NEGATIVE CONTROL: with no colour face in the chrome stack (the
        /// semantic renderer not landed) the title falls to the cell lane, the
        /// square monospace slab the 2026-10-18 judges found
        /// (`emoji-title-tab-slab-in-new-strip`).
        #[test]
        fn a_colour_emoji_title_is_a_band_tab() {
            if !STRIP_WINDOWS_PAINT {
                return;
            }
            let renderer = aterm_render::Renderer::from_bytes(
                aterm_render::embedded_font(),
                14.0,
                Theme::default(),
            )
            .expect("embedded renderer");
            crate::tray_raster::install_settled_chrome_fonts_for_test(renderer);
            if !with_ui_faces() {
                aterm_spec::host::could_not_run("no system UI face here for the strip band");
            }
            let label_px = band_label_px((BAND_TOP + CELL_H) as f32, 1.0);
            if crate::tray_raster::chrome_colour_emoji("\u{1F680}", emoji_box_px(label_px))
                .is_none()
            {
                crate::tray_raster::clear_ui_fonts_for_test();
                aterm_spec::host::could_not_run("no colour emoji face here for the chrome");
            }
            let title = "\u{1F680} deploy";
            assert!(
                !crate::tray_raster::strip_band_run_coverable(title),
                "CONTROL: the UI stack cannot draw the rocket"
            );
            let metadata = plain(3);
            let segments = layout_segments_with_metadata(90, 3, &metadata, 1, false);
            let titles = vec!["build".to_string(), title.to_string(), "logs".to_string()];
            let input = BandInput {
                active: 1,
                ..band(
                    &segments,
                    &titles,
                    &metadata,
                    StripPaint::default(),
                    geometry(90, 1),
                )
            };
            let rows = raster_band(&input, &[]).expect("band");
            let covered = |col: u16| rows[0].iter().any(|(c, _)| *c == usize::from(col));
            let seg = segments[1];
            assert!(
                (seg.start_col..seg.end_col).all(covered),
                "the emoji tab's every column is the band's"
            );
            let (rgba, w, h) = image_of(&rows);
            let layout = tab_content_layout(&seg, metadata[1]);
            let colourful = (0..h)
                .flat_map(|y| {
                    (usize::from(layout.title_start) * CELL_W
                        ..usize::from(layout.title_end) * CELL_W)
                        .map(move |x| (x, y))
                })
                .filter(|&(x, y)| {
                    let [r, g, b] = pixel(&rgba, w, x, y);
                    r.max(g).max(b) - r.min(g).min(b) > 60
                })
                .count();
            assert!(
                colourful > 20,
                "the rocket is drawn in colour: {colourful} px"
            );
            let colors = strip_colors_for(input.palette);
            let inside = usize::from(seg.start_col) * CELL_W + 4;
            assert_eq!(
                pixel(&rgba, w, inside, h - 1),
                colors.active_bg,
                "the selected emoji tab is the tab, to the band's last row"
            );
            // NEGATIVE CONTROL: no colour face, the cell lane's title.
            let bare = aterm_render::Renderer::from_bytes(
                aterm_render::embedded_font(),
                14.0,
                Theme::default(),
            )
            .map(|mut renderer| {
                renderer.set_runtime_font_discovery(false);
                renderer
            })
            .expect("embedded renderer");
            crate::tray_raster::install_settled_chrome_fonts_for_test(bare);
            let rows = raster_band(&input, &[]).expect("band");
            if crate::tray_raster::chrome_colour_emoji("\u{1F680}", emoji_box_px(label_px))
                .is_none()
            {
                assert!(
                    !rows[0]
                        .iter()
                        .any(|(c, _)| *c == usize::from(seg.start_col) + 1),
                    "CONTROL: without a colour face the title is the cell lane's"
                );
            }
            crate::tray_raster::clear_ui_fonts_for_test();
        }

        /// A CJK title NO face maps takes the cell lane: with no chain face in
        /// the cascade at all, and none of the UI faces' own, the band has
        /// nothing to parse for it and leaves its segment to the cell painter
        /// (the twin of
        /// `a_cjk_title_is_a_band_tab_on_its_first_frame_with_its_hairlines`,
        /// whose cascade has the faces, unparsed).
        #[test]
        fn a_cjk_title_no_face_maps_falls_to_the_cell_lane() {
            // A settled cascade with no chain face: the embedded primary alone.
            let mut renderer = aterm_render::Renderer::from_bytes(
                aterm_render::embedded_font(),
                14.0,
                Theme::default(),
            )
            .expect("embedded renderer");
            renderer.set_runtime_font_discovery(false);
            crate::tray_raster::install_settled_chrome_fonts_for_test(renderer);
            if !with_ui_faces() {
                aterm_spec::host::could_not_run("no system UI face here for the strip band");
            }
            crate::tray_raster::set_ui_cjk_faces_for_test(None);
            let title = "日本語 shell";
            assert!(
                !crate::tray_raster::strip_band_run_coverable(title),
                "no loaded face carries the ideographs yet"
            );
            let metadata = plain(2);
            let segments = layout_segments_with_metadata(80, 2, &metadata, 0, false);
            let titles = vec!["alpha".to_string(), title.to_string()];
            let input = band(
                &segments,
                &titles,
                &metadata,
                StripPaint::default(),
                geometry(80, 1),
            );
            let rows = raster_band(&input, &[]).expect("band");
            let covered = |col: u16| rows[0].iter().any(|(c, _)| *c == usize::from(col));
            assert!(
                covered(segments[0].start_col + 1),
                "the Latin segment is band-covered"
            );
            assert!(
                !covered(segments[1].start_col + 1),
                "the CJK segment is left to the cell lane"
            );
            crate::tray_raster::clear_ui_fonts_for_test();
        }

        /// EACH LABEL'S EAST ASIAN FACES ARE ITS OWN WEIGHT'S (the 2026-10-18
        /// judges' `j4t-cjk-band-labels-bold`: a quiet `日本語` and `한국어` in
        /// Microsoft YaHei Bold and Malgun Gothic Bold beside Segoe UI Regular).
        /// On the first raster of a strip whose SELECTED tab is `日本語 notes`
        /// and whose quiet one is `한국어`, over a cold store of the UI faces'
        /// own fallback, the band has exactly the faces those labels draw with
        /// parsed: Yu Gothic UI Semibold for the selected label's ideographs,
        /// Malgun Gothic (Regular) for the quiet label's Hangul, and neither
        /// one's other weight. Both titles are band titles.
        #[cfg(windows)]
        #[test]
        fn each_labels_east_asian_faces_are_its_own_weights() {
            let Some(_live) = crate::tray_raster::install_unparsed_cjk_chain_for_test() else {
                aterm_spec::host::could_not_run("no CJK fallback chain could be installed here");
            };
            let Some(store) = crate::tray_raster::host_ui_cjk_faces_for_test() else {
                crate::tray_raster::clear_ui_fonts_for_test();
                aterm_spec::host::could_not_run("no host UI CJK faces here");
            };
            crate::tray_raster::set_ui_cjk_faces_for_test(Some(Arc::clone(&store)));
            if !with_ui_faces() {
                aterm_spec::host::could_not_run("no system UI face here for the strip band");
            }
            let (geometry, _) = window_band(100);
            let metadata = plain(3);
            let segments = layout_segments_with_metadata(100, 3, &metadata, 0, false);
            let titles = vec![
                "日本語 notes".to_string(),
                "한국어".to_string(),
                "logs".to_string(),
            ];
            let input = band(
                &segments,
                &titles,
                &metadata,
                StripPaint::default(),
                geometry,
            );
            assert_eq!(input.selected(), 0, "the CJK tab is the selected one");
            assert!(
                store.parsed_files_for_test().is_empty(),
                "CONTROL: a cold store"
            );
            let rows = raster_band(&input, &[]).expect("band");
            assert_eq!(
                store.parsed_files_for_test(),
                ["malgun.ttf#0", "YuGothB.ttc#2"],
                "the quiet Hangul's Regular face and the selected ideographs' Semibold one"
            );
            for tab in [0, 1] {
                let seg = segments
                    .iter()
                    .find(|s| matches!(s.kind, TabHit::Select(i) if i == tab))
                    .expect("the tab");
                assert!(
                    rows[0]
                        .iter()
                        .any(|(c, _)| *c == usize::from(seg.start_col) + 1),
                    "tab {tab} is a band title"
                );
            }
            crate::tray_raster::clear_ui_fonts_for_test();
        }

        /// The real window's band (2 px pad, a 13 px head, one 23 px row, the
        /// 6 px foot) at `cols`, and the y the hairlines are centred on.
        fn window_band(cols: usize) -> (BandGeometry, usize) {
            let (band_top, cell_h, foot) = (15usize, 23usize, 6usize);
            let h = band_top + cell_h;
            let geometry = BandGeometry {
                cols,
                cell_w: CELL_W,
                cell_h,
                strip_rows: 1,
                band_top_px: band_top,
                scale: 1.0,
                seam_top_px: Some(h - strip_seam_px(1.0) - foot),
                foot_px: foot,
            };
            let label_px = band_label_px(h as f32, 1.0);
            let d = BandDesign::resolve(&geometry, h, label_px);
            (geometry, (d.baseline - label_px * 0.35).round() as usize)
        }

        /// THE FIRST FRAME OF A CJK TITLE IS A BAND TAB (the 2026-10-18 Fluent
        /// judges' `F3-cjk-title-tab-cell-lane-first-paint`: on the first paint
        /// `日本語 notes` was the cell lane's, in the terminal's bold face and
        /// size, with no hairline on either side). The cascade holds the host's
        /// CJK faces UNPARSED, as a fresh window's does (the chain's, and on
        /// Windows the UI faces' own), and the very first raster of the strip
        /// draws the title in the band, the face its quiet label draws with
        /// parsed for it, parted from its quiet neighbours by a hairline on
        /// each side. NEGATIVE CONTROL: before the raster the label's routing
        /// does not reach the ideograph.
        #[test]
        fn a_cjk_title_is_a_band_tab_on_its_first_frame_with_its_hairlines() {
            let Some(_live) = crate::tray_raster::install_unparsed_cjk_chain_for_test() else {
                aterm_spec::host::could_not_run("no CJK fallback chain could be installed here");
            };
            if !with_ui_faces() {
                aterm_spec::host::could_not_run("no system UI face here for the strip band");
            }
            assert!(
                !crate::tray_raster::chrome_ui_reaches_for_test(TextFace::Ui, '日'),
                "CONTROL: the CJK face is not parsed before the first frame"
            );
            let (geometry, cy) = window_band(100);
            let metadata = plain(4);
            let segments = layout_segments_with_metadata(100, 4, &metadata, 0, false);
            let titles = vec![
                "alpha".to_string(),
                "beta".to_string(),
                "日本語 notes".to_string(),
                "gamma".to_string(),
            ];
            let input = band(
                &segments,
                &titles,
                &metadata,
                StripPaint::default(),
                geometry,
            );
            let rows = raster_band(&input, &[]).expect("band");
            let cjk = segments
                .iter()
                .find(|s| matches!(s.kind, TabHit::Select(2)))
                .copied()
                .expect("the CJK tab");
            assert!(
                (cjk.start_col..cjk.end_col)
                    .all(|col| rows[0].iter().any(|(c, r)| *c == usize::from(col)
                        && Arc::ptr_eq(&r.image, &rows[0][0].1.image))),
                "the CJK tab's every column is the band's on its first frame"
            );
            assert!(
                crate::tray_raster::chrome_ui_reaches_for_test(TextFace::Ui, '日'),
                "the band had the face its label needs parsed"
            );
            if SELECTED_TAB_IS_GROUND {
                let (rgba, w, _) = image_of(&rows);
                let seam = strip_colors_for_on(input.palette, input.floor)
                    .seam
                    .expect("a seam");
                for boundary in [cjk.start_col, cjk.end_col] {
                    assert_eq!(
                        pixel(&rgba, w, usize::from(boundary) * CELL_W, cy),
                        seam,
                        "a hairline at column {boundary}"
                    );
                }
            }
            crate::tray_raster::clear_ui_fonts_for_test();
        }

        /// A TITLE THE CELL LANE DRAWS KEEPS ITS HAIRLINES. A title no face maps
        /// (a private-use char) is the cell lane's, whose columns are not the
        /// band's; the hairline between it and each quiet neighbour lies wholly
        /// in the neighbour's band column, against the boundary, so the cell
        /// lane drops none of it. NEGATIVE CONTROL: the 2026-10-18 band drew
        /// hairlines only between two tabs it drew itself.
        #[test]
        fn quiet_neighbours_of_a_cell_lane_title_keep_their_hairlines() {
            if !SELECTED_TAB_IS_GROUND {
                return;
            }
            // A settled cascade with no chain face: the embedded primary alone.
            let mut renderer = aterm_render::Renderer::from_bytes(
                aterm_render::embedded_font(),
                14.0,
                Theme::default(),
            )
            .expect("embedded renderer");
            renderer.set_runtime_font_discovery(false);
            crate::tray_raster::install_settled_chrome_fonts_for_test(renderer);
            if !with_ui_faces() {
                aterm_spec::host::could_not_run("no system UI face here for the strip band");
            }
            let (geometry, cy) = window_band(100);
            let metadata = plain(4);
            let segments = layout_segments_with_metadata(100, 4, &metadata, 0, false);
            let titles = vec![
                "alpha".to_string(),
                "beta".to_string(),
                "logs \u{10FFFD}".to_string(),
                "gamma".to_string(),
            ];
            assert!(!crate::tray_raster::strip_band_run_coverable(&titles[2]));
            let input = band(
                &segments,
                &titles,
                &metadata,
                StripPaint::default(),
                geometry,
            );
            let rows = raster_band(&input, &[]).expect("band");
            let covered = |col: usize| rows[0].iter().any(|(c, _)| *c == col);
            let lane = segments
                .iter()
                .find(|s| matches!(s.kind, TabHit::Select(2)))
                .copied()
                .expect("the cell lane's tab");
            let (start, end) = (usize::from(lane.start_col), usize::from(lane.end_col));
            assert!(!covered(start) && !covered(end - 1), "the cell lane's tab");
            let (rgba, w, _) = image_of(&rows);
            let seam = strip_colors_for_on(input.palette, input.floor)
                .seam
                .expect("a seam");
            // Before the cell lane's tab: the last pixel column of `beta`.
            let before = start * CELL_W - 1;
            // After it: the first pixel column of `gamma`.
            let after = end * CELL_W;
            for x in [before, after] {
                assert!(covered(x / CELL_W), "x={x} is a band column");
                assert_eq!(pixel(&rgba, w, x, cy), seam, "a hairline at x={x}");
            }
            assert_eq!(hairline_x(90.0, 2.0, true, true), Some(89.0));
            assert_eq!(hairline_x(90.0, 2.0, true, false), Some(88.0));
            assert_eq!(hairline_x(90.0, 2.0, false, true), Some(90.0));
            assert_eq!(hairline_x(90.0, 2.0, false, false), None);
            crate::tray_raster::clear_ui_fonts_for_test();
        }

        /// A title the chrome cascade covers but the variable face's cmap does not
        /// (`✓`, a Nerd-Font PUA icon) draws in full on the active chip through the
        /// `UiBold` per-char fallback, never as `prefix…` or a lone `…`.
        #[test]
        fn active_label_the_variable_face_cannot_shape_draws_whole_through_the_cascade() {
            if !with_ui_faces() {
                aterm_spec::host::could_not_run("no system UI face here for the strip band");
            }
            let Some(vf) = crate::tray_raster::strip_band_variable_semibold() else {
                // Static UI regular: there is no variable path to regress.
                crate::tray_raster::clear_ui_fonts_for_test();
                return;
            };
            // A char outside the variable face's cmap that the chrome cascade draws
            // as real ink, probed rather than assumed, so a future face that covers
            // it makes the test end COULD NOT RUN rather than lie.
            let odd = [
                '\u{2713}', '\u{2717}', '\u{2423}', '\u{2318}', '\u{23CE}', '\u{2500}', '\u{2588}',
                '\u{2591}',
            ]
            .into_iter()
            .find(|&ch| {
                !vf.cmap.maps(ch) && crate::tray_raster::strip_band_run_coverable(&ch.to_string())
            });
            let Some(odd) = odd else {
                crate::tray_raster::clear_ui_fonts_for_test();
                aterm_spec::host::could_not_run(
                    "no char here that the cascade draws and the variable face lacks",
                );
            };
            let title = format!("Alpha{odd}Beta");
            // The decision in isolation: the whole run refuses the variable
            // face, while the fontdue cascade covers every char of it.
            assert!(shape_variable_label(&vf, &title, 13.0).is_none());
            assert!(crate::tray_raster::strip_band_run_coverable(&title));
            let metadata = plain(2);
            let segments = layout_segments_with_metadata(80, 2, &metadata, 0, false);
            let titles = vec![title, "Beta".to_string()];
            let input = band(
                &segments,
                &titles,
                &metadata,
                StripPaint::default(),
                geometry(80, 1),
            );
            let rows = raster_band(&input, &[]).expect("band");
            let (rgba, w, h) = image_of(&rows);
            let layout = tab_content_layout(&segments[0], metadata[0]);
            let span = (
                usize::from(layout.title_start) * CELL_W,
                usize::from(layout.title_end) * CELL_W,
            );
            let (x0, _, x1, _) = ink_bbox(&rgba, w, h, span.0, span.1).expect("active ink");
            assert!(
                x0 >= span.0 && x1 <= span.1,
                "ink stays in span: {x0}..{x1} vs {span:?}"
            );
            let label_px = band_label_px((BAND_TOP + CELL_H) as f32, 1.0);
            // The label's ink is at least as wide as its Latin stem in semibold, and
            // well past what a truncated `Alpha…` would ink.
            let ink_w = (x1 - x0) as f32;
            let stem =
                crate::tray_raster::ui_text_width_for(TextFace::UiBold, "AlphaBeta", label_px);
            let cut = crate::tray_raster::ui_text_width_for(TextFace::UiBold, "Alpha…", label_px);
            assert!(
                ink_w + 2.0 >= stem,
                "ink {ink_w}px must cover the whole label (stem {stem}px)"
            );
            assert!(
                ink_w > cut + 4.0,
                "ink {ink_w}px must exceed the truncated `Alpha…` ({cut}px)"
            );
            crate::tray_raster::clear_ui_fonts_for_test();
        }

        /// The glyph-level fit ([`VariablePen::fit`]) places every glyph where
        /// shaping [`fit_label`]'s string would, across nothing fits, a few chars,
        /// one short, exact and over-fit, and with a cell-truncation `…` to drop.
        #[test]
        fn glyph_level_fit_matches_the_string_fit_glyph_for_glyph() {
            if !with_ui_faces() {
                aterm_spec::host::could_not_run("no system UI face here for the strip band");
            }
            let Some(vf) = crate::tray_raster::strip_band_variable_semibold() else {
                crate::tray_raster::clear_ui_fonts_for_test();
                return;
            };
            let pen = VariablePen::new(&vf).expect("the retained bytes parse");
            let px = 13.0;
            for label in [
                "Windows Terminal Session Title",
                "Overflowing cell cut…",
                "W",
            ] {
                let full = pen.shape(label, px).expect("Latin shapes");
                let spans = [
                    4.0,
                    20.0,
                    55.0,
                    full.width - 0.5,
                    full.width,
                    full.width + 10.0,
                ];
                for span in spans {
                    let expected_text = fit_label(label.to_string(), span, |s| {
                        pen.shape(s, px).map_or(f32::INFINITY, |run| run.width)
                    });
                    let expected = pen
                        .shape(&expected_text, px)
                        .expect("the fitted string shapes");
                    let actual = pen
                        .fit(pen.shape(label, px).expect("shapes"), span, px)
                        .expect("fits");
                    let actual_text: String = actual.cuts.iter().map(|cut| cut.ch).collect();
                    assert_eq!(actual_text, expected_text, "{label:?} at {span}px");
                    assert_eq!(
                        actual.glyphs.len(),
                        expected.glyphs.len(),
                        "{label:?} at {span}px"
                    );
                    for (a, e) in actual.glyphs.iter().zip(&expected.glyphs) {
                        assert_eq!(
                            (a.x(), a.w, a.h, a.ymin),
                            (e.x(), e.w, e.h, e.ymin),
                            "{label:?} at {span}px"
                        );
                        assert_eq!(a.cov, e.cov, "{label:?} at {span}px");
                    }
                    assert!(
                        (actual.width - expected.width).abs() < 1e-3,
                        "{label:?} at {span}px: {} vs {}",
                        actual.width,
                        expected.width
                    );
                    assert!(
                        actual.width <= span || expected_text == "…",
                        "{label:?} at {span}px: fitted run {}px overflows",
                        actual.width
                    );
                }
            }
            crate::tray_raster::clear_ui_fonts_for_test();
        }

        /// A tab's want ([`tab_wants`]) seats its whole title, laid out and
        /// labelled: the band draws the title uncut in a tab of that width. A
        /// spinner's frames (`✳ `, `✶ `, `· `, a braille frame) want the same
        /// width, so a spinning title never moves its `✕`; a short title wants
        /// less than the equal tab and keeps it. NEGATIVE CONTROL: at the fixed
        /// 27 columns the same title is cut.
        #[test]
        fn a_tabs_want_seats_its_title_and_ignores_a_spinner() {
            if !STRIP_WINDOWS_PAINT {
                return;
            }
            if !with_ui_faces() {
                aterm_spec::host::could_not_run("no system UI face here for the strip band");
            }
            let band_h = BAND_TOP + CELL_H;
            let title = "Claude Code: fix the strip floor under splits".to_string();
            // The frames this host's chrome stack draws (a frame it cannot draw
            // sends the title to the cell lane, which wants nothing).
            let spun: Vec<String> = ["\u{2733}", "\u{2736}", "\u{b7}", "\u{280b}"]
                .iter()
                .map(|frame| format!("{frame} Fix the strip floor under splits"))
                .filter(|title| crate::tray_raster::strip_band_run_coverable(title))
                .collect();
            assert!(spun.len() >= 2, "CONTROL: two frames to compare");
            // Every frame is measured as one stand-in, whatever its own advance;
            // a title that merely starts with a letter or a path is not touched.
            for frame in ["\u{2733}", "\u{2736}", "\u{b7}", "\u{280b}", "*"] {
                assert_eq!(
                    steady_title(&format!("{frame} Fix the strip")),
                    "M Fix the strip",
                    "the {frame:?} frame"
                );
            }
            for plain in ["logs", "~/src/aterm", "C:\\Users", "1 build"] {
                assert_eq!(steady_title(plain), plain);
            }
            let wants = tab_wants(&spun, &plain(spun.len()), CELL_W, band_h, 1.0);
            assert!(
                wants.windows(2).all(|pair| pair[0] == pair[1]),
                "a spinner's frames move the tab: {wants:?}"
            );
            let short = tab_wants(&["logs".to_string()], &plain(1), CELL_W, band_h, 1.0);
            assert!(short[0] <= MAX_CHIP_COLS, "a short title wants {short:?}");
            let want = tab_wants(std::slice::from_ref(&title), &plain(1), CELL_W, band_h, 1.0)[0];
            assert!(want > MAX_CHIP_COLS, "CONTROL: the title is long: {want}");
            let label = |sizing: TabSizing<'_>| {
                let metadata = plain(1);
                let titles = vec![title.clone()];
                let segments = layout_segments_with_version_sized(
                    120,
                    1,
                    &metadata,
                    0,
                    false,
                    VersionSlotCols::NONE,
                    sizing,
                );
                let input = band(
                    &segments,
                    &titles,
                    &metadata,
                    StripPaint::default(),
                    geometry(120, 1),
                );
                let label_px = band_label_px(band_h as f32, 1.0);
                resolve_band_labels(&input, 0, 120, CELL_W as f32, label_px, true)[0]
                    .clone()
                    .expect("a label")
            };
            let wants = [want];
            assert_eq!(
                label(TabSizing {
                    cap: MAX_CHIP_COLS,
                    grow: 48,
                    wants: &wants
                }),
                title,
                "the grown tab seats the whole title"
            );
            assert_ne!(label(TabSizing::FIXED), title, "CONTROL: 27 columns cut it");
            crate::tray_raster::clear_ui_fonts_for_test();
        }

        /// The selected title through the variable semibold pen reads at one weight
        /// in a dark and a light theme: its mean gamma-space coverage on its tab
        /// (each ink pixel's step from the tab toward the ink, in sRGB luma) agrees
        /// within 5% between Default and Default Light, as every other chrome
        /// glyph's does. NEGATIVE CONTROL: composited uncorrected, the light title
        /// measured 0.54 against the dark's 0.68 (the 2026-10-18 judges'
        /// `light-selected-tab-label-uncorrected-alpha`).
        #[test]
        fn the_selected_title_keeps_its_weight_on_light_and_dark_tabs() {
            if !with_ui_faces() {
                aterm_spec::host::could_not_run("no system UI face here for the strip band");
            }
            if crate::tray_raster::strip_band_variable_semibold().is_none() {
                // Static UI regular: the selected title takes the corrected pen.
                crate::tray_raster::clear_ui_fonts_for_test();
                return;
            }
            let luma = |c: [u8; 3]| {
                0.2126 * f32::from(c[0]) + 0.7152 * f32::from(c[1]) + 0.0722 * f32::from(c[2])
            };
            let mean_coverage = |theme: Theme| {
                let metadata = plain(2);
                let segments = layout_segments_with_metadata(80, 2, &metadata, 0, false);
                let titles = vec!["~\\aterm-winperfect".to_string(), "logs".to_string()];
                let input = BandInput {
                    palette: theme.into(),
                    ..band(
                        &segments,
                        &titles,
                        &metadata,
                        StripPaint::default(),
                        geometry(80, 1),
                    )
                };
                let colors = strip_colors_for(input.palette);
                let rows = raster_band(&input, &[]).expect("band");
                let (rgba, w, h) = image_of(&rows);
                let layout = tab_content_layout(&segments[0], metadata[0]);
                let (ground, ink) = (luma(colors.active_bg), luma(colors.active_fg));
                // Inside the tab only: the band above it is not ink, nor is
                // the seam-tone rim along a light tab's top
                // ([`selected_tab_rim`]).
                let d = BandDesign::resolve(&geometry(80, 1), h, band_label_px(h as f32, 1.0));
                let rim = if colors.active_rim.is_some() {
                    strip_seam_px(1.0)
                } else {
                    0
                };
                let (mut sum, mut n) = (0.0f32, 0usize);
                for y in (d.card_top.ceil() as usize + rim)..(d.card_bot.floor() as usize).min(h) {
                    for x in usize::from(layout.title_start) * CELL_W
                        ..usize::from(layout.title_end) * CELL_W
                    {
                        let px = pixel(&rgba, w, x, y);
                        let t = (luma(px) - ground) / (ink - ground);
                        if t > 0.02 {
                            sum += t.min(1.0);
                            n += 1;
                        }
                    }
                }
                assert!(n > 50, "the selected title inked {n} px");
                sum / n as f32
            };
            let dark = mean_coverage(Theme::default());
            let light = mean_coverage(Theme::from(
                &aterm_types::scheme::builtin("Default Light").expect("builtin"),
            ));
            assert!(
                (dark - light).abs() <= 0.05 * dark.max(light),
                "the selected title's weight differs by theme: dark {dark:.3}, light {light:.3}"
            );
            crate::tray_raster::clear_ui_fonts_for_test();
        }

        /// The band's title window and span for one segment, measured as
        /// [`resolve_band_labels`] does, without metadata (layout numbers only).
        fn window(seg: &TabSegment) -> (u16, f32) {
            let layout = band_content_layout(seg, None, seg.end_col);
            let avail = layout.title_end.saturating_sub(layout.title_start);
            (avail, f32::from(avail) * CELL_W as f32 - 1.0)
        }

        /// A UI face whose every char out-measures the mono cell by half: a closed
        /// form of a wide title, so these laws hold on hosts with no system font.
        fn wide_face(_tab: usize, s: &str) -> f32 {
            s.chars().count() as f32 * (CELL_W as f32 * 1.5)
        }

        /// The pixel fit must not collapse labels the cell pass
        /// ([`distinct_chip_labels`]) made distinct, whether they arrive distinct
        /// (four titles that differ only at their end, whole in cells, which the
        /// per-chip head fit collapses to one `service-…`) or already ordinal-ed
        /// (ten shells in one cwd): no two chips paint one string, the active chip
        /// is never re-cut, a chip that leads with digits leads with its OWN
        /// position (never `1…` cut from `10 · …`), and every answer fits its span.
        /// (A tail cut keeps its end through the fit, so prompts no longer collide.)
        #[test]
        fn the_pixel_fit_cannot_undo_the_cell_pass_distinctness() {
            let distinct: Vec<String> = ["alpha", "beta", "gamma", "delta"]
                .iter()
                .map(|leaf| format!("service-{leaf}"))
                .collect();
            let identical: Vec<String> = (0..10)
                .map(|_| "user@m17-tower: ~/aterm · Typing a command".to_string())
                .collect();
            for (case, titles, widths) in [
                ("distinct", distinct, &[80u16][..]),
                ("identical", identical, &[80, 130, 200][..]),
            ] {
                let metadata = plain(titles.len());
                for &cols in widths {
                    let at = format!("{case} at {cols} cols");
                    let segments =
                        layout_segments_with_metadata(cols, titles.len(), &metadata, 0, false);
                    let cell = distinct_chip_labels(&segments, &titles, Some(&metadata), 0, None);
                    let entries: Vec<BandLabel> = segments
                        .iter()
                        .filter_map(|seg| match seg.kind {
                            TabHit::Select(i) => Some(BandLabel {
                                tab: i,
                                span_px: window(seg).1,
                                text: cell[i].clone().expect("a seated chip is labelled"),
                            }),
                            _ => None,
                        })
                        .collect();
                    let naive: Vec<String> = entries
                        .iter()
                        .map(|entry| {
                            fit_label(entry.text.clone(), entry.span_px, |s| {
                                wide_face(entry.tab, s)
                            })
                        })
                        .collect();
                    if case == "distinct" {
                        assert!(
                            naive.iter().enumerate().any(|(a, x)| {
                                naive.iter().enumerate().any(|(b, y)| a != b && x == y)
                            }),
                            "fixture: the naive per-chip fit must collide: {naive:?}"
                        );
                    }
                    let fitted = fit_labels_distinctly(&entries, 0, &[], &wide_face);
                    let is_distinct = case == "distinct";
                    if is_distinct {
                        assert_eq!(fitted.len(), titles.len(), "{at}: every chip resolved");
                        assert_eq!(fitted[0].1, naive[0], "{at}: the ACTIVE chip is re-cut");
                    }
                    for (n, (tab, text)) in fitted.iter().enumerate() {
                        assert!(!is_distinct || !text.is_empty(), "{at}: tab {tab} is mute");
                        for (m, (_, other)) in fitted.iter().enumerate() {
                            assert!(
                                n == m || (!is_distinct && text.is_empty()) || text != other,
                                "{at}: two chips paint {text:?}"
                            );
                        }
                        if *tab != 0 && !text.is_empty() {
                            let position = (tab + 1).to_string();
                            let digits: String =
                                text.chars().take_while(char::is_ascii_digit).collect();
                            assert!(
                                if is_distinct {
                                    *text == naive[n] || text.starts_with(&position)
                                } else {
                                    digits == position || label_says_nothing(text)
                                },
                                "{at}: tab {tab} paints {text:?}"
                            );
                        }
                        assert!(
                            wide_face(*tab, text) <= entries[n].span_px || text == "…",
                            "{at}: {text:?} would need a SECOND fit at paint time"
                        );
                    }
                }
            }
        }

        /// Each repair [`fit_labels_distinctly`] owns, from the string the shipped fit
        /// left: a selection the fit stranded takes its ordinal (or keeps the mark
        /// when not even its number fits); a `……` stub is repaired like the bare
        /// mark; every quiet twin is numbered, the first painted too (an uncontested
        /// name is left alone); a title that reads as a number gives it back; a
        /// cell-painter fallback still counts as a name.
        #[test]
        fn the_pixel_fit_repairs_every_label_that_names_nothing_or_another_tab() {
            // A window that seats the number and not a character more.
            let squeeze = wide_face(0, "……");
            // FIXTURES: the exact strings the shipped per-chip fit leaves.
            assert!(label_says_nothing(&fit_label(
                "…-676-2".to_string(),
                squeeze,
                |s| wide_face(0, s)
            )));
            assert_eq!(
                fit_label("…and".to_string(), squeeze, |s| wide_face(1, s)),
                "…",
                "a tail cut keeps its end down to three characters, then the mark"
            );
            assert_eq!(
                fit_label(
                    "…\\work\\sub dir".to_string(),
                    wide_face(0, "…sub dir"),
                    |s| { wide_face(0, s) }
                ),
                "…sub dir",
                "a tail cut sheds from the front, keeping what tells it apart"
            );
            assert_eq!(
                fit_label(
                    "2 · …\\work\\sub dir".to_string(),
                    wide_face(0, "2 · …sub dir"),
                    |s| { wide_face(0, s) }
                ),
                "2 · …sub dir",
                "behind its ordinal too"
            );
            assert_eq!(
                fit_label(
                    "Thinking… (esc to interrupt)".to_string(),
                    wide_face(0, "Thinking… (esc…"),
                    |s| wide_face(0, s)
                ),
                "Thinking… (esc…",
                "a title's own `…` is no cut: it keeps its head"
            );
            let mono = |_tab: usize, s: &str| s.chars().count() as f32 * CELL_W as f32;
            let twin_span = mono(0, "13 · ~\\aterm") + 1.0;
            let distinct = |case: &str, fitted: &[(usize, String)]| {
                for (n, (tab, text)) in fitted.iter().enumerate() {
                    for (m, (_, other)) in fitted.iter().enumerate() {
                        assert!(
                            n == m || text != other,
                            "{case}: tab {tab} doubles: {fitted:?}"
                        );
                    }
                }
            };
            type Check<'a> = &'a dyn Fn(&str, &[(usize, String)], &[BandLabel]);
            // (case, face, [(tab, span, cell label)], active, fallbacks, check).
            type Case<'a> = (
                &'a str,
                &'a dyn Fn(usize, &str) -> f32,
                Vec<(usize, f32, &'a str)>,
                usize,
                &'a [&'a str],
                Check<'a>,
            );
            let cases: [Case; 7] = [
                (
                    "stranded selection",
                    &wide_face,
                    vec![(0, squeeze, "…-676-2"), (1, squeeze, "…-676-2")],
                    0,
                    &[],
                    &|case, fitted, entries| {
                        assert_eq!(fitted[0].1, "1", "{case}: {fitted:?}");
                        distinct(case, fitted);
                        for ((tab, text), entry) in fitted.iter().zip(entries) {
                            assert!(wide_face(*tab, text) <= entry.span_px, "{case}: {text:?}");
                        }
                    },
                ),
                (
                    "no room for the number",
                    &wide_face,
                    vec![(9, wide_face(9, "1") * 0.5, "…-676-2")],
                    9,
                    &[],
                    &|case, fitted, _| {
                        assert!(label_says_nothing(&fitted[0].1), "{case}: {fitted:?}")
                    },
                ),
                (
                    "stub of marks",
                    &wide_face,
                    vec![
                        (0, 200.0, "…alpha"),
                        (1, squeeze, "…and"),
                        (2, 200.0, "…gamma"),
                    ],
                    0,
                    &[],
                    &|case, fitted, entries| {
                        assert_eq!(fitted[1].1, "2", "{case}: {fitted:?}");
                        for ((tab, text), entry) in fitted.iter().zip(entries) {
                            assert!(
                                !label_says_nothing(text)
                                    || wide_face(*tab, &(tab + 1).to_string()) > entry.span_px,
                                "{case}: tab {tab} paints {text:?} where its number fits"
                            );
                        }
                    },
                ),
                (
                    "a title that reads as a number",
                    &wide_face,
                    vec![(0, 200.0, "…zulu"), (1, 200.0, "3"), (2, squeeze, "…and")],
                    0,
                    &[],
                    &|case, fitted, _| {
                        distinct(case, fitted);
                        assert_eq!(fitted[2].1, "3", "{case}: {fitted:?}");
                        assert!(fitted[1].1.starts_with('2'), "{case}: {fitted:?}");
                    },
                ),
                (
                    "a cell-painter fallback",
                    &wide_face,
                    vec![(1, 200.0, "…~/aterm"), (2, 200.0, "…$HOME/trust")],
                    0,
                    &["…~/aterm"],
                    &|case, fitted, _| {
                        assert!(fitted[0].1.starts_with('2'), "{case}: {fitted:?}");
                        assert_eq!(fitted[1].1, "…$HOME/trust", "{case}: untouched");
                    },
                ),
                (
                    "quiet twins",
                    &mono,
                    vec![
                        (5, twin_span, "…gui\\src"),
                        (8, twin_span, "~\\aterm"),
                        (11, twin_span, "~\\aterm"),
                        (12, twin_span, "~\\aterm"),
                        (13, twin_span, "C:\\Temp"),
                    ],
                    5,
                    &[],
                    &|case, fitted, _| {
                        let labels: Vec<&str> = fitted.iter().map(|(_, t)| t.as_str()).collect();
                        assert_eq!(
                            labels,
                            [
                                "…gui\\src",
                                "9 · ~\\aterm",
                                "12 · ~\\aterm",
                                "13 · ~\\aterm",
                                "C:\\Temp"
                            ],
                            "{case}"
                        );
                    },
                ),
                (
                    "an uncontested name",
                    &mono,
                    vec![(5, twin_span, "…gui\\src"), (8, twin_span, "~\\aterm")],
                    5,
                    &[],
                    &|case, fitted, _| assert_eq!(fitted[1].1, "~\\aterm", "{case}"),
                ),
            ];
            for (case, face, entries, active, fallbacks, check) in cases {
                let entries: Vec<BandLabel> = entries
                    .into_iter()
                    .map(|(tab, span_px, text)| BandLabel {
                        tab,
                        span_px,
                        text: text.to_string(),
                    })
                    .collect();
                let fitted = fit_labels_distinctly(&entries, active, fallbacks, face);
                check(case, &fitted, &entries);
            }
        }

        /// Titles are cut for the UI face on Windows, not for terminal cells (Linux
        /// keeps its cell cut): a short title in a chip the cells call too narrow
        /// is whole (`Settings`, not `Settin…`), and a path the UI-widened budget
        /// would keep whole but the face cannot fit is cut by the cell pass at a
        /// narrower step, so both twins keep the directory (`…\work\sub dir`), the
        /// quiet one behind its ordinal, rather than a pixel trim of the path's
        /// tail.
        #[test]
        fn chip_titles_are_cut_for_the_ui_face_and_paths_keep_their_end() {
            if !with_ui_faces() {
                crate::tray_raster::clear_ui_fonts_for_test();
                aterm_spec::host::could_not_run("no system UI face here for the strip band");
            }
            let label_px = 13.0;
            if !STRIP_WINDOWS_PAINT {
                assert_eq!(
                    ui_cells_per_cell(CELL_W as f32, label_px),
                    1.0,
                    "Linux keeps cutting titles in terminal cells"
                );
                crate::tray_raster::clear_ui_fonts_for_test();
                return;
            }
            assert!(
                ui_cells_per_cell(CELL_W as f32, label_px) > 1.2,
                "the UI face sets more than a character per 9 px cell"
            );
            // Two 12-column chips: seven cells of title room, which cut `Settings`
            // to `Settin…` in cells, though it measures well inside 62 px.
            let titles = vec!["Settings".to_string(), "x".to_string()];
            let metadata = plain(2);
            let segments = layout_segments_with_metadata(27, 2, &metadata, 0, false);
            let input = band(
                &segments,
                &titles,
                &metadata,
                StripPaint::default(),
                geometry(27, 1),
            );
            let resolved = resolve_band_labels(&input, 0, 27, CELL_W as f32, label_px, true);
            assert_eq!(resolved[0].as_deref(), Some("Settings"), "{resolved:?}");

            let dir = r"C:\Users\m6-an\AppData\Local\Temp\wp-chrome2\work\sub dir".to_string();
            let titles = vec![dir.clone(), dir];
            let segments = layout_segments_with_metadata(100, 2, &metadata, 0, false);
            let input = band(
                &segments,
                &titles,
                &metadata,
                StripPaint::default(),
                geometry(100, 1),
            );
            let resolved = resolve_band_labels(&input, 0, 100, CELL_W as f32, label_px, true);
            for (i, label) in resolved.iter().enumerate() {
                let label = label.as_deref().expect("a painted chip has a label");
                assert!(label.ends_with(r"\sub dir"), "tab {i}: {label:?}");
            }
            assert!(
                resolved[1]
                    .as_deref()
                    .is_some_and(|l| l.starts_with("2 · ")),
                "the quiet twin is numbered: {resolved:?}"
            );
            crate::tray_raster::clear_ui_fonts_for_test();
        }

        /// EACH TAB IS CUT AT ITS OWN WIDEST STEP ([`widest_fitting_cuts`],
        /// font-free): a label that overruns its span at a step waits for a
        /// narrower one, and a label that fits keeps the step it fits at, however
        /// far a neighbour has to narrow. At the floor (1.0) every label left is
        /// taken as it is. NEGATIVE CONTROL: one step for the whole strip cut the
        /// fitting tab at the wide tab's step too.
        #[test]
        fn each_tab_is_cut_at_its_own_widest_step() {
            // Two tabs of 20 cells; the cell pass at `widen` keeps that many
            // characters of each title, the rest behind `…`.
            let wide = "W".repeat(40);
            let titles = [wide.as_str(), "Claude Code: reviewing the strip"];
            let labels_at = |widen: f32| -> Vec<Option<String>> {
                // The steps are tenths; read them as such, not a hair under.
                let keep = (20.0 * widen + 0.001).floor() as usize;
                titles
                    .iter()
                    .map(|title| {
                        Some(if title.chars().count() <= keep {
                            (*title).to_string()
                        } else {
                            title.chars().take(keep - 1).collect::<String>() + "…"
                        })
                    })
                    .collect()
            };
            // `W` is twice as wide as the other letters.
            let width = |s: &str| {
                s.chars()
                    .map(|c| if c == 'W' { 14.0 } else { 7.0 })
                    .sum::<f32>()
            };
            let overflows = |_: usize, s: &str, span: f32| width(s) > span;
            // Tab 0 overruns 200 px at every step and is taken at the floor;
            // tab 1 fits 250 px at the first step, 1.5, and keeps it.
            let cut = widest_fitting_cuts(&[(0, 200.0), (1, 250.0)], 1.5, labels_at, overflows);
            assert_eq!(cut.len(), 2);
            let floor = format!("{}…", "W".repeat(19));
            assert_eq!(
                cut[0].as_deref(),
                Some(floor.as_str()),
                "the floor: {cut:?}"
            );
            assert_eq!(
                cut[1].as_deref(),
                Some("Claude Code: reviewing the st…"),
                "the fitting tab keeps its own step"
            );
            // The strip-wide step would have cut it with tab 0, at the floor.
            assert_eq!(
                labels_at(1.0)[1].as_deref(),
                Some("Claude Code: review…"),
                "CONTROL"
            );
            // A label that first fits at a middle step (1.2, 24 cells) stops
            // there.
            let middle = widest_fitting_cuts(&[(1, 180.0)], 1.5, labels_at, overflows);
            assert_eq!(
                middle[1].as_deref(),
                Some("Claude Code: reviewing …"),
                "{middle:?}"
            );
            // A tab the pass does not seat gets nothing.
            assert_eq!(middle[0], None);
        }

        /// A NEIGHBOUR'S TITLE COSTS A TAB NO CHARACTERS (with the UI faces): at
        /// 200% (17 px cells, a 26 px label) in a five-tab strip, the Claude Code
        /// task's label, quiet or selected, reads the same beside a
        /// `C:\Users\m6-an\aterm` tab as beside a `logs` tab, and the same
        /// quiet as selected. NEGATIVE CONTROL:
        /// with one step for the whole strip the path tab's overrun cut it to
        /// `Claude Code: rev…` (the 2026-10-18 verifier's
        /// `A11-hidpi-selected-label-fit`, measured live at 150% and 200%).
        #[test]
        fn a_neighbours_title_costs_a_tab_no_characters() {
            if !STRIP_WINDOWS_PAINT {
                crate::tray_raster::clear_ui_fonts_for_test();
                return;
            }
            if !with_ui_faces() {
                crate::tray_raster::clear_ui_fonts_for_test();
                aterm_spec::host::could_not_run("no system UI face here for the strip band");
            }
            let claude = "Claude Code: reviewing platform_win.rs and the Default Light contrast of bright white on blue";
            let (cw, label_px) = (17.0, 26.0);
            let metadata = plain(5);
            let label_of = |first: &str, active: usize| {
                let titles: Vec<String> = [first, "build", claude, "wsl ~/src", "logs"]
                    .iter()
                    .map(|s| (*s).to_string())
                    .collect();
                let segments = layout_segments_with_metadata(100, 5, &metadata, active, false);
                let input = band(
                    &segments,
                    &titles,
                    &metadata,
                    StripPaint::default(),
                    geometry(100, 1),
                );
                resolve_band_labels(&input, active, 100, cw, label_px, true)[2]
                    .clone()
                    .expect("the Claude tab is labelled")
            };
            for active in [0, 2] {
                let beside_logs = label_of("logs-old", active);
                let beside_path = label_of(r"C:\Users\m6-an\aterm", active);
                assert_eq!(beside_path, beside_logs, "active {active}");
                assert!(
                    beside_logs.starts_with("Claude Code: revi"),
                    "{beside_logs:?}"
                );
            }
            // Its step is chosen in the selected weight, so selecting it
            // changes none of its words.
            assert_eq!(
                label_of(r"C:\Users\m6-an\aterm", 0),
                label_of(r"C:\Users\m6-an\aterm", 2)
            );
            crate::tray_raster::clear_ui_fonts_for_test();
        }

        /// Band label plumbing end to end, no fonts needed: every painted chip gets
        /// a resolved label, the inline rename segment gets none, and no two chips
        /// share a string.
        #[test]
        fn the_band_resolves_one_label_per_painted_chip() {
            let titles: Vec<String> = ["alpha", "beta", "gamma"]
                .iter()
                .map(|leaf| format!("user@m17-tower: ~/work/service-{leaf}"))
                .collect();
            let metadata = plain(titles.len());
            let segments = layout_segments_with_metadata(80, titles.len(), &metadata, 0, false);
            let paint = StripPaint {
                rename: Some(StripRenameField {
                    tab: 2,
                    text: "renaming",
                    cursor: 0,
                }),
                ..StripPaint::default()
            };
            let input = band(&segments, &titles, &metadata, paint, geometry(80, 1));
            let resolved =
                resolve_band_labels(&input, input.selected(), 80, CELL_W as f32, 13.0, true);
            assert_eq!(resolved.len(), titles.len());
            assert!(
                resolved[2].is_none(),
                "the rename well is the cell painter's: {resolved:?}"
            );
            for (i, label) in resolved.iter().enumerate().take(2) {
                let label = label.as_deref().expect("a painted chip has a label");
                assert!(!label.is_empty(), "tab {i} says nothing");
                for (j, other) in resolved.iter().enumerate().take(2) {
                    assert!(
                        i == j || Some(label) != other.as_deref(),
                        "two chips resolved to {label:?}"
                    );
                }
            }
        }

        /// A CUT THAT KEEPS ITS END KEEPS IT THROUGH THE PIXEL FIT ([`fit_label`],
        /// font-free): a command cut's head gives way a character at a time,
        /// then the whole head, then the file's own head behind a leading mark,
        /// so `…platform_win.rs` outlives `vim`. Only such a cut (and a tail
        /// cut) is re-cut this way where the variable pen overruns its span
        /// ([`cut_keeps_its_end`]); a head cut and a title that says `…` itself
        /// keep [`VariablePen::fit`]'s head cut.
        #[test]
        fn the_pixel_fit_keeps_a_command_cuts_file() {
            let measure = |s: &str| s.chars().count() as f32 * 7.0;
            let label = format!("vim -n -R{MIDDLE_MARK}src/platform_win.rs");
            assert_eq!(fit_label(label.clone(), 300.0, measure), label);
            for (span, want) in [
                (210.0, "vim -n - … src/platform_win.rs"),
                (180.0, "vim … src/platform_win.rs"),
                (140.0, "…src/platform_win.rs"),
                (100.0, "…atform_win.rs"),
            ] {
                let fitted = fit_label(label.clone(), span, measure);
                assert_eq!(fitted, want, "{span} px");
                assert!(measure(&fitted) <= span, "{fitted:?} in {span} px");
            }
            for (label, keeps) in [
                ("vim -n … src/x.rs", true),
                ("…src/x.rs", true),
                ("2 · …src/x.rs", true),
                ("vim -n -R -i NO…", false),
                ("Thinking… (esc to interrupt)", false),
                ("logs", false),
                ("…", false),
            ] {
                assert_eq!(cut_keeps_its_end(label), keeps, "{label:?}");
            }
        }

        /// `(band_top_px, cell_h, underline_y)` for the optical bands the Windows
        /// metrics produce at 96 dpi for four font sizes. The synthetic head
        /// keeps the band 32 px, so `band_top` shrinks as `cell_h` grows. On
        /// Windows the seam is the band's last pixel row (31) at every size; on
        /// Linux it is the cell underline ([`seal_strip_bottom`]), at the seam
        /// row below ([`band_seam_top`]):
        ///
        /// | `font_px` | cell   | `pad_top + head` | Linux seam row |
        /// |----------:|:-------|-----------------:|---------------:|
        /// | 11        | 6×13   | 2 + 17 = 19      | 29             |
        /// | 13        | 8×15   | 2 + 15 = 17      | 29             |
        /// | 16        | 9×19   | 2 + 11 = 13      | 28             |
        /// | 17        | 9×23   | 2 + 7  = 9       | 27             |
        const WIN_BANDS: [(usize, usize, usize); 4] =
            [(19, 13, 10), (17, 15, 12), (13, 19, 15), (9, 23, 18)];

        /// The seam's canvas row for a [`WIN_BANDS`] band, as the splice resolves
        /// it ([`BandGeometry::seam_top_px`]).
        fn band_seam_top((band_top, cell_h, underline_y): (usize, usize, usize)) -> usize {
            if STRIP_WINDOWS_PAINT {
                band_top + cell_h - strip_seam_px(1.0)
            } else {
                band_top + underline_y
            }
        }

        /// A one-window band at a [`WIN_BANDS`] row.
        fn win_geometry(cols: usize, win: (usize, usize, usize)) -> BandGeometry {
            let (band_top, cell_h, _) = win;
            BandGeometry {
                cols,
                cell_w: CELL_W,
                cell_h,
                strip_rows: 1,
                band_top_px: band_top,
                scale: 1.0,
                seam_top_px: Some(band_seam_top(win)),
                foot_px: 0,
            }
        }

        /// At every Windows band, the label run sits inside its card and its
        /// descenders clear the seam: on the band's last row, which the canvas
        /// draws, on Windows; the cell lane's underline, which it does not, on
        /// Linux.
        #[test]
        fn the_label_stays_in_its_card_with_descender_room_at_every_font_px() {
            if !with_ui_faces() {
                aterm_spec::host::could_not_run("no system UI face here for the strip band");
            }
            let colors = strip_colors_with_active(Theme::default(), None);
            for win in WIN_BANDS {
                let (band_top, cell_h, _) = win;
                let seam_top = band_seam_top(win);
                let metadata = plain(2);
                let segments = layout_segments_with_metadata(80, 2, &metadata, 0, false);
                // An ascender and a descender in one run, so the bbox is the label's
                // true optical extent rather than an x-height band.
                let titles = vec!["Apply".to_string(), "gypsy".to_string()];
                let geometry = win_geometry(80, win);
                let input = band(
                    &segments,
                    &titles,
                    &metadata,
                    StripPaint::default(),
                    geometry,
                );
                let rows = raster_band(&input, &[]).expect("band");
                let (rgba, w, h) = image_of(&rows);
                assert_eq!(
                    h,
                    band_top + cell_h,
                    "the canvas is the WHOLE optical band, lip included"
                );
                let seg = segments[0];
                let layout = tab_content_layout(&seg, metadata[0]);
                let span = (
                    usize::from(layout.title_start) * CELL_W,
                    usize::from(layout.title_end) * CELL_W,
                );
                let (_, y0, _, y1) = ink_bbox_off_surfaces(
                    &rgba,
                    w,
                    h,
                    span.0,
                    span.1,
                    &[colors.band_bg, colors.active_bg, colors.chip_bg],
                )
                .expect("the active title inks its span");
                let label_px = band_label_px(h as f32, 1.0);
                let d = BandDesign::resolve(&geometry, h, label_px);
                assert!(
                    y0 as f32 >= d.card_top - 1.0 && y1 as f32 <= d.card_bot + 1.0,
                    "{win:?}: ink {y0}..{y1} escapes its card {}..{}",
                    d.card_top,
                    d.card_bot
                );
                // Descender room: the last inked row clears the seam rule.
                assert!(
                    y1 < seam_top,
                    "{win:?}: descenders reach {y1}, seam rule at {seam_top}"
                );
                if d.tabs {
                    // A Windows tab stands on the seam, its title's ink box
                    // centred in it with air at both ends.
                    assert_eq!(d.card_bot, seam_top as f32, "{win:?}: on the seam");
                    let (above, below) = (y0 as f32 - d.card_top, d.card_bot - 1.0 - y1 as f32);
                    assert!(
                        above >= 1.0 && below >= 1.0 && (above - below).abs() <= 3.0,
                        "{win:?}: ink {y0}..={y1} in tab {}..{}: {above} above, {below} below",
                        d.card_top,
                        d.card_bot
                    );
                } else {
                    // The card row is centred in the band above the seam: as far
                    // below the window's top edge as above the rule.
                    let above = d.card_top;
                    let below = seam_top as f32 - d.card_bot;
                    assert!(
                        (above - below).abs() <= 1.0,
                        "{win:?}: card {}..{} is not centred over the seam at {seam_top}",
                        d.card_top,
                        d.card_bot
                    );
                }
                // On Windows the rule is the band's last row, in the seam tone,
                // edge to edge; on Linux the canvas leaves it to the cell lane.
                let seam = colors.seam.expect("a chrome band has a seam");
                for x in [0, w / 2, w - 1] {
                    let at = (seam_top * w + x) * 4;
                    assert_eq!(
                        &rgba[at..at + 3] == seam.as_slice(),
                        STRIP_WINDOWS_PAINT,
                        "{win:?}: seam at x={x}"
                    );
                }
            }
            crate::tray_raster::clear_ui_fonts_for_test();
        }

        /// THE WINDOWS TAB ROW (`SELECTED_TAB_IS_GROUND`), on the real window's
        /// band (2 px pad, a 13 px head, one 23 px row: the 32 px band and the
        /// 6 px foot): the selected tab is the terminal's ground and runs through
        /// the seam and the foot into the grid; a quiet tab has no card, the seam
        /// runs under it and the foot below; two quiet neighbours are parted by a
        /// hairline, a quiet tab and the selected one are not. On a light theme
        /// the selected tab is the band's LIGHTEST surface. NEGATIVE CONTROL: the
        /// 2026-10-18 strip painted every quiet tab a card and the selected one a
        /// capsule above an unbroken seam, the darkest chip of a light band.
        #[test]
        fn a_selected_tab_opens_into_the_grid_and_quiet_tabs_are_bare() {
            if !SELECTED_TAB_IS_GROUND {
                return;
            }
            if !with_ui_faces() {
                aterm_spec::host::could_not_run("no system UI face here for the strip band");
            }
            let (band_top, cell_h, foot) = (15usize, 23usize, 6usize);
            let h = band_top + cell_h;
            let seam_top = h - strip_seam_px(1.0) - foot;
            let geometry = BandGeometry {
                cols: 90,
                cell_w: CELL_W,
                cell_h,
                strip_rows: 1,
                band_top_px: band_top,
                scale: 1.0,
                seam_top_px: Some(seam_top),
                foot_px: foot,
            };
            for theme in [
                Theme::default(),
                Theme::from(&aterm_types::scheme::builtin("Solarized Light").expect("builtin")),
            ] {
                let colors = strip_colors_with_active(theme, None);
                let seam = colors.seam.expect("a seam");
                let metadata = plain(3);
                let segments = layout_segments_with_metadata(90, 3, &metadata, 0, false);
                let titles = vec!["Apply".to_string(), "gypsy".to_string(), "logs".to_string()];
                let input = band(
                    &segments,
                    &titles,
                    &metadata,
                    StripPaint::default(),
                    geometry,
                );
                let input = BandInput {
                    palette: theme.into(),
                    ..input
                };
                let rows = raster_band(&input, &[]).expect("band");
                let (rgba, w, img_h) = image_of(&rows);
                assert_eq!(img_h, h);
                let at = |x: usize, y: usize| pixel(&rgba, w, x, y);
                let mid = |i: usize| {
                    let s = segments[i];
                    (
                        usize::from(s.start_col) * CELL_W + 4,
                        usize::from(s.end_col) * CELL_W,
                    )
                };
                let (active_x, _) = mid(0);
                let (quiet_x, quiet_end) = mid(1);
                for y in [seam_top, seam_top + 1, h - 1] {
                    assert_eq!(at(active_x, y), colors.foot, "the selected tab at y={y}");
                }
                assert_eq!(
                    colors.active_bg, colors.foot,
                    "the selected tab IS the ground"
                );
                assert_eq!(at(quiet_x, seam_top), seam, "the seam under a quiet tab");
                assert_eq!(at(quiet_x, h - 1), colors.foot, "the foot");
                assert_eq!(
                    at(quiet_x, seam_top - 3),
                    colors.band_bg,
                    "a quiet tab has no card"
                );
                let d = BandDesign::resolve(&geometry, h, band_label_px(h as f32, 1.0));
                let cy = (d.baseline - band_label_px(h as f32, 1.0) * 0.35).round() as usize;
                assert_eq!(at(quiet_end, cy), seam, "a hairline between quiet tabs");
                let (_, active_end) = mid(0);
                assert_eq!(
                    at(active_end, cy),
                    colors.band_bg,
                    "none beside the selection"
                );
                let luma = |c: [u8; 3]| crate::chrome_band::contrast(c, [0, 0, 0]);
                if bg_is_light(colors.foot) {
                    assert!(
                        luma(colors.active_bg) > luma(colors.band_bg)
                            && luma(colors.hover_bg) < luma(colors.band_bg),
                        "light: the selected tab rises toward white, the hover toward the ink"
                    );
                }
            }
            crate::tray_raster::clear_ui_fonts_for_test();
        }

        /// THE SELECTED TAB IS ONE SURFACE WITH WHAT LIES UNDER THE SEAM
        /// ([`StripFloor`]), on the real window's band: over a native page it
        /// takes the page's surface and opens through the seam and the foot into
        /// it; over a band it cannot take (the find panel's, the message band's)
        /// it stands closed on the seam in the terminal's ground and the foot is
        /// the band's tone, so the panel under it is not fused with the tab;
        /// over the ground (the grid, a split's ground-wearing headers) it opens
        /// as before. NEGATIVE CONTROL: the 2026-10-18 band painted the ground's
        /// tab and foot over every floor: a dark notch over a lighter Settings
        /// page, an island in the grey header band, an L with the find well (the
        /// judges' `settings-tab-ground-mismatch`, `selected-tab-opens-onto-grey-band`).
        #[test]
        fn the_selected_tab_joins_what_lies_under_the_seam() {
            if !SELECTED_TAB_IS_GROUND {
                return;
            }
            if !with_ui_faces() {
                aterm_spec::host::could_not_run("no system UI face here for the strip band");
            }
            let (band_top, cell_h, foot) = (15usize, 23usize, 6usize);
            let h = band_top + cell_h;
            let seam_top = h - strip_seam_px(1.0) - foot;
            let geometry = BandGeometry {
                cols: 90,
                cell_w: CELL_W,
                cell_h,
                strip_rows: 1,
                band_top_px: band_top,
                scale: 1.0,
                seam_top_px: Some(seam_top),
                foot_px: foot,
            };
            for theme in [
                Theme::default(),
                Theme::from(&aterm_types::scheme::builtin("Default Light").expect("builtin")),
            ] {
                let ground = strip_colors(theme).foot;
                let band_bg = strip_colors(theme).band_bg;
                let page = crate::settings::Roles::from_theme(theme).surface;
                assert_ne!(page, ground, "CONTROL: the page is not the ground");
                let metadata = plain(3);
                let segments = layout_segments_with_metadata(90, 3, &metadata, 0, false);
                let titles = vec![
                    "Settings".to_string(),
                    "build".to_string(),
                    "logs".to_string(),
                ];
                let paint_over = |floor: StripFloor| {
                    let input = BandInput {
                        palette: theme.into(),
                        floor,
                        ..band(
                            &segments,
                            &titles,
                            &metadata,
                            StripPaint::default(),
                            geometry,
                        )
                    };
                    let rows = raster_band(&input, &[]).expect("band");
                    image_of(&rows)
                };
                let active_x = usize::from(segments[0].start_col) * CELL_W + 4;
                let quiet_x = usize::from(segments[1].start_col) * CELL_W + 4;
                let seam = strip_colors(theme).seam.expect("a seam");
                let inside = seam_top - 3;
                for (floor, tab, opens, under) in [
                    (StripFloor::Ground, ground, true, ground),
                    (StripFloor::Surface(ground), ground, true, ground),
                    (StripFloor::Surface(page), page, true, page),
                    (StripFloor::Surface(band_bg), ground, false, band_bg),
                ] {
                    let (rgba, w, _) = paint_over(floor);
                    let at = |x: usize, y: usize| pixel(&rgba, w, x, y);
                    assert_eq!(at(active_x, inside), tab, "{floor:?}: the selected tab");
                    let at_seam = if opens { tab } else { seam };
                    assert_eq!(at(active_x, seam_top), at_seam, "{floor:?}: under the tab");
                    assert_eq!(
                        at(active_x, h - 1),
                        under,
                        "{floor:?}: the foot under the tab"
                    );
                    assert_eq!(at(quiet_x, seam_top), seam, "{floor:?}: the seam elsewhere");
                    assert_eq!(at(quiet_x, h - 1), under, "{floor:?}: the foot elsewhere");
                }
            }
            crate::tray_raster::clear_ui_fonts_for_test();
        }

        /// A SELECTED TAB ON A LIGHT BAND WEARS THE SEAM ([`selected_tab_rim`]):
        /// over Default Light's Settings page (`#eaebeb` on its `#e2e3e5` band,
        /// 1.08:1) and over its grid (1.20:1) alike the selected tab keeps its
        /// floor's tone and opens into it, its sides and top a seam-tone
        /// hairline whose sides run down through the seam rows to meet the
        /// rule, the foot under it the floor. At 1x and 2x the hairline is the
        /// seam's own thickness. Over Default's page and grid the tab has no
        /// outline. NEGATIVE CONTROL: the 2026-10-18 band left the light
        /// Settings tab rimless, a shape only its missing seam drew (the
        /// judges' `j3d-light-settings-tab-faint`), and the next one rimmed it
        /// but not the light terminal tab (`J4F-4-light-selected-tab-two-idioms`).
        #[test]
        fn a_selected_tab_the_band_would_swallow_wears_the_seam() {
            if !SELECTED_TAB_IS_GROUND {
                return;
            }
            if !with_ui_faces() {
                aterm_spec::host::could_not_run("no system UI face here for the strip band");
            }
            let light =
                Theme::from(&aterm_types::scheme::builtin("Default Light").expect("builtin"));
            for (theme, page, rimmed) in [
                (light, true, true),
                (light, false, true),
                (Theme::default(), true, false),
                (Theme::default(), false, false),
            ] {
                let floor = if page {
                    StripFloor::Surface(crate::settings::Roles::from_theme(theme).surface)
                } else {
                    StripFloor::Ground
                };
                let colors = strip_colors_for_on(theme, floor);
                assert_eq!(
                    colors.active_rim.is_some(),
                    rimmed,
                    "page={page} {:?}: {:.2}:1",
                    colors.active_bg,
                    crate::chrome_band::contrast(colors.active_bg, colors.band_bg)
                );
                for scale in [1usize, 2] {
                    let (band_top, cell_h, foot) = (15 * scale, 23 * scale, 6 * scale);
                    let cell_w = CELL_W * scale;
                    let h = band_top + cell_h;
                    let seam_px = strip_seam_px(scale as f32);
                    let seam_top = h - seam_px - foot;
                    let geometry = BandGeometry {
                        cols: 90,
                        cell_w,
                        cell_h,
                        strip_rows: 1,
                        band_top_px: band_top,
                        scale: scale as f32,
                        seam_top_px: Some(seam_top),
                        foot_px: foot,
                    };
                    let metadata = plain(3);
                    let segments = layout_segments_with_metadata(90, 3, &metadata, 0, false);
                    let titles = vec![
                        "Settings".to_string(),
                        "build".to_string(),
                        "logs".to_string(),
                    ];
                    let input = BandInput {
                        palette: theme.into(),
                        floor,
                        ..band(
                            &segments,
                            &titles,
                            &metadata,
                            StripPaint::default(),
                            geometry,
                        )
                    };
                    let rows = raster_band(&input, &[]).expect("band");
                    let (rgba, w, _) = image_of(&rows);
                    let at = |x: usize, y: usize| pixel(&rgba, w, x, y);
                    // Within a step per channel: the rim is an anti-aliased stroke.
                    let near =
                        |a: [u8; 3], b: [u8; 3]| a.iter().zip(b).all(|(x, y)| x.abs_diff(y) <= 2);
                    let d =
                        BandDesign::resolve(&geometry, h, band_label_px(h as f32, scale as f32));
                    let x0 = usize::from(segments[0].start_col) * cell_w + d.gap_h as usize;
                    let x1 = usize::from(segments[0].end_col) * cell_w - d.gap_h as usize;
                    let seam = colors.seam.expect("a seam");
                    let tab = colors.active_bg;
                    let inside = seam_top - 3 * scale;
                    let case = format!("page={page} {scale}x");
                    let mid_x = (x0 + x1) / 2;
                    let top = d.card_top as usize;
                    let is = |x: usize, y: usize, want: [u8; 3], what: &str| {
                        let got = at(x, y);
                        assert!(
                            near(got, want),
                            "{case}: {what} at ({x}, {y}): {got:?}, not {want:?}"
                        );
                    };
                    if rimmed {
                        for k in 0..seam_px {
                            for y in [inside, seam_top, seam_top + seam_px - 1] {
                                is(x0 + k, y, seam, "the left side");
                                is(x1 - 1 - k, y, seam, "the right side");
                            }
                            is(mid_x, top + k, seam, "the top");
                        }
                        is(x0 + seam_px, inside, tab, "inside the rim");
                        is(mid_x, top + seam_px, tab, "under the top");
                        is(x0 + seam_px, seam_top, tab, "the opening");
                    } else {
                        is(x0, inside, tab, "no rim");
                        is(mid_x, top, tab, "no rim on top");
                    }
                    assert_eq!(at(x0, h - 1), colors.foot, "{case}: the foot under the tab");
                    assert_eq!(at(mid_x, h - 1), colors.foot, "{case}: the foot");
                }
            }
            crate::tray_raster::clear_ui_fonts_for_test();
        }

        /// A compact band (the headless 25 px band: a 2 px pad over one 23 px
        /// row) keeps a title's descenders a pixel inside its tab at 100%.
        /// NEGATIVE CONTROL: cap-centred, the descenders of `gypsy` reached the
        /// card's last row (the 2026-10-18 judges' `tab-title-descender-clearance`).
        #[test]
        fn a_compact_tab_keeps_its_descenders_inside() {
            if !SELECTED_TAB_IS_GROUND {
                return;
            }
            if !with_ui_faces() {
                aterm_spec::host::could_not_run("no system UI face here for the strip band");
            }
            let geometry = BandGeometry {
                cols: 80,
                cell_w: CELL_W,
                cell_h: 23,
                strip_rows: 1,
                band_top_px: 2,
                scale: 1.0,
                seam_top_px: Some(24),
                foot_px: 0,
            };
            let colors = strip_colors_with_active(Theme::default(), None);
            let metadata = plain(2);
            let segments = layout_segments_with_metadata(80, 2, &metadata, 0, false);
            let titles = vec!["gypsy jig".to_string(), "Apply".to_string()];
            let input = band(
                &segments,
                &titles,
                &metadata,
                StripPaint::default(),
                geometry,
            );
            let rows = raster_band(&input, &[]).expect("band");
            let (rgba, w, h) = image_of(&rows);
            let layout = tab_content_layout(&segments[0], metadata[0]);
            let (_, y0, _, y1) = ink_bbox_off_surfaces(
                &rgba,
                w,
                h,
                usize::from(layout.title_start) * CELL_W,
                usize::from(layout.title_end) * CELL_W,
                &[colors.band_bg, colors.active_bg, colors.chip_bg],
            )
            .expect("the title inks");
            let d = BandDesign::resolve(&geometry, h, band_label_px(h as f32, 1.0));
            assert!(
                y1 as f32 <= d.card_bot - 2.0,
                "descenders at {y1}, the tab ends at {}",
                d.card_bot
            );
            assert!(
                y0 as f32 >= d.card_top + 1.0,
                "ascenders at {y0}, the tab starts at {}",
                d.card_top
            );
            crate::tray_raster::clear_ui_fonts_for_test();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Segments for a one-tab window with the solo title band forced on, so the solo
    /// painter is tested on every platform; only the policy ([`SOLO_TITLE_BAND`]) is
    /// platform-gated.
    fn forced_solo_segments(cols: u16, metadata: &[TabStripMetadata]) -> Vec<TabSegment> {
        let mut segments = layout_segments_with_metadata(cols, 1, metadata, 0, false);
        if let Some(band) = segments.first_mut() {
            band.solo = true;
            band.close_col = None; // what `layout_segments` does for a solo band
        }
        segments
    }

    /// A lone tab on macOS is the window title (no close column, `solo`, full band);
    /// elsewhere it is an ordinary chip with its close column, capped at
    /// [`SOLO_CHIP_MAX_COLS`] so the band stays visible to its right.
    #[test]
    fn single_tab_is_a_full_width_title_band_only_where_it_is_the_window_title() {
        let segs = layout_segments(80, 1, 0, false);
        assert_eq!(segs.len(), 2, "one tab + the new-tab affordance");
        assert_eq!(segs[0].kind, TabHit::Select(0));
        assert_eq!(segs[0].start_col, 0, "the lone tab always LEADS");
        match SOLO_CHIP_MAX_COLS {
            // macOS: the band is spent, not rationed — everything up to the `+`.
            None => {
                assert_eq!(segs[0].end_col, 80 - NEW_TAB_W);
                assert!(segs[0].solo, "a lone tab is the window title");
                assert_eq!(
                    segs[0].close_col, None,
                    "a title band has no close affordance; the window's own already exists"
                );
                // The `+` sits flush after it, NEW_TAB_W cells wide.
                assert_eq!(segs[1].start_col, 80 - NEW_TAB_W);
            }
            // Elsewhere: a bounded chip, with real band left over behind the `+`.
            Some(cap) => {
                assert_eq!(
                    segs[0].end_col, cap,
                    "the lone chip is capped, not full-width"
                );
                assert!(
                    cap < 80 - NEW_TAB_W,
                    "and the cap must actually leave band showing at this width"
                );
                assert!(
                    !segs[0].solo,
                    "the OS caption carries the title here, so a lone tab is a CHIP"
                );
                assert!(
                    segs[0].close_col.is_some(),
                    "and a chip keeps the close affordance a title band declines"
                );
                // The `+` follows the chip rather than being flung to the far edge.
                assert_eq!(segs[1].start_col, cap);
            }
        }
        assert_eq!(segs[1].kind, TabHit::NewTab);
        assert!(!segs[1].solo);
        assert_eq!(segs[1].end_col, segs[1].start_col + NEW_TAB_W);

        // The lone-chip cap is a maximum: a narrower window still gets its share.
        let segs = layout_segments(20, 1, 0, false);
        assert_eq!(segs[0].end_col, 20 - NEW_TAB_W);
        assert_eq!(segs[1].start_col, 20 - NEW_TAB_W);
    }

    /// The inline rename field replaces the chip's title span with the find bar's
    /// well: text verbatim (wide chars as themselves), a reverse-video block caret
    /// covering both cells of a wide char, and the selected chip's accent seam kept
    /// underneath.
    #[test]
    fn the_rename_field_paints_the_text_with_a_reverse_video_caret() {
        let theme = Theme::default();
        let metadata = [
            TabStripMetadata::from_presentation(&crate::tab_model::TabPresentation::terminal("a")),
            TabStripMetadata::from_presentation(&crate::tab_model::TabPresentation::terminal("b")),
        ];
        let segments = layout_segments_with_metadata(60, 2, &metadata, 0, false);
        let titles = ["one".to_string(), "two".to_string()];
        let mut row = vec![blank_cell(theme); 60];
        let (_, painted_caret) = paint_strip_with_metadata_and_rename_caret(
            &mut row,
            &segments,
            &titles,
            &metadata,
            StripPaint {
                hovered: None,
                new_tab_hovered: false,
                version_hovered: false,
                caption_hovered: None,
                subtitle: None,
                rename: Some(StripRenameField {
                    tab: 0,
                    // Caret on the wide char: "日" starts at byte 2.
                    text: "bu日ld",
                    cursor: 2,
                }),
            },
            0,
            theme.into(),
            StripFloor::Ground,
        );
        let seg = segments[0];
        let painted: String = row[seg.start_col as usize..seg.end_col as usize]
            .iter()
            .filter(|c| !c.wide)
            .map(|c| c.ch)
            .collect();
        assert!(
            painted.contains("bu日ld"),
            "the field's text, not the title"
        );
        assert!(
            !painted.contains("one"),
            "the label is replaced while editing, not drawn beside the field"
        );
        // The caret cell is the one at the edit position, reverse-videoed: the
        // well's background becomes its ink, so the caret costs no cell.
        let colors = crate::chrome_band::band_colors(theme);
        let lead_col = row
            .iter()
            .position(|cell| cell.ch == '日')
            .expect("the wide char under the caret is painted as itself");
        let (caret, tail) = (row[lead_col], row[lead_col + 1]);
        assert_eq!(caret.fg, colors.field_bg, "reverse video: bg becomes ink");
        assert_eq!(caret.bg, colors.caret);
        assert!(tail.wide, "the continuation is flagged for the renderer");
        assert_eq!(tail.bg, colors.caret, "and the block covers both cells");
        assert_eq!(
            row[lead_col + 2].bg,
            colors.field_bg,
            "the next cell is field"
        );
        assert_eq!(
            caret.underline,
            UnderlineStyle::Single,
            "the edited chip is still the selected one"
        );
        // The tab not being edited keeps its ordinary label.
        let other: String = row[segments[1].start_col as usize..segments[1].end_col as usize]
            .iter()
            .map(|c| c.ch)
            .collect();
        assert!(other.contains("two"));
        assert_eq!(painted_caret, Some(lead_col));
        assert_eq!(
            rename_ime_cursor_cell(2, painted_caret),
            Some((1, lead_col))
        );

        // An edit can outlive a resize that leaves no cell for its caret.
        // Both the chip and solo-band paths must keep the anchor on the strip.
        for solo in [false, true] {
            let mut segments = forced_solo_segments(4, &metadata[..1]);
            segments[0].solo = solo;
            let mut narrow = vec![blank_cell(theme); 4];
            let (_, painted_caret) = paint_strip_with_metadata_and_rename_caret(
                &mut narrow,
                &segments,
                &titles[..1],
                &metadata[..1],
                StripPaint {
                    hovered: None,
                    new_tab_hovered: false,
                    version_hovered: false,
                    caption_hovered: None,
                    subtitle: None,
                    rename: Some(StripRenameField {
                        tab: 0,
                        text: "日",
                        cursor: 0,
                    }),
                },
                0,
                theme.into(),
                StripFloor::Ground,
            );
            assert_eq!(painted_caret, None, "no caret fits in the narrow well");
            assert!(!narrow.iter().any(|cell| cell.wide));
            assert_eq!(rename_ime_cursor_cell(2, painted_caret), Some((1, 0)));
        }
        assert_eq!(rename_ime_cursor_cell(0, painted_caret), None);
    }

    /// While editing a narrow chip, icon and status canvas are suppressed so the
    /// field gets the whole interior and scrolls rather than truncating.
    #[test]
    fn a_busy_narrow_chip_spends_its_whole_interior_on_the_field() {
        let theme = Theme::default();
        let mut presentation = crate::tab_model::TabPresentation::terminal("a");
        presentation.icon = Some(TabIconKind::Settings);
        presentation.indicators.busy = true;
        presentation.indicators.attention = true;
        let metadata = [
            TabStripMetadata::from_presentation(&presentation),
            TabStripMetadata::from_presentation(&presentation),
        ];
        let segments = layout_segments_with_metadata(28, 2, &metadata, 0, false);
        let titles = ["one".to_string(), "two".to_string()];
        let mut row = vec![blank_cell(theme); 28];
        let images = paint_strip_with_metadata(
            &mut row,
            &segments,
            &titles,
            &metadata,
            StripPaint {
                hovered: None,
                new_tab_hovered: false,
                version_hovered: false,
                caption_hovered: None,
                subtitle: None,
                rename: Some(StripRenameField {
                    tab: 0,
                    text: "abcdef",
                    cursor: 6,
                }),
            },
            0,
            theme,
            None,
        );
        let seg = segments[0];
        let painted: String = row[seg.start_col as usize..seg.end_col as usize]
            .iter()
            .map(|c| c.ch)
            .collect();
        assert!(
            painted.contains("def"),
            "the field scrolled to keep the caret in view: {painted:?}"
        );
        assert!(
            !painted.contains('…'),
            "a field scrolls; it never ellipsises what you are typing: {painted:?}"
        );
        assert!(
            images
                .iter()
                .all(|(col, _)| *col < seg.start_col as usize || *col >= seg.end_col as usize),
            "the edited chip's icon and status marks yield their cells to the field"
        );
    }

    /// Editing the lone tab turns the solo band into one left-aligned field.
    #[test]
    fn editing_the_solo_band_drops_the_centred_group_for_a_left_aligned_field() {
        let theme = Theme::default();
        let metadata = [TabStripMetadata::from_presentation(
            &crate::tab_model::TabPresentation::terminal("aterm"),
        )];
        let segments = forced_solo_segments(40, &metadata);
        let band = segments[0];
        assert!(band.solo);
        let mut row = vec![blank_cell(theme); 40];
        paint_strip_with_metadata(
            &mut row,
            &segments,
            &["aterm".to_string()],
            &metadata,
            StripPaint {
                hovered: None,
                new_tab_hovered: false,
                version_hovered: false,
                caption_hovered: None,
                subtitle: Some("~/aterm"),
                rename: Some(StripRenameField {
                    tab: 0,
                    text: "agent",
                    cursor: 5,
                }),
            },
            0,
            theme,
            None,
        );
        let painted: String = row[..band.end_col as usize].iter().map(|c| c.ch).collect();
        assert!(painted.contains("agent"), "the field is drawn");
        assert!(
            !painted.contains("~/aterm"),
            "the description would recompose under the caret: {painted:?}"
        );
        let first = painted.find(|c: char| c != ' ').expect("some ink");
        assert_eq!(
            first, SOLO_EDGE_COLS,
            "left-aligned at the band's inset, not centred"
        );
    }

    /// An empty field shows the label the ladder falls back to, dimmed.
    #[test]
    fn an_empty_rename_field_placeholds_with_the_resolved_label() {
        let theme = Theme::default();
        let metadata = [
            TabStripMetadata::from_presentation(&crate::tab_model::TabPresentation::terminal("a")),
            TabStripMetadata::from_presentation(&crate::tab_model::TabPresentation::terminal("b")),
        ];
        let segments = layout_segments_with_metadata(60, 2, &metadata, 0, false);
        let titles = ["vim src/main.rs".to_string(), "two".to_string()];
        let mut row = vec![blank_cell(theme); 60];
        paint_strip_with_metadata(
            &mut row,
            &segments,
            &titles,
            &metadata,
            StripPaint {
                hovered: None,
                new_tab_hovered: false,
                version_hovered: false,
                caption_hovered: None,
                subtitle: None,
                rename: Some(StripRenameField {
                    tab: 0,
                    text: "",
                    cursor: 0,
                }),
            },
            0,
            theme,
            None,
        );
        let seg = segments[0];
        let painted: String = row[seg.start_col as usize..seg.end_col as usize]
            .iter()
            .map(|c| c.ch)
            .collect();
        assert!(
            painted.contains("vim src"),
            "the fallback label is the placeholder: {painted:?}"
        );
        let colors = crate::chrome_band::band_colors(theme);
        let hint = row
            .iter()
            .find(|cell| cell.ch == 'v')
            .expect("the placeholder is drawn");
        assert_eq!(hint.fg, colors.label, "dimmed — it is not content");
    }

    /// The `✕` is hover-revealed on quiet tabs and, on a chip-card band, resident on
    /// the selected card. The column is reserved either way and `hit_test` closes
    /// there.
    #[test]
    fn the_close_mark_is_hover_revealed_and_resident_on_the_selected_card() {
        let theme = Theme::default();
        let metadata = [
            TabStripMetadata::from_presentation(&crate::tab_model::TabPresentation::terminal("a")),
            TabStripMetadata::from_presentation(&crate::tab_model::TabPresentation::terminal("b")),
        ];
        let titles = ["a".to_string(), "b".to_string()];
        let segments = layout_segments_with_metadata(60, 2, &metadata, 0, false);
        let close_cols: Vec<u16> = segments.iter().filter_map(|seg| seg.close_col).collect();
        assert_eq!(close_cols.len(), 2, "both chips reserve a close column");

        let marks = |hovered: Option<usize>| {
            let mut row = vec![blank_cell(theme); 60];
            paint_strip_with_metadata(
                &mut row,
                &segments,
                &titles,
                &metadata,
                StripPaint {
                    hovered,
                    new_tab_hovered: false,
                    version_hovered: false,
                    caption_hovered: None,
                    subtitle: None,
                    rename: None,
                },
                0,
                theme,
                None,
            );
            close_cols
                .iter()
                .map(|col| row[*col as usize].ch)
                .collect::<Vec<_>>()
        };
        if STRIP_CHIP_CARDS {
            // The selected card keeps a resident ✕; quiet chips stay
            // hover-revealed.
            assert_eq!(
                marks(None),
                ['✕', ' '],
                "the selected card's ✕ is resident; quiet chips stay bare"
            );
            assert_eq!(marks(Some(0)), ['✕', ' ']);
            assert_eq!(
                marks(Some(1)),
                ['✕', '✕'],
                "hover reveals a quiet chip's ✕ beside the resident one"
            );
        } else {
            assert_eq!(
                marks(None),
                [' ', ' '],
                "no pointer, no ✕ — not even on the selected tab"
            );
            assert_eq!(
                marks(Some(0)),
                ['✕', ' '],
                "only the hovered tab reveals it"
            );
            assert_eq!(marks(Some(1)), [' ', '✕']);
        }
        // Reserved either way: the geometry the title is laid out around does not
        // depend on the pointer, so nothing reflows when the ✕ appears.
        assert_eq!(
            layout_segments_with_metadata(60, 2, &metadata, 1, false),
            segments,
            "selection does not move the close columns either"
        );
    }

    /// The solo band spends its width on the title: the description compresses
    /// first and disappears entirely before the title gives up a cell.
    #[test]
    fn solo_band_compresses_the_description_before_the_title() {
        let (title, subtitle) = solo_band_text("aterm", Some("~/very/long/path/here"), 40);
        assert_eq!(title, "aterm", "a title that fits is never cut");
        assert_eq!(
            subtitle.as_deref(),
            Some("~/very/long/path/here"),
            "and a description that fits is not cut either"
        );

        // Squeezed: the title still fits whole, so the description takes the loss.
        let (title, subtitle) = solo_band_text("aterm", Some("~/very/long/path/here"), 24);
        assert_eq!(title, "aterm");
        assert_eq!(subtitle.as_deref(), Some("~/very/long…"));

        let (title, subtitle) = solo_band_text("aterm", Some("~/aterm"), 14);
        assert_eq!(title, "aterm");
        assert_eq!(
            subtitle, None,
            "no room for a description, so none is drawn"
        );

        let (title, subtitle) = solo_band_text("a-very-long-session-title", Some("~/aterm"), 14);
        assert_eq!(title, "a-very-lo…", "the title takes every cell there is");
        assert_eq!(subtitle, None);
        assert_eq!(
            title.chars().count(),
            14 - 2 * SOLO_EDGE_COLS,
            "and never overruns the band"
        );
    }

    /// Two or more tabs split the band into equal shares, each at most
    /// [`CHIP_MAX_COLS`] (no maximum on macOS), and none is `solo`. Where the cap
    /// holds, the band left over shows after the last chip, as after a lone one.
    #[test]
    fn multiple_tabs_split_the_whole_band_evenly() {
        for count in 2..=6usize {
            let segs = layout_segments(200, count, 0, false);
            let tabs: Vec<_> = segs
                .iter()
                .filter(|s| matches!(s.kind, TabHit::Select(_)))
                .collect();
            assert_eq!(tabs.len(), count);
            let band = 200 - NEW_TAB_W;
            let share = band / u16::try_from(count).unwrap();
            let per = CHIP_MAX_COLS.map_or(share, |cap| share.min(cap));
            for (index, seg) in tabs.iter().enumerate() {
                assert!(!seg.solo, "only a LONE tab is a title band");
                assert_eq!(seg.start_col, per * u16::try_from(index).unwrap());
                assert_eq!(seg.end_col - seg.start_col, per, "equal shares");
            }
            if per == share {
                // Nothing but the integer-division remainder is left over.
                assert!(band - tabs.last().unwrap().end_col < u16::try_from(count).unwrap());
            }
        }
    }

    /// ONE CAP FOR EVERY COUNT (Windows): a lone chip is never narrower than each
    /// chip of a pair at the same width, so opening a second tab never widens the
    /// first.
    #[test]
    fn a_lone_chip_is_as_wide_as_each_of_a_pair() {
        if !STRIP_WINDOWS_PAINT {
            return;
        }
        for cols in 20..=300u16 {
            let width = |count: usize| {
                let segs = layout_segments(cols, count, 0, false);
                let seg = segs
                    .iter()
                    .find(|s| s.kind == TabHit::Select(0))
                    .expect("the first chip");
                seg.end_col - seg.start_col
            };
            assert!(
                width(1) >= width(2),
                "{cols} cols: {} < {}",
                width(1),
                width(2)
            );
        }
    }

    /// Each platform's chips at 200 columns. Windows caps every chip at
    /// [`MAX_CHIP_COLS`]; Linux keeps its strip, a lone chip of 28 columns and two
    /// tabs sharing the whole band; macOS's lone tab is the window title band.
    #[test]
    fn chip_widths_are_each_platforms_own() {
        let width = |count: usize| {
            let segs = layout_segments(200, count, 0, false);
            let seg = segs
                .iter()
                .find(|s| s.kind == TabHit::Select(0))
                .expect("the first chip");
            seg.end_col - seg.start_col
        };
        let band = 200 - NEW_TAB_W;
        let expected = if cfg!(windows) {
            (27, 27)
        } else if cfg!(target_os = "macos") {
            (band, band / 2)
        } else {
            (28, band / 2)
        };
        assert_eq!((width(1), width(2)), expected);
    }

    /// THE WINDOWS TAB CAP IS IN DIPS ([`TabSizing::at`]): WinUI's 240 epx and the
    /// 432 epx growth cap at each scale's own cell width, within half a cell of
    /// the exact width, so a title that fits at 100% fits at 150% and 200%.
    /// NEGATIVE CONTROL: 27 columns of the rounded-down cell (13 px at 150%, 17
    /// at 200%) made the tab 4-6% narrower than its exactly scaled label (the
    /// 2026-10-18 judges' `lone-tab-cap-hidpi`).
    #[test]
    fn the_windows_tab_cap_is_counted_in_dips() {
        for (cell_w, scale, cap, grow) in [
            (9usize, 1.0f32, 27u16, 48u16),
            (13, 1.5, 28, 50),
            (17, 2.0, 28, 51),
        ] {
            let sizing = TabSizing::at(cell_w, scale, &[]);
            assert_eq!((sizing.cap, sizing.grow), (cap, grow), "{scale}x");
            for (cols, dip) in [(sizing.cap, TAB_MAX_DIP), (sizing.grow, TAB_GROW_DIP)] {
                let off = (f32::from(cols) * cell_w as f32 - dip * scale).abs();
                assert!(
                    off <= cell_w as f32 * 0.5,
                    "{scale}x: {cols} cols is {off} px off"
                );
            }
            assert!(
                f32::from(MAX_CHIP_COLS) * (cell_w as f32) < TAB_MAX_DIP * scale - 6.0
                    || scale <= 1.0,
                "CONTROL: {MAX_CHIP_COLS} columns fall short at {scale}x"
            );
        }
    }

    /// A WINDOWS TAB GROWS TOWARD ITS TITLE WHILE THE STRIP HAS ROOM
    /// ([`TabSizing`], [`layout_segments_with_version_sized`]): a lone tab whose
    /// title wants 40 columns is 40 wide, one that wants less keeps the 27-column
    /// tab, one that wants more stops at the growth cap; two tabs grow
    /// independently; three share the room left past their equal widths; a
    /// crowded strip keeps its equal shares; the `+` follows the last tab.
    /// NEGATIVE CONTROL: the 2026-10-18 strip cut every title at 27 columns with
    /// the rest of the band empty (the judges' `tab-title-cap-27-cols-lone-tab`).
    #[test]
    fn a_windows_tab_grows_toward_its_title_while_the_strip_has_room() {
        if !STRIP_WINDOWS_PAINT {
            return;
        }
        let widths = |cols: u16, wants: &[u16]| {
            let metadata = vec![PLAIN_TAB; wants.len()];
            let sizing = TabSizing::at(9, 1.0, wants);
            let segs = layout_segments_with_version_sized(
                cols,
                wants.len(),
                &metadata,
                0,
                false,
                VersionSlotCols::NONE,
                sizing,
            );
            let mut x = 0;
            let mut out = Vec::new();
            for seg in &segs {
                assert_eq!(seg.start_col, x, "segments are packed: {segs:?}");
                x = seg.end_col;
                if let TabHit::Select(_) = seg.kind {
                    out.push(seg.end_col - seg.start_col);
                }
            }
            assert!(
                segs.last().is_some_and(|s| s.kind == TabHit::NewTab),
                "the + follows the last tab"
            );
            out
        };
        assert_eq!(widths(111, &[40]), vec![40], "a lone long title");
        assert_eq!(widths(111, &[12]), vec![27], "a short title keeps the tab");
        assert_eq!(widths(111, &[90]), vec![48], "the growth cap");
        assert_eq!(widths(111, &[40, 0]), vec![40, 27], "two tabs");
        assert_eq!(
            widths(95, &[48, 48, 0]),
            vec![33, 32, 27],
            "the room past 3 x 27 is shared by the tabs that want it"
        );
        assert_eq!(
            widths(100, &[48, 48, 48, 48, 48]),
            vec![19; 5],
            "a crowded strip keeps equal shares"
        );
        // The fixed sizing is the equal strip, to the column.
        let metadata = vec![PLAIN_TAB; 2];
        assert_eq!(
            layout_segments_with_version_sized(
                111,
                2,
                &metadata,
                0,
                false,
                VersionSlotCols::NONE,
                TabSizing::FIXED
            ),
            layout_segments_with_version(111, 2, &metadata, 0, false, VersionSlotCols::NONE)
        );
    }

    /// THE BAND'S FOOT (Windows): a real window's synthetic head reserves
    /// `STRIP_FOOT_LOGICAL_PX` past the 32 px band, the canvas gives it back as
    /// the terminal's ground under the seam, and the gutters' rail is that
    /// ground; a compact band and a headless window's sealed boot band (no head
    /// to spare) keep the seam on the band's last row. NEGATIVE CONTROL: the
    /// 2026-10-18 build reserved no foot, so the grid's first row met the rule.
    #[test]
    fn a_sized_band_keeps_a_foot_of_ground_under_its_seam() {
        if !STRIP_WINDOWS_PAINT {
            assert_eq!(strip_foot_px(38, 32, 1.0), 0, "no foot off Windows");
            return;
        }
        // (canvas, target, scale) → foot: 2 + 13 + 23 at 100%, 3 + 19 + 35 at
        // 150%, 4 + 26 + 46 at 200%, a large font with no head, headless, compact.
        for (canvas, target, scale, want) in [
            (38, 32, 1.0, 6),
            (57, 48, 1.5, 9),
            (76, 64, 2.0, 12),
            (42, 32, 1.0, 6),
            (25, 32, 1.0, 0),
            (25, 0, 1.0, 0),
        ] {
            assert_eq!(
                strip_foot_px(canvas, target, scale),
                want,
                "canvas {canvas}, target {target}, {scale}x"
            );
        }
        let theme = Theme::default();
        let colors = strip_colors(theme);
        let pack = |c: [u8; 3]| (u32::from(c[0]) << 16) | (u32::from(c[1]) << 8) | u32::from(c[2]);
        let footed = strip_seam_rail(0, 100, 1, 6, theme, StripFloor::Ground).expect("a rail");
        assert_eq!(footed.rail_h, 6);
        assert!(footed.rail.iter().all(|&px| px == pack(colors.foot)));
        // Over another floor the gutters' foot takes that floor's tone, so it
        // runs level with the band's own foot (`StripFloor`).
        let page = [0x1e, 0x1e, 0x20];
        let paged =
            strip_seam_rail(0, 100, 1, 6, theme, StripFloor::Surface(page)).expect("a rail");
        assert!(paged.rail.iter().all(|&px| px == pack(page)));
        let bare = strip_seam_rail(0, 100, 1, 0, theme, StripFloor::Ground).expect("a rail");
        assert_eq!(bare.rail_h, 1);
        assert!(
            bare.rail
                .iter()
                .all(|&px| px == pack(colors.seam.expect("a seam")))
        );
        // The synthetic head of a real window reserves the foot past the band.
        let mut app = crate::App::headless_for_test();
        app.tab_strip_rows = 1;
        assert_eq!(
            app.synthetic_strip_head_px(1.0, 23),
            32 + 6
                - crate::logical_to_device_px(app.config.window_padding_top_or_default(), 1.0)
                - 23,
            "the head holds the 32 px band less its row and pad, plus the foot"
        );
    }

    /// `show_update` prepends a leading `↻` segment at col 0, shifting the tabs right;
    /// a click on it is `TabHit::Update`. Absent when `show_update = false`.
    #[test]
    fn update_icon_prepends_and_shifts_tabs() {
        let none = layout_segments(80, 2, 0, false);
        assert!(
            !none.iter().any(|s| s.kind == TabHit::Update),
            "no icon when not staged"
        );

        let segs = layout_segments(80, 2, 0, true);
        assert_eq!(
            segs[0].kind,
            TabHit::Update,
            "the leading segment is the update icon"
        );
        assert_eq!(segs[0].start_col, 0);
        assert_eq!(segs[0].end_col, UPDATE_W);
        // Tabs start after the icon.
        let first_tab = segs
            .iter()
            .find(|s| matches!(s.kind, TabHit::Select(_)))
            .unwrap();
        assert_eq!(
            first_tab.start_col, UPDATE_W,
            "tabs shifted right by the icon width"
        );
        // A click anywhere on the icon segment hits Update.
        assert_eq!(hit_test(&segs, 1), Some(TabHit::Update));
        assert_eq!(hit_test(&segs, UPDATE_W), Some(TabHit::Select(0)));
    }

    #[test]
    fn canonical_metadata_preserves_segment_and_hit_geometry() {
        let base = layout_segments(80, 2, 0, false);
        let terminal = crate::tab_model::TabPresentation::terminal("long-live-session-title");
        let metadata = [
            TabStripMetadata::from_presentation(&terminal),
            TabStripMetadata {
                icon: Some(TabIconKind::Settings),
                dirty: true,
                busy: true,
                attention: ChipLevel::Wait,
                conn: Some(TabConnRole::Both),
                closable: false,
                team: TabTeam::NONE,
                load: 0,
            },
        ];
        let canonical = layout_segments_with_metadata(80, metadata.len(), &metadata, 0, false);
        assert_eq!(canonical.len(), base.len());
        for (actual, prior) in canonical.iter().zip(&base) {
            assert_eq!(
                (actual.start_col, actual.end_col, actual.kind),
                (prior.start_col, prior.end_col, prior.kind),
                "icons/status never alter select/reorder geometry"
            );
        }
        assert_eq!(canonical[0].close_col, base[0].close_col);
        assert_eq!(canonical[1].close_col, None, "canonical closable=false");
        assert_eq!(
            hit_test(&canonical, canonical[0].close_col.unwrap()),
            Some(TabHit::Close(0))
        );
        // A non-closable chip's freed trailing cell (its status canvas) is part
        // of the chip: a press there selects it.
        assert_eq!(
            hit_test(&canonical, base[1].close_col.unwrap()),
            Some(TabHit::Select(1)),
            "the painted status cell selects its tab"
        );
    }

    /// Every [`IconKey`] reachable with `conn: None` (4 icon kinds, all 8 status
    /// combinations, two colours) through the real `append_*` entry points matches a
    /// fresh rasterization byte for byte.
    #[test]
    fn memoized_strip_glyphs_match_a_fresh_raster_for_every_key() {
        let kinds = [
            TabIconKind::Settings,
            TabIconKind::Markdown,
            TabIconKind::Editor,
            TabIconKind::Recovery,
        ];
        for color in [[213u8, 219, 255], [255, 180, 64]] {
            for kind in kinds {
                let mut images = Vec::new();
                append_icon_images(&mut images, 0, kind, color);
                let expected = rasterize_icon(tab_icon_primitives(kind), color);
                assert_eq!(images.len(), usize::from(ICON_COLS));
                for (_, placed) in &images {
                    assert_eq!(placed.image.bytes, expected, "{kind:?} in {color:?}");
                    assert_eq!(placed.image.cols, ICON_COLS, "{kind:?} keeps its footprint");
                }
            }
            for bits in 0..8u8 {
                let metadata = TabStripMetadata {
                    icon: None,
                    dirty: bits & 0b001 != 0,
                    busy: bits & 0b010 != 0,
                    attention: crate::app_presence::chip_of_attention(bits & 0b100 != 0),
                    conn: None,
                    team: TabTeam::NONE,
                    closable: true,
                    load: 0,
                };
                let mut images = Vec::new();
                append_status_image(&mut images, 0, metadata, color);
                let expected = rasterize_icon(&status_primitives(metadata), color);
                let placed = &images[0].1;
                assert_eq!(placed.image.bytes, expected, "{metadata:?} in {color:?}");
                assert_eq!(placed.image.cols, 1, "status marks stay one cell wide");
            }
        }
    }

    #[test]
    fn terminal_tab_removes_icon_raster_and_recovers_title_cells() {
        let theme = Theme::default();
        let presentations = [
            crate::tab_model::TabPresentation::terminal("terminal-session-with-a-long-name"),
            crate::tab_model::TabPresentation {
                title: "Settings".to_string(),
                icon: Some(TabIconKind::Settings),
                indicators: crate::tab_model::TabIndicators::default(),
                conn: None,
                closable: true,
                tooltip: None,
            },
        ];
        let metadata = presentations
            .iter()
            .map(TabStripMetadata::from_presentation)
            .collect::<Vec<_>>();
        assert_eq!(metadata[0].icon, None, "terminal identity is title-only");
        assert_eq!(metadata[1].icon, Some(TabIconKind::Settings));

        let segments = layout_segments_with_metadata(80, metadata.len(), &metadata, 0, false);
        let terminal_layout = tab_content_layout(&segments[0], metadata[0]);
        let settings_layout = tab_content_layout(&segments[1], metadata[1]);
        // Content shifts by the chip-card interior pad where that language is
        // live; the recovered-reservation arithmetic is pad-independent.
        let lead = 1 + u16::from(STRIP_CHIP_CARDS);
        assert_eq!(terminal_layout.icon_start, None);
        assert_eq!(
            settings_layout.icon_start,
            Some(segments[1].start_col + lead)
        );
        assert_eq!(terminal_layout.title_start, segments[0].start_col + lead);
        assert_eq!(
            settings_layout.title_start,
            segments[1].start_col + lead + ICON_COLS + ICON_GAP
        );
        assert_eq!(
            settings_layout.title_start - (segments[1].start_col + lead),
            ICON_COLS + ICON_GAP,
            "terminal title recovers the entire icon-and-gap reservation"
        );

        let mut row = vec![blank_cell(theme); 80];
        let images = paint_strip_with_metadata(
            &mut row,
            &segments,
            &presentations
                .iter()
                .map(|presentation| presentation.title.clone())
                .collect::<Vec<_>>(),
            &metadata,
            StripPaint {
                hovered: Some(0),
                new_tab_hovered: false,
                version_hovered: false,
                caption_hovered: None,
                subtitle: None,
                rename: None,
            },
            0,
            theme,
            None,
        );
        assert!(
            images
                .iter()
                .all(|(col, _)| *col >= usize::from(segments[1].start_col)),
            "terminal segment has no icon image placement"
        );
        assert_eq!(
            row[terminal_layout.title_start as usize].ch, 't',
            "the recovered leading cell carries session-title text"
        );
    }

    /// The solo band's subtitle comes from the composed session chrome, prefers
    /// authored prose over observed status, and never echoes the title back.
    #[test]
    fn solo_subtitle_prefers_authored_prose_and_never_echoes_the_title() {
        for (title, chrome, expected) in [
            // An authored description outranks every generated fact.
            (
                "aterm",
                Some("aterm\ndescription: the release cutter\nactivity: Building\ncwd: ~/aterm"),
                Some("the release cutter"),
            ),
            // Where the session is outranks what it is doing.
            (
                "aterm",
                Some("aterm\ncwd: ~/aterm\nactivity: Building"),
                Some("~/aterm"),
            ),
            ("aterm", Some("aterm\nactivity: Building"), Some("Building")),
            // A lifecycle word is not a description.
            ("aterm", Some("aterm\nstate: exited"), None),
            // A title that already carries the fact gets no echo (a program may say
            // it in its own title), and the NEXT-best fact still wins when only the
            // first one echoes.
            (
                "aterm · Ready",
                Some("aterm · Ready\nactivity: Ready"),
                None,
            ),
            (
                "aterm · Ready",
                Some("aterm · Ready\nactivity: Ready\ncwd: ~/aterm"),
                Some("~/aterm"),
            ),
            // Nothing to say stays silent; `-` is the unset sentinel; a timeline
            // row is not an identity line.
            ("aterm", None, None),
            ("aterm", Some("aterm"), None),
            ("aterm", Some("aterm\ncwd: "), None),
            ("aterm", Some("aterm\ncwd: -"), None),
            ("aterm", Some("aterm\n\nstate changed · just now"), None),
        ] {
            assert_eq!(
                solo_subtitle(title, chrome).as_deref(),
                expected,
                "{title:?} over {chrome:?}"
            );
        }
    }

    #[test]
    fn native_point_layout_keeps_close_and_tab_targets_while_reserving_app_icon_only() {
        let plain = native_tab_content_layout(220.0, 14.0, false, false);
        let app = native_tab_content_layout(220.0, 14.0, true, true);
        assert_eq!(app.close, plain.close, "close hit target never moves");
        assert!(plain.close_available && app.close_available);
        assert!(plain.icon.is_none() && app.icon.is_some() && app.status.is_some());
        for layout in [
            plain,
            app,
            native_tab_content_layout(220.0, 14.0, true, false),
        ] {
            assert_eq!(
                layout.label[0],
                220.0 - (layout.label[0] + layout.label[2]),
                "the title's insets are symmetric, so a centred title reads centred"
            );
        }
        assert_eq!(
            plain.label[2] - native_tab_content_layout(220.0, 14.0, true, false).label[2],
            2.0 * (TAB_ICON_NATIVE_SIZE + 4.0),
            "a terminal gets the native icon slot and gap back for its label"
        );

        // Compact: the title reclaims the close space and the decorations go
        // before the label does; a narrow label clips, never underflows.
        for width in [48.0, 63.0] {
            let compact = native_tab_content_layout(width, 14.0, true, true);
            assert_eq!(compact.close, app.close);
            assert!(
                !compact.close_available,
                "{width}: title reclaims close space"
            );
            assert_eq!(compact.icon, None, "{width}: title wins before identity");
            assert_eq!(compact.status, None, "{width}: no room for the states");
            assert!(
                compact.label[2] >= 0.0,
                "{width}: label clips, never underflows"
            );
        }

        // Every slot tracks the measured vertical centre, not the view's midpoint.
        let lowered = native_tab_content_layout(220.0, 16.0, true, true);
        assert_eq!(lowered.close[1], app.close[1] + 2.0);
        assert_eq!(lowered.icon.unwrap()[1], app.icon.unwrap()[1] + 2.0);
        assert_eq!(lowered.status.unwrap()[1], app.status.unwrap()[1] + 2.0);
        assert_eq!(lowered.label[1], app.label[1] + 2.0);
        assert_eq!(
            [lowered.close[0], lowered.label[0], lowered.label[2]],
            [app.close[0], app.label[0], app.label[2]],
            "a vertical re-centre never disturbs horizontal geometry"
        );
    }

    /// THE LOAD METER (design §12.4): a tenth of a core shows even on a
    /// machine of hundreds, quiet earns none, the length is the share of the
    /// track between the insets in whole points and never past it, and the
    /// meter takes no status slot.
    #[test]
    fn a_chip_load_meter_shows_a_tenth_of_a_core_in_whole_points() {
        assert_eq!(chip_load_step(0.05, 10.0), 0, "quiet earns no meter");
        assert_eq!(chip_load_step(0.1, 512.0), 1, "a tenth of a core shows");
        assert_eq!(chip_load_step(5.0, 10.0), 128);
        assert_eq!(chip_load_step(40.0, 10.0), 255, "the machine is the end");
        assert_eq!(chip_load_step(1.0, 0.0), 0, "no capacity, no meter");
        assert_eq!(chip_load_step(f32::NAN, 10.0), 0);
        assert_eq!(chip_load_len_pt(120.0, 0), None);
        assert_eq!(chip_load_len_pt(120.0, 1), Some(1.0));
        assert_eq!(chip_load_len_pt(120.0, 128), Some(50.0));
        assert_eq!(chip_load_len_pt(120.0, 255), Some(100.0));
        assert_eq!(
            chip_load_len_pt(22.0, 255),
            None,
            "no room between the insets"
        );
        let loaded = TabStripMetadata {
            load: 200,
            ..PLAIN_TAB
        };
        assert!(!loaded.has_status());
        assert_eq!(loaded.status_count(), 0);
    }

    /// THE TAB'S MARKS ARE TELLABLE APART (§5): every attention level draws
    /// its own shape — a finished task's dot is never the tab's own note (a
    /// failed command, a notice), which is a dash — so a background shell's
    /// failed `grep` never reads as a finished worker. Pairwise distinct on
    /// pixels, in one ink, like the role marks.
    #[test]
    fn every_attention_mark_is_tellable_apart() {
        let with = |attention| TabStripMetadata {
            icon: None,
            dirty: false,
            busy: false,
            attention,
            conn: None,
            closable: true,
            team: TabTeam::NONE,
            load: 0,
        };
        let levels = [
            ChipLevel::Note,
            ChipLevel::Done,
            ChipLevel::Yours,
            ChipLevel::Wait,
            ChipLevel::Stop(crate::presence::StopCause::Wall),
        ];
        let note = status_primitives(with(ChipLevel::Note));
        assert!(
            matches!(note[..], [TabIconPrimitive::Line { .. }]),
            "the note is one dash"
        );
        let done = status_primitives(with(ChipLevel::Done));
        assert!(
            matches!(done[..], [TabIconPrimitive::Dot { .. }]),
            "the done mark is one dot"
        );
        let rasters: Vec<Vec<u8>> = levels
            .iter()
            .map(|level| rasterize_icon(&status_primitives(with(*level)), [213, 219, 255]))
            .collect();
        for (i, a) in rasters.iter().enumerate() {
            assert!(
                a.as_chunks::<4>().0.iter().any(|p| p[3] != 0),
                "{:?} draws",
                levels[i]
            );
            for (j, b) in rasters.iter().enumerate().skip(i + 1) {
                assert_ne!(a, b, "{:?} and {:?} look alike", levels[i], levels[j]);
            }
        }
    }

    /// A segment below `MIN_SEG_WITH_CLOSE` drops the close `x` but still selects.
    /// The rule is per segment: 9 columns and 3 tabs seat the two-seat minimum
    /// ([`STRIP_MIN_SEATS`]), each too narrow for a close column.
    #[test]
    fn narrow_tab_drops_close_but_selectable() {
        let segs = layout_segments(9, 3, 0, false);
        let tabs: Vec<_> = segs
            .iter()
            .filter(|s| matches!(s.kind, TabHit::Select(_)))
            .collect();
        assert!(!tabs.is_empty());
        let mut saw_narrow = false;
        for t in &tabs {
            let width = t.end_col - t.start_col;
            assert_eq!(
                t.close_col.is_some(),
                width >= MIN_SEG_WITH_CLOSE,
                "a close x exists exactly where the segment can carry one: {t:?}"
            );
            saw_narrow |= width < MIN_SEG_WITH_CLOSE;
            // Still selectable.
            assert_eq!(hit_test(&segs, t.start_col), Some(t.kind));
        }
        assert!(
            saw_narrow,
            "the case is still reachable — a segment too narrow for a close x: {tabs:?}"
        );
    }

    /// Title truncation from either end: a fit is kept, `max` budgets display cells
    /// (a wide char that cannot fit before the `…` is dropped whole), and the tail
    /// cut snaps to a word boundary unless that costs more than half the survivor.
    #[test]
    fn truncate_title_cuts_display_cells_from_either_end() {
        let head = truncate_title as fn(&str, usize) -> String;
        let tail = truncate_title_tail as fn(&str, usize) -> String;
        for (cut, title, max, expected) in [
            (head, "bash", 10, "bash"),
            (head, "abcde", 5, "abcde"),
            (head, "a-very-long-title", 5, "a-ve…"),
            (head, "anything", 1, "…"),
            (head, "anything", 0, ""),
            (head, "日本語", 6, "日本語"),
            (head, "日本語", 5, "日本…"),
            (head, "日本語", 4, "日…"),
            (head, "日本語", 2, "…"),
            (head, "a🚀bc", 5, "a🚀bc"),
            (head, "a🚀bcd", 4, "a🚀…"),
            (tail, "abcdef", 6, "abcdef"),
            (tail, "abcdef", 4, "…def"),
            (tail, "abcdef", 1, "…"),
            (tail, "abcdef", 0, ""),
            (tail, "user@m17-tower: ~/aterm", 10, "…~/aterm"),
            (tail, "Ready in aterm", 13, "…in aterm"),
            (tail, "Typing a command", 14, "…a command"),
            (tail, "verylongtokenwithoutspaces/x", 12, "…outspaces/x"),
            (tail, "日本語", 5, "…本語"),
            (tail, "日本語", 4, "…語"),
            (tail, "日本語", 2, "…"),
        ] {
            assert_eq!(cut(title, max), expected, "{title:?} in {max} cells");
        }
    }

    /// A COMMAND LINE KEEPS ITS FILE ([`command_cut`]): cut in the middle, the
    /// program and as many whole leading words as fit (a quoted argument kept
    /// whole), the mark, then the last argument whole or its longest tail that
    /// starts a path component; where the program and the file's own name do
    /// not fit together, the file alone behind a leading mark. A title whose
    /// last word names no file (prose, a version), or whose file's own name
    /// does not fit at all, is head-cut; a title that fits is whole.
    /// NEGATIVE CONTROL: the head cut
    /// every chip took before lost vim's file at 60 cells (the 2026-10-18
    /// judges' `j3d-long-title-end-cut`).
    #[test]
    fn a_cut_command_line_keeps_its_file() {
        let vim = "vim -n -R -i NONE -c 'syntax on' +1925 crates/aterm-gui/src/platform_win.rs";
        for (title, max, expected) in [
            (
                vim,
                60,
                "vim -n -R -i NONE -c … crates/aterm-gui/src/platform_win.rs",
            ),
            (vim, 30, "vim -n … src/platform_win.rs"),
            (vim, 21, "vim … platform_win.rs"),
            (vim, 20, "…src/platform_win.rs"),
            (vim, 16, "…platform_win.rs"),
            ("less -R +1200 CHANGELOG.md", 15, "…CHANGELOG.md"),
            ("less -R +1200 CHANGELOG.md", 20, "less … CHANGELOG.md"),
            (
                "less -R +1200 CHANGELOG.md",
                26,
                "less -R +1200 CHANGELOG.md",
            ),
            (
                r"code --wait C:\Users\m6-an\aterm-winperfect\SPECS.md",
                32,
                r"code … aterm-winperfect\SPECS.md",
            ),
            (
                r"code --wait C:\Users\m6-an\aterm-winperfect\SPECS.md",
                30,
                "code --wait … SPECS.md",
            ),
            ("tail -f /var/log/syslog/", 14, "tail … syslog/"),
        ] {
            let cut = chip_cut(title, max);
            assert_eq!(cut, expected, "{title:?} in {max} cells");
            assert!(strip_display_cells(&cut) <= max, "{cut:?} in {max} cells");
        }
        for (title, max) in [
            (vim, 15),
            (
                "Claude Code: reviewing platform_win.rs and the Default Light contrast",
                40,
            ),
            ("aterm nightly build 0.106.1241", 20),
            ("wait for it, e.g.", 10),
            ("crates/aterm-gui/src/platform_win.rs", 20),
            (r"C:\Program Files\Git\usr\share\doc\README.md", 30),
        ] {
            assert_eq!(command_cut(title, max), None, "{title:?} in {max} cells");
            assert_eq!(chip_cut(title, max), truncate_title(title, max));
        }
        assert!(
            !truncate_title(vim, 60).contains("platform_win.rs"),
            "CONTROL: the head cut loses the file"
        );
        // The strip's first cut, on a lone chip (the window title on macOS,
        // which this pass does not label) and beside a quiet one.
        for tabs in [1usize, 2] {
            if SOLO_TITLE_BAND && tabs == 1 {
                continue;
            }
            let metadata = vec![PLAIN_TAB; tabs];
            let segments = layout_segments_with_metadata(120, tabs, &metadata, 0, false);
            let mut titles = vec![vim.to_string()];
            titles.resize(tabs, "logs".to_string());
            let labels = distinct_chip_labels(&segments, &titles, Some(&metadata), 0, None);
            let label = labels[0].as_deref().expect("a seated chip");
            assert!(
                label.ends_with("platform_win.rs") && label.starts_with("vim"),
                "{tabs} tab(s): {label:?}"
            );
        }
    }

    /// A TITLE THE SHELL ALREADY CUT keeps its end cut ([`command_cut`]): the
    /// integrations end a command line they cut with `…`, so its last word is a
    /// fragment of the file, and the chip cuts the title's head with its own
    /// mark at the end; whole where it fits, mark and all. NEGATIVE CONTROL: the
    /// same 64 characters with no mark, as every integration sent them before,
    /// read as a command whose file is `crates/aterm-gui/src/plat` (the
    /// 2026-10-18 judges' `J4F-1-cut64-middle-cut-misleads`).
    #[test]
    fn a_title_the_shell_cut_keeps_its_end_cut() {
        let sent = "vim -n -R -i NONE -c 'syntax on' +1925 crates/aterm-gui/src/plat…";
        for max in [12, 20, 30, 40, 60] {
            assert_eq!(command_cut(sent, max), None, "{max} cells");
            let cut = chip_cut(sent, max);
            assert_eq!(cut, truncate_title(sent, max), "{max} cells");
            assert!(
                cut.starts_with("vim -n") && cut.ends_with('…') && !cut.contains(" … "),
                "{cut:?} in {max} cells"
            );
        }
        assert_eq!(chip_cut(sent, 80), sent);
        let unmarked = sent.trim_end_matches('…');
        assert!(
            chip_cut(unmarked, 40).ends_with(" … crates/aterm-gui/src/plat"),
            "CONTROL: an unmarked cut reads as its file"
        );
    }

    /// A cut path keeps its end, cut only where a name begins ([`path_tail`]).
    #[test]
    fn a_cut_path_keeps_its_directory_and_never_a_fragment_of_a_name() {
        // Loners: the longest tail that starts a name and fits.
        for (subject, room, expected) in [
            ("C:\\Windows\\Temp", 12, Some("…Temp")),
            ("C:\\Windows\\Temp", 13, Some("…Windows\\Temp")),
            ("C:\\Windows\\System32\\drivers", 12, Some("…drivers")),
            (
                "C:\\Windows\\System32\\drivers",
                17,
                Some("…System32\\drivers"),
            ),
            ("C:\\Program Files\\Git", 11, Some("…Files\\Git")),
            (
                "C:\\Program Files\\Common Files\\microsoft shared",
                12,
                Some("…shared"),
            ),
            (
                "C:\\Program Files\\Common Files\\microsoft shared",
                17,
                Some("…microsoft shared"),
            ),
            (
                "~\\aterm\\crates\\aterm-cli\\src",
                14,
                Some("…aterm-cli\\src"),
            ),
            ("~\\aterm\\crates\\aterm-cli\\src", 13, Some("…cli\\src")),
            ("~\\aterm\\crates\\aterm-cli\\src", 4, Some("…src")),
            ("/home/person/work/api", 9, Some("…work/api")),
            // The leaf seats and `…` with it does not: the directory's name, whole.
            ("C:\\Windows\\System32", 8, Some("System32")),
            ("~\\aterm\\crates\\aterm-cli\\src", 3, Some("src")),
            // Nothing but a fragment fits: `path_fallback` answers.
            ("C:\\Windows\\System32", 7, None),
            ("~/a/verylongname", 8, None),
        ] {
            assert_eq!(
                path_tail(subject, room, &[]).as_deref(),
                expected,
                "{subject:?} in {room} cells"
            );
        }

        // A family: a tail a kin also ends with is passed over, and the shared
        // trailing component (`\src`, not `i\src`) is shed.
        let gui = "~\\aterm\\crates\\aterm-gui\\src";
        let cli = "~\\aterm\\crates\\aterm-cli\\src";
        let sys = "C:\\Windows\\System32";
        assert_eq!(common_suffix_bytes(gui, cli), "i\\src".len(), "fixture");
        for (room, expected) in [
            // The selected chip's window.
            (15, Some("…aterm-gui\\src")),
            (14, Some("…aterm-gui\\src")),
            (13, Some("…gui\\src")),
            (8, Some("…gui\\src")),
            // `…src` fits, and names both: the shared component goes instead.
            (7, Some("…gui")),
            (4, Some("…gui")),
            (3, None),
        ] {
            assert_eq!(
                path_tail(gui, room, &[cli]).as_deref(),
                expected,
                "{room} cells"
            );
            if let Some(label) = path_tail(gui, room, &[cli]) {
                assert_ne!(
                    Some(label),
                    path_tail(cli, room, &[gui]),
                    "{room} cells: the pair stays tellable apart"
                );
            }
        }
        // Kin that end differently change nothing, and an ancestor is not cut
        // for a tab below it: `~\aterm\crates` keeps `…crates`.
        assert_eq!(
            path_tail(gui, 7, &[cli, sys, "~\\aterm"]).as_deref(),
            Some("…gui")
        );
        assert_eq!(
            path_tail("~\\aterm\\crates", 7, &[gui]).as_deref(),
            Some("…crates")
        );
        assert!(ends_with_token("~\\x\\aterm-gui", "gui"));
        assert!(!ends_with_token("~\\x\\argui", "gui"));

        // The fallback: the head of the directory's name; the chip's position where
        // that head is a stub or reads as a kin's name.
        for (tab, subject, room, kin, expected) in [
            (6, sys, 7, &[][..], "System…"),
            (6, sys, 4, &[], "Sys…"),
            (6, sys, 7, &[gui, cli, "C:\\Windows\\Temp"], "System…"),
            // Two letters of a name are a stub.
            (6, sys, 3, &[], "7"),
            // `System…` would name `SystemApps` just as well.
            (6, sys, 7, &["C:\\Windows\\SystemApps"], "7"),
            // Both `src` directories: the name alone names neither.
            (4, gui, 3, &[cli], "5"),
            // Not even the digits fit: the mark; no window: nothing.
            (11, sys, 1, &[], "…"),
            (11, sys, 0, &[], ""),
        ] {
            assert_eq!(
                path_fallback(tab, subject, room, kin),
                expected,
                "{subject:?} in {room} cells beside {kin:?}"
            );
        }

        // The window's kin, seated or not; a twin is not kin: text cannot tell
        // twins apart, their position does.
        let subjects = ["~\\aterm", gui, cli, sys, gui, "cargo test -p crates"];
        assert_eq!(path_chip_label(1, &subjects, 7), "…gui");
        assert_eq!(path_chip_label(2, &subjects, 7), "…cli");
        assert_eq!(path_chip_label(4, &subjects, 7), "…gui");
        assert_eq!(path_chip_label(3, &subjects, 7), "System…");
        // Control: with no `aterm-cli\src` in the window, `…src` names gui alone.
        assert_eq!(path_chip_label(1, &["~\\aterm", gui, sys], 7), "…src");
        // Only paths are kin: a command naming `crates` is not cut to it.
        assert_eq!(
            path_chip_label(0, &["~\\aterm\\crates", "cargo test -p crates"], 7),
            "…crates"
        );
        // A path twin's ordinal carries the path's own cut, or stands bare —
        // never `5 · …ui`.
        assert_eq!(path_ordinal_label(4, &subjects, 7), "5");
        assert_eq!(path_ordinal_label(4, &subjects, 8), "5 · …gui");
        assert_eq!(path_ordinal_label(4, &subjects, 12), "5 · …gui\\src");
        assert_eq!(path_ordinal_label(11, &subjects, 1), "…");

        // The shed itself: whole words and components, or nothing.
        assert_eq!(snap_shed("build-10", 1), 0, "`0` is inside a name");
        assert_eq!(snap_shed("x · Ready", " · Ready".len()), " · Ready".len());
        assert_eq!(
            snap_shed("same", 4),
            4,
            "a whole-title suffix is cut_core's"
        );
        for (subject, path) in [
            ("~", true),
            ("~\\aterm", true),
            ("~/aterm", true),
            ("/tmp", true),
            ("C:\\Windows", true),
            ("c:/windows", true),
            ("\\\\build-01\\share", true),
            ("~aterm", false),
            ("user@m17-tower: ~/aterm", false),
            ("cargo build", false),
            ("C:", false),
            ("", false),
        ] {
            assert_eq!(is_path_subject(subject), path, "{subject:?}");
        }
    }

    /// Whether `label` is an honest cut of the path `subject` on tab `tab`: the
    /// subject whole; a marked tail that starts and ends on a name boundary; the
    /// directory name whole or its head keeping three cells; or the chip's position,
    /// bare or ahead of one of those. Never a name cut from inside (`…ystem32`) or
    /// only the shared ancestry (`C:\Win…`).
    fn honest_path_cut(tab: usize, subject: &str, label: &str) -> bool {
        let position = (tab + 1).to_string();
        if label == position || label == "…" {
            return true;
        }
        let label = label
            .strip_prefix(format!("{position} · ").as_str())
            .unwrap_or(label);
        let name = subject.rsplit(['/', '\\']).next().unwrap_or_default();
        if label == subject || label == name {
            return true;
        }
        if let Some(text) = label.strip_prefix('…') {
            return !text.is_empty()
                && subject.match_indices(text).any(|(at, _)| {
                    subject[..at].ends_with(is_token_boundary)
                        && subject[at + text.len()..]
                            .chars()
                            .next()
                            .is_none_or(|c| matches!(c, '/' | '\\'))
                });
        }
        label.strip_suffix('…').is_some_and(|head| {
            name.starts_with(head) && strip_display_cells(head) >= strip_display_cells(name).min(3)
        })
    }

    /// The title width budget of `tab`'s seated chip, in cells.
    fn chip_avail(segments: &[TabSegment], tab: usize) -> usize {
        let seg = segments
            .iter()
            .find(|seg| seg.kind == TabHit::Select(tab))
            .expect("a seated chip has a segment");
        let layout = tab_content_layout(seg, PLAIN_TAB);
        usize::from(layout.title_end.saturating_sub(layout.title_start))
    }

    /// A windowed strip of fifteen tabs, over every width and selection, where the
    /// `aterm-gui\src` and `aterm-cli\src` tabs can land on different pages: gui and
    /// cli chips name `gui`/`cli` wherever four cells fit (or carry their position);
    /// every path cut is honest; a path chip's label is one per (tab, width) across
    /// pages and selections; and `System32` reads the same when the Temp tab moves
    /// to `C:\Users`.
    #[test]
    fn a_windowed_strip_names_each_path_the_same_on_every_page() {
        let gui = "~\\aterm\\crates\\aterm-gui\\src";
        let cli = "~\\aterm\\crates\\aterm-cli\\src";
        let sys = "C:\\Windows\\System32";
        let titles: Vec<String> = [
            "~\\aterm",
            "~\\aterm",
            "~\\aterm",
            "~\\aterm",
            gui,
            cli,
            sys,
            "C:\\Windows\\Temp",
            "~\\aterm",
            "C:\\Program Files\\Git",
            "C:\\Windows\\System32\\drivers",
            "~\\aterm",
            "~\\aterm",
            gui,
            "~\\aterm",
        ]
        .map(String::from)
        .to_vec();
        let mut moved = titles.clone();
        moved[7] = "C:\\Users".to_string();
        let mut by_width: std::collections::HashMap<(usize, usize), String> =
            std::collections::HashMap::new();
        let mut apart = 0usize;
        for cols in 30..=240u16 {
            for active in 0..titles.len() {
                let segments = layout_segments(cols, titles.len(), active, false);
                let labels = seated(&segments, &titles, active);
                let on_page = |tab: usize| labels.iter().any(|(seat, _)| *seat == tab);
                if on_page(4) != on_page(5) {
                    apart += 1;
                }
                for (tab, label) in &labels {
                    let subject = titles[*tab].as_str();
                    if !is_path_subject(subject) || subject == "~\\aterm" {
                        continue;
                    }
                    let avail = chip_avail(&segments, *tab);
                    let at = format!("{cols} cols, active {active}, tab {tab} ({avail} cells)");
                    assert!(
                        honest_path_cut(*tab, subject, label),
                        "{at}: {label:?} is not an honest cut of {subject:?}"
                    );
                    let position = format!("{} · ", tab + 1);
                    let ordinal = *label == (tab + 1).to_string() || label.starts_with(&position);
                    if let Some(name) = match tab {
                        4 | 13 => Some("gui"),
                        5 => Some("cli"),
                        _ => None,
                    } && avail >= 4
                        && !ordinal
                    {
                        assert!(
                            label.contains(name),
                            "{at}: {label:?} lost the name that tells it apart"
                        );
                    }
                    // Whether a twin takes its ordinal depends on the twins cut beside
                    // it, so an ordinal is the one label allowed to change with the page.
                    if !ordinal {
                        let first = by_width
                            .entry((*tab, avail))
                            .or_insert_with(|| label.clone());
                        assert_eq!(
                            label, first,
                            "{at}: tab {tab} reads differently on another page"
                        );
                    }
                }
                let sys_of = |titles: &[String]| {
                    seated(&segments, titles, active)
                        .into_iter()
                        .find(|(tab, _)| *tab == 6)
                        .map(|(_, label)| label)
                };
                assert_eq!(
                    sys_of(&titles),
                    sys_of(&moved),
                    "{cols} cols, active {active}: System32's label turned on the Temp tab"
                );
            }
        }
        assert!(
            apart > 100,
            "vacuous: gui and cli shared every page ({apart})"
        );
    }

    /// One label pin for both band geometries: the chip-card interior pad
    /// ([`tab_content_layout`]) costs one title cell, so a pin at the budget edge
    /// differs by one glyph between the chip-card band (Windows, Linux) and the
    /// padless macOS strip. Pin both.
    fn on_band<'a>(chip_card: &'a str, padless: &'a str) -> &'a str {
        if STRIP_CHIP_CARDS { chip_card } else { padless }
    }

    /// `(tab, label)` for every chip the strip seats, in strip order. Skips the
    /// `None` entries [`distinct_chip_labels`] leaves for unseated tabs.
    fn seated(segments: &[TabSegment], titles: &[String], active: usize) -> Vec<(usize, String)> {
        let labels = distinct_chip_labels(segments, titles, None, active, None);
        segments
            .iter()
            .filter_map(|seg| match seg.kind {
                TabHit::Select(i) => {
                    Some((i, labels[i].clone().expect("a seated chip is labelled")))
                }
                _ => None,
            })
            .collect()
    }

    /// Just the painted strings of [`seated`], for a whole-strip pin.
    fn seated_labels(segments: &[TabSegment], titles: &[String], active: usize) -> Vec<String> {
        seated(segments, titles, active)
            .into_iter()
            .map(|(_, label)| label)
            .collect()
    }

    /// Deep sibling titles past the chrome cap stay distinguishable: the strip
    /// presents with no cap (`ChromeSurface::TabStrip`), so the pass sees whole
    /// titles and each chip keeps the component that differs.
    ///
    /// One case per region a fixed cut discards: cwds differing in the leaf (a head
    /// cut collapses them), two worktree ids inside a middle cut's `[48, N-48)`
    /// window (bare cwd rung and a prompt-set title), and trees differing past
    /// both. The fixture guard pins which cuts collapse each pair, and each
    /// collapsing cut is replayed as a negative control.
    #[test]
    fn deep_siblings_past_the_chrome_cap_stay_tellable_apart() {
        use crate::chrome_label::{ChromeSurface, capped_label, present};
        use aterm_grapheme::GraphemeClusters as _;
        // (one, two, what names one, what names two, widths, [head, middle] collapse).
        type Case = (
            String,
            String,
            &'static str,
            &'static str,
            &'static [u16],
            [bool; 2],
        );

        let head_cut: fn(&str) -> String =
            |t| format!("{}…", t.graphemes().take(96).collect::<String>());
        let middle_cut: fn(&str) -> String = |t| capped_label(t).into_owned();
        let deep = format!("user@m17-tower: ~/{}", "ab/".repeat(40));
        let cases: [Case; 4] = [
            (
                format!("{deep}deep-one"),
                format!("{deep}deep-two"),
                "deep-one",
                "deep-two",
                &[80],
                [true, false],
            ),
            (
                "~/src/github.com/alabsystems/aterm/.claude/worktrees/lane-bec3123b-89c-10/crates/aterm-gui/src/native_config/fixtures/model".to_string(),
                "~/src/github.com/alabsystems/aterm/.claude/worktrees/lane-bec3123b-89c-25/crates/aterm-gui/src/native_config/fixtures/model".to_string(),
                "10",
                "25",
                &[80],
                [false, true],
            ),
            (
                "user@m17-tower: ~/aterm/.claude/worktrees/lane-bec3123b-89c-10/crates/aterm-gui/src/native_config/description.rs".to_string(),
                "user@m17-tower: ~/aterm/.claude/worktrees/lane-bec3123b-89c-11/crates/aterm-gui/src/native_config/description.rs".to_string(),
                "89c-10",
                "89c-11",
                &[60, 120, 240],
                [false, true],
            ),
            (
                "~/work/clients/acme-corporation/platform/monorepo/services/payments/ledger/src/main/java/com/example/alpha/internal/handlers/impl/validation/rules/default/v2".to_string(),
                "~/work/clients/acme-corporation/platform/monorepo/services/payments/ledger/src/main/java/com/example/bravo/internal/handlers/impl/validation/rules/default/v2".to_string(),
                "alpha",
                "bravo",
                &[80],
                [true, true],
            ),
        ];
        for (one, two, own, other, widths, collapse) in cases {
            let raw = [one, two];
            // Fixture guard: past the cap, and collapsed by exactly the cuts named.
            assert!(raw.iter().all(|t| t.graphemes().count() > 96), "{raw:?}");
            let negatives: Vec<Vec<String>> = [head_cut, middle_cut]
                .into_iter()
                .zip(collapse)
                .filter_map(|(cut, collapses)| {
                    let cut: Vec<String> = raw.iter().map(|t| cut(t)).collect();
                    assert_eq!(
                        cut[0] == cut[1],
                        collapses,
                        "{raw:?}: which fixed cuts collapse the pair"
                    );
                    collapses.then_some(cut)
                })
                .collect();
            let titles: Vec<String> = raw
                .iter()
                .map(|t| present(t, ChromeSurface::TabStrip))
                .collect();
            assert_eq!(titles, raw, "the strip receives the whole titles");
            for &cols in widths {
                let segments = layout_segments(cols, 2, 0, false);
                let labels = seated_labels(&segments, &titles, 0);
                assert_eq!(labels.len(), 2, "{cols} cols: {labels:?}");
                assert!(
                    labels[0].contains(own) && !labels[0].contains(other),
                    "{cols} cols: {labels:?}"
                );
                assert!(
                    labels[1].contains(other) && !labels[1].contains(own),
                    "{cols} cols: {labels:?}"
                );
                for cut in &negatives {
                    let labels = seated_labels(&segments, cut, 0);
                    assert!(
                        labels
                            .iter()
                            .all(|label| !label.contains(own) && !label.contains(other)),
                        "{cols} cols: {labels:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn colliding_truncated_titles_keep_their_distinguishing_tails() {
        let titles = [
            "user@m17-tower: ~/aterm".to_string(),
            "user@m17-tower: $HOME/trust".to_string(),
            "user@m17-tower: ~".to_string(),
            "user@m17-tower: ~/aterm/crates".to_string(),
        ];
        // 80 cols, 4 tabs → ~19-cell shares: every title is cut, and the head
        // cut would hand tabs 0, 1 and 3 one identical label.
        let segments = layout_segments(80, titles.len(), 0, false);
        let labels = distinct_chip_labels(&segments, &titles, None, 0, None);
        let resolved: Vec<String> = labels.into_iter().map(Option::unwrap).collect();
        for (i, a) in resolved.iter().enumerate() {
            for (j, b) in resolved.iter().enumerate() {
                if i != j {
                    assert_ne!(a, b, "tabs {i} and {j} must be tellable apart at a glance");
                }
            }
        }
        // Every colliding label carries its tail, cut at the word boundary inside
        // the shared head so the clean cwd survives.
        assert_eq!(resolved[0], "…~/aterm");
        assert_eq!(resolved[1], "…$HOME/trust");
        assert_eq!(resolved[2], "…~");
        // Tab 3's expectation depends on the chip-card interior pad
        // ([`tab_content_layout`], [`STRIP_CHIP_CARDS`]): `~/aterm/crates` is 14
        // cells, a cell too wide with the pad (plain tail cut) and an exact fit
        // without it. Branch at runtime on the same const so the test runs on every
        // platform.
        assert_eq!(
            resolved[3],
            on_band("…/aterm/crates", "…~/aterm/crates"),
            "chip-card: a remainder one cell too wide falls back to the plain \
             tail cut; padless: the exact fit keeps the word-boundary cut, \
             `~` included"
        );

        // Titles that share their ending (a prompt that prints its status into
        // the title); the group's common suffix is shed first, so the kept tail
        // ends at the cwd.
        let suffixed = [
            "user@m17-tower: ~/aterm · Typing a command".to_string(),
            "user@m17-tower: $HOME/trust · Typing a command".to_string(),
            "user@m17-tower: ~ · Typing a command".to_string(),
            "user@m17-tower: ~/aterm/crates · Typing a command".to_string(),
        ];
        let segments = layout_segments(80, suffixed.len(), 0, false);
        let labels = distinct_chip_labels(&segments, &suffixed, None, 0, None);
        let resolved: Vec<String> = labels.into_iter().map(Option::unwrap).collect();
        assert!(
            resolved[0].ends_with("aterm"),
            "the shared suffix is shed, the cwd tail survives: {:?}",
            resolved[0]
        );
        assert!(resolved[1].ends_with("trust"), "{:?}", resolved[1]);
        assert!(resolved[2].ends_with('~'), "{:?}", resolved[2]);
        assert!(resolved[3].ends_with("crates"), "{:?}", resolved[3]);

        // One dialect per strip: on a wider band the head cuts stop colliding, but
        // the shared-head cluster still flips the whole family together.
        let segments = layout_segments(130, suffixed.len(), 0, false);
        let labels = distinct_chip_labels(&segments, &suffixed, None, 0, None);
        let resolved: Vec<String> = labels.into_iter().map(Option::unwrap).collect();
        assert_eq!(
            resolved,
            ["…~/aterm", "…$HOME/trust", "…~", "…~/aterm/crates"],
            "every member of the shared-prompt family speaks the tail dialect"
        );

        // One member with a different ending collapses the cluster's shed: a label
        // made only of the suffix the others share (`…· Ready`) names nothing, so
        // each still shows its own place.
        let mixed = [
            "user@m17-tower: ~/aterm · Ready".to_string(),
            "user@m17-tower: $HOME/trust · Ready".to_string(),
            "user@m17-tower: ~/ay · Ready".to_string(),
            "user@m17-tower: /tmp · Typing a command".to_string(),
        ];
        let segments = layout_segments(80, mixed.len(), 3, false);
        let resolved: Vec<String> = distinct_chip_labels(&segments, &mixed, None, 3, None)
            .into_iter()
            .enumerate()
            .map(|(i, l)| l.unwrap_or_else(|| mixed[i].clone()))
            .collect();
        for (i, label) in resolved.iter().enumerate().take(3) {
            let painted = label.trim_start_matches('…').trim_start();
            assert!(
                !painted.is_empty() && painted != "· Ready" && painted != "Ready",
                "tab {i} paints only the shared furniture: {resolved:?}"
            );
        }
        for (i, a) in resolved.iter().enumerate() {
            for (j, b) in resolved.iter().enumerate() {
                if i != j {
                    assert_ne!(a, b, "tabs {i} and {j} must differ: {resolved:?}");
                }
            }
        }

        // And the painted row carries the resolved labels (the pass feeds the
        // painter, not just itself); chip-card band only.
        if !STRIP_CHIP_CARDS {
            return;
        }
        let theme = Theme::default();
        let segments = layout_segments(80, titles.len(), 0, false);
        let mut row = vec![blank_cell(theme); 80];
        paint_strip(&mut row, &segments, &titles, None, 0, theme);
        let tab_text = |seg: &TabSegment| -> String {
            (seg.start_col..seg.end_col)
                .map(|c| row[c as usize].ch)
                .collect::<String>()
                .trim()
                .to_string()
        };
        let painted: Vec<String> = segments
            .iter()
            .filter(|seg| matches!(seg.kind, TabHit::Select(_)))
            .map(tab_text)
            .collect();
        assert!(
            painted[0].contains("aterm") && painted[1].contains("trust"),
            "the strip shows the tails that differ: {painted:?}"
        );
    }

    /// Ten to twenty identical shells across every column range the strip lays out:
    /// a non-active twin's label is its own position (bare or with a tail), or `…`
    /// exactly where the window seats no number; never a cut of the shared title.
    #[test]
    fn every_strip_width_labels_a_twin_honestly() {
        let mut marks = 0usize;
        // Two-digit ordinals over the whole column range, and three-digit ones over
        // the widths that can seat 120 chips.
        let sweep: [(usize, Vec<u16>); 5] = [
            (10, (1..=240).collect()),
            (11, (1..=240).collect()),
            (17, (1..=240).collect()),
            (20, (1..=240).collect()),
            (120, (240..=1200).step_by(53).collect()),
        ];
        for (tabs, widths) in sweep {
            let titles: Vec<String> = (0..tabs)
                .map(|_| "user@m17-tower: ~/aterm".to_string())
                .collect();
            for cols in widths {
                for active in [0usize, tabs / 2] {
                    let segments = layout_segments(cols, tabs, active, false);
                    let labels = distinct_chip_labels(&segments, &titles, None, active, None);
                    for seg in &segments {
                        let TabHit::Select(i) = seg.kind else {
                            continue;
                        };
                        if i == active {
                            continue;
                        }
                        let layout = tab_content_layout(seg, PLAIN_TAB);
                        let avail =
                            usize::from(layout.title_end.saturating_sub(layout.title_start));
                        // Only cut titles get ordinals; a title that fits is painted
                        // whole.
                        if strip_display_cells(&titles[i]) <= avail {
                            continue;
                        }
                        let label = labels[i].as_deref().unwrap_or("");
                        let digits = (i + 1).to_string();
                        let where_ = format!("{tabs} tabs, {cols} cols, tab {i}, avail {avail}");
                        if label.is_empty() {
                            assert_eq!(avail, 0, "{where_}: a window with room says something");
                            continue;
                        }
                        if label == "…" {
                            marks += 1;
                            assert!(
                                strip_display_cells(&digits) > avail,
                                "{where_}: the mark is only for a window that \
                                 seats no number — this one seats {digits}"
                            );
                            continue;
                        }
                        assert!(
                            label == digits || label.starts_with(&format!("{digits} · ")),
                            "{where_}: {label:?} is neither this tab's position \
                             nor an honest mark"
                        );
                        assert!(
                            strip_display_cells(label) <= avail,
                            "{where_}: {label:?} overflows its window"
                        );
                    }
                }
            }
        }
        assert!(
            marks > 0,
            "the mark's window is reachable — if it ever stops being, this \
             test is the place that says so"
        );
    }

    /// The ordinal's tail follows the survivor rule ([`furniture_survivor_recut`]):
    /// it is re-cut instead of spending the window on `· Ready`, the state the
    /// other chips share — when one member in a different state collapses the
    /// cluster's shed, and when the twins are a family of their own whose ending
    /// two chips outside it share ([`shared_furniture`]).
    ///
    /// Plain titles: a path subject (`~/work/aterm · Ready`) takes the path's own
    /// cut behind its ordinal instead ([`path_ordinal_label`]).
    ///
    /// Not on macOS: it paints no band ([`STRIP_IS_CHROME_BAND`]), and this pins
    /// the chip-card avail (14). The cross-band `on_band` tests cover macOS.
    #[cfg(not(target_os = "macos"))]
    #[test]
    fn an_ordinal_tail_is_never_the_shared_furniture() {
        // A head every title shares, long enough that all four cluster (the
        // different state collapses the shed), then one too short for that
        // (the twins cluster alone, and the two other chips share their state).
        for (head, last_state) in [("build: ", "Typing a command"), ("work/", "Ready")] {
            let titles = [
                format!("{head}aterm · Ready"),
                format!("{head}aterm · Ready"),
                format!("{head}trust · Ready"),
                format!("{head}other · {last_state}"),
            ];
            let segments = layout_segments(80, titles.len(), 0, false);
            let resolved: Vec<String> = distinct_chip_labels(&segments, &titles, None, 0, None)
                .into_iter()
                .map(Option::unwrap)
                .collect();
            let twin = &resolved[1];
            assert!(
                twin.starts_with("2 · "),
                "{head:?}: the non-active twin is labelled by its position: {twin:?}"
            );
            assert!(
                !twin.contains("Ready"),
                "{head:?}: the tail is this tab's own text, not the state every chip \
                 on the strip shares: {twin:?}"
            );
            assert!(
                twin.contains("aterm"),
                "{head:?}: and what it spends the tail on is the name that tells it \
                 apart: {twin:?}"
            );
            assert!(
                resolved[0].contains("aterm"),
                "{head:?}: the active twin keeps that name too, never the furniture \
                 alone: {:?}",
                resolved[0]
            );
            // Fixture guard: the word-boundary cut at the tail's budget lands in
            // the furniture.
            let seg = segments
                .iter()
                .find(|seg| matches!(seg.kind, TabHit::Select(1)))
                .expect("the twin has a chip");
            let layout = tab_content_layout(seg, PLAIN_TAB);
            let avail = usize::from(layout.title_end.saturating_sub(layout.title_start));
            let naive = truncate_title_tail(&titles[1], avail.saturating_sub(4));
            assert!(
                naive.contains("Ready") && !naive.contains("aterm"),
                "{head:?} fixture: the naive tail is pure furniture: {naive:?}"
            );
        }
    }

    /// A tail one chip outside a cluster happens to share is not furniture: it is
    /// the name that tells the cluster's members apart ([`shared_furniture`]).
    /// Each strip below is cut at 80 columns; `named` lists, per tab, the text
    /// its chip must keep. A family of twins sheds an ending only when two chips
    /// outside it share it; the last strip has one.
    ///
    /// Not on macOS: it paints no band ([`STRIP_IS_CHROME_BAND`]), and this pins
    /// the chip-card avail.
    #[cfg(not(target_os = "macos"))]
    #[test]
    fn a_tail_shared_outside_the_cluster_still_names_its_members() {
        let strips: [([&str; 4], [Option<&str>; 4]); 4] = [
            (
                [
                    "building: aterm",
                    "building: trust",
                    "checking: aterm",
                    "checking: trust",
                ],
                [Some("aterm"), Some("trust"), Some("aterm"), Some("trust")],
            ),
            (
                [
                    "user@gb10: ~/aterm",
                    "user@gb10: $HOME/trust",
                    "root@gb10: ~/aterm",
                    "zsh",
                ],
                [Some("aterm"), Some("trust"), None, None],
            ),
            (
                [
                    "building: aterm",
                    "building: trust",
                    "nvim ~/src/aterm",
                    "zsh",
                ],
                [Some("aterm"), Some("trust"), None, None],
            ),
            (
                [
                    "building: aterm",
                    "building: aterm",
                    "nvim ~/src/aterm",
                    "zsh",
                ],
                [Some("aterm"), Some("aterm"), None, None],
            ),
        ];
        for (raw, named) in strips {
            let titles = raw.map(str::to_string);
            let segments = layout_segments(80, titles.len(), 0, false);
            let resolved: Vec<String> = distinct_chip_labels(&segments, &titles, None, 0, None)
                .into_iter()
                .map(Option::unwrap)
                .collect();
            for (tab, name) in named.iter().enumerate() {
                let Some(name) = name else { continue };
                assert!(
                    resolved[tab].contains(name),
                    "{raw:?}: tab {tab} keeps {name:?}, the name that tells it apart: \
                     {resolved:?}"
                );
                assert!(
                    titles[tab] != resolved[tab],
                    "{raw:?} fixture: tab {tab} is cut: {resolved:?}"
                );
                for (other, other_name) in named.iter().enumerate() {
                    if other_name.is_some_and(|n| n != *name) {
                        assert_ne!(
                            resolved[tab], resolved[other],
                            "{raw:?}: tabs {tab} and {other} are told apart"
                        );
                    }
                }
            }
        }
    }

    /// A twin family's shed ending is whole words: the tail two outside chips
    /// share (`ing: aterm`) is snapped to a token boundary, so the twins never
    /// paint a word cut off with no mark (`build`, `2 · build`), at any width.
    #[test]
    fn a_shed_ending_is_whole_words() {
        // (strip, columns): each width has room for the twins' whole first word,
        // so a shed that leaves less is the bug, not the budget.
        let strips: [(&[&str], u16); 2] = [
            (
                &[
                    "building: aterm",
                    "building: aterm",
                    "checking: aterm",
                    "linting: aterm",
                    "zsh",
                ],
                80,
            ),
            (
                &[
                    "checking: other · Ready",
                    "checking: other · Ready",
                    "checking: other · Ready",
                    "linting: other · Ready",
                    "linting: other · Ready",
                    "make",
                    "zsh",
                ],
                150,
            ),
        ];
        for (raw, cols) in strips {
            let titles: Vec<String> = raw.iter().map(|t| t.to_string()).collect();
            let segments = layout_segments(cols, titles.len(), 0, false);
            let resolved = distinct_chip_labels(&segments, &titles, None, 0, None);
            for (tab, label) in resolved.iter().enumerate() {
                let Some(label) = label else { continue };
                // The twins: a lone chip's own head cut is not this rule's.
                if titles.iter().filter(|t| **t == titles[tab]).count() < 2 {
                    continue;
                }
                let core = label
                    .trim_start_matches(|c: char| c.is_ascii_digit())
                    .trim_start_matches(" · ")
                    .trim_start_matches('…');
                if core.is_empty() || label.ends_with('…') {
                    continue;
                }
                let Some(at) = titles[tab].find(core) else {
                    continue;
                };
                let next = titles[tab][at + core.len()..].chars().next();
                assert!(
                    !(core.ends_with(char::is_alphanumeric)
                        && next.is_some_and(char::is_alphanumeric)),
                    "{raw:?} at {cols} columns: tab {tab} paints {label:?}, a word of {:?} \
                     cut off with no mark: {resolved:?}",
                    titles[tab]
                );
            }
        }
    }

    /// Whether `painted` is context for the path `subject` in [`twin_tail`]'s sense:
    /// the subject whole, whole trailing components behind the mark, or a non-empty
    /// head with a trailing mark; never a directory name's cut-off end. Chips whose
    /// twins are not on the strip, and the selected twin, paint the path's own cut
    /// ([`path_chip_label`]), which may also be the leaf whole or its head keeping
    /// three cells (`ate…`).
    fn is_twin_context(subject: &str, painted: &str) -> bool {
        if painted == subject {
            return true;
        }
        if subject
            .strip_suffix(painted)
            .is_some_and(|before| !painted.is_empty() && before.ends_with(['/', '\\']))
        {
            return true;
        }
        let leaf = subject.rsplit(['/', '\\']).next().unwrap_or_default();
        if painted.strip_suffix('…').is_some_and(|head| {
            leaf.starts_with(head) && strip_display_cells(head) >= strip_display_cells(leaf).min(3)
        }) {
            return true;
        }
        if let Some(tail) = painted.strip_prefix('…') {
            return !tail.is_empty()
                && subject.strip_suffix(tail).is_some_and(|before| {
                    tail.starts_with(['/', '\\']) || before.ends_with(['/', '\\'])
                });
        }
        painted
            .strip_suffix('…')
            .is_some_and(|head| !head.is_empty() && subject.starts_with(head))
    }

    /// Three shells in one directory with identical titles, over every title window
    /// a chip can get: a family of twins paints context beside its ordinals
    /// ([`twin_tail`]): the subject whole, its whole trailing components behind the
    /// mark, or its head; never the cut-off end of a directory name (`…term`).
    /// Pinned by value per window and for deeper directories, then end to end at the
    /// layout's own windows.
    #[test]
    fn twins_keep_whole_components_or_the_head_at_every_width() {
        let subject = "~\\aterm";
        let title = subject;
        // Its twins share the whole title, so nothing is furniture.
        let furniture = shared_furniture(title, &[title, title], 1);
        // (window, the non-active twin's label, the active twin's text).
        let expected: [(usize, &str, &str); 12] = [
            (0, "", ""),
            (1, "2", "…"),
            (2, "2", "~…"),
            (3, "2", "~\\…"),
            (4, "2", "~\\a…"),
            // `2` + ` · ` costs four; a one-cell tail says nothing.
            (5, "2", "~\\at…"),
            (6, "2 · ~…", "…aterm"),
            (7, "2 · ~\\…", "~\\aterm"),
            (8, "2 · ~\\a…", "~\\aterm"),
            (9, "2 · ~\\at…", "~\\aterm"),
            (10, "2 · …aterm", "~\\aterm"),
            (11, "2 · ~\\aterm", "~\\aterm"),
        ];
        for (avail, ordinal, active) in expected {
            assert_eq!(
                ordinal_chip_label(1, title, subject, "", avail, furniture, twin_tail),
                ordinal,
                "window {avail}: the non-active twin"
            );
            assert_eq!(
                twin_tail(subject, "", avail),
                active,
                "window {avail}: the active twin"
            );
        }
        // The family's own cut is what split the name on glass (`2 · …term`).
        assert_eq!(family_tail(subject, "", 5), "…term");
        assert_eq!(family_tail(subject, "", 3), "…rm");
        // A deeper directory keeps as many whole components as fit (`\` counts as a
        // component boundary even though the tail cut does not treat it as a word
        // boundary).
        assert_eq!(twin_tail("~\\work\\api", "", 10), "~\\work\\api");
        assert_eq!(twin_tail("~\\work\\api", "", 9), "…work\\api");
        assert_eq!(twin_tail("~\\work\\api", "", 8), "…api");
        assert_eq!(twin_tail("~\\work\\api", "", 5), "…\\api");
        assert_eq!(twin_tail("~/work/api", "", 4), "…api");
        assert_eq!(twin_tail("~/work/api", "", 3), "~/…");
        // A name is not a path: a family of names keeps the family's cut.
        assert_eq!(twin_tail("Settings.toml", "", 6), "….toml");
        assert_eq!(twin_tail("same shell", "shell", 4), "…ell");
        // A prompt title keeps the cwd word whole with its mark, then its whole
        // components, then the marked head of the whole title, so a dropped
        // `user@host: ` never reads as a local cwd.
        let core = "user@m17-tower: ~/aterm";
        assert_eq!(twin_tail(core, "~/aterm", 10), "…~/aterm");
        assert_eq!(twin_tail(core, "~/aterm", 8), "…~/aterm");
        assert_eq!(twin_tail(core, "~/aterm", 7), "…/aterm");
        assert_eq!(twin_tail(core, "~/aterm", 6), "…aterm");
        assert_eq!(twin_tail(core, "~/aterm", 5), "ayat…");

        // End to end at the layout's own windows, every width and selection.
        let mut cut = 0usize;
        let titles: Vec<String> = (0..3).map(|_| subject.to_string()).collect();
        for cols in 12..=120u16 {
            for active in 0..3 {
                let segments = layout_segments(cols, 3, active, false);
                let labels = distinct_chip_labels(&segments, &titles, None, active, None);
                for seg in &segments {
                    let TabHit::Select(i) = seg.kind else {
                        continue;
                    };
                    let Some(label) = labels[i].as_deref() else {
                        continue;
                    };
                    if label == subject {
                        continue; // wide enough: painted whole
                    }
                    cut += 1;
                    let digits = (i + 1).to_string();
                    let painted = label.strip_prefix(&format!("{digits} · ")).unwrap_or(label);
                    assert!(
                        painted.is_empty()
                            || painted == "…"
                            || painted == digits
                            || is_twin_context(subject, painted),
                        "{cols} cols, active {active}, tab {i}: {label:?} is not context \
                         for {subject:?}, a number, or the mark"
                    );
                }
            }
        }
        // The title is seven cells, so only the narrow widths cut it.
        assert!(cut > 50, "vacuous: only {cut} chips were cut");
    }

    /// Twins in a roomy strip both name their place. Two shells in one directory
    /// whose name has a space: the quiet twin shows the path's own cut behind its
    /// number (`2 · …\work\sub dir`), not a word cut at that space (`2 · …dir`).
    /// Two Git Bash prompts in one directory: the active twin shows the whole
    /// trailing components (`…/work/ünï 日本`), not the last word (`…日本`).
    #[test]
    fn roomy_twins_name_their_directory_whole() {
        let dir = r"C:\Users\me\AppData\Local\Temp\wp-shells\work\sub dir".to_string();
        let titles = vec![dir.clone(), dir];
        let segments = layout_segments(100, 2, 0, false);
        let seats = seated(&segments, &titles, 0);
        assert!(seats[0].1.ends_with(r"work\sub dir"), "{seats:?}");
        // The cut keeps whole trailing components (`…work\sub dir`, the mark in
        // place of the separator it cut at), never a word of the last one.
        assert!(
            seats[1].1.starts_with("2 · …") && seats[1].1.ends_with(r"work\sub dir"),
            "the quiet twin names the directory: {seats:?}"
        );

        let prompt = "me@box MINGW64 /tmp/wp-shells/work/ünï 日本".to_string();
        let titles = vec![prompt.clone(), prompt];
        let segments = layout_segments(48, 2, 1, false);
        let seats = seated(&segments, &titles, 1);
        let active = &seats[1].1;
        assert!(
            active.ends_with("/ünï 日本") && active.len() > "…/ünï 日本".len(),
            "the active twin keeps whole components: {seats:?}"
        );
    }

    /// [`twin_tail`]'s reach on a pressure strip. A family of twins that kept its
    /// head flips no loner into the tail dialect; one whose directory name fits
    /// behind the mark paints whole components and the loner flips with it. A
    /// family mixing twins with another member keeps the tail dialect for its twins
    /// (`…gui` vs `…cli`).
    #[test]
    fn twin_context_stops_at_a_mixed_family_and_a_head_flips_no_loner() {
        let strip = |dir: &str| -> Vec<String> {
            vec![
                dir.to_string(),
                dir.to_string(),
                "cargo build --release".to_string(),
                "README.md".to_string(),
                "Setup.sh".to_string(),
            ]
        };
        // The deep directory: `workspace` does not fit behind a quiet chip's
        // ordinal, so the family keeps its head — and the loner keeps its own.
        let titles = strip("~/wave/workspace");
        let segments = layout_segments(48, titles.len(), 0, false);
        let seats = seated(&segments, &titles, 0);
        assert_eq!(
            seats.iter().map(|(tab, _)| *tab).collect::<Vec<_>>(),
            [0, 1, 2],
            "the fixture's geometry: twin, twin, loner on one page"
        );
        assert!(
            seats[1].1.starts_with("2 · ~/") && seats[1].1.ends_with('…'),
            "the quiet twin keeps its head: {seats:?}"
        );
        assert!(
            seats[2].1.starts_with("cargo"),
            "the loner keeps the head dialect the family speaks: {seats:?}"
        );
        // The shallow directory: `api` fits whole behind the mark — the tail
        // dialect — and the loner flips with the family.
        let titles = strip("~/wave/services/api");
        let segments = layout_segments(48, titles.len(), 0, false);
        let seats = seated(&segments, &titles, 0);
        assert!(
            seats[1].1.starts_with("2 · …") && seats[1].1.ends_with("api"),
            "the quiet twin names the directory by its whole component: {seats:?}"
        );
        assert!(
            seats[2].1.starts_with('…') && seats[2].1.ends_with("release"),
            "the loner joins the tail dialect: {seats:?}"
        );

        // The mixed family keeps the tail dialect, twins included, at every width
        // that seats and cuts all three and gives the twin a tail. Prompt titles,
        // so the tail cut can land inside a component, where a family of twins
        // would take its head and a mixed one must not.
        let titles = [
            "user@m17-tower: ~/src/aterm-gui".to_string(),
            "user@m17-tower: ~/src/aterm-gui".to_string(),
            "user@m17-tower: ~/src/aterm-cli".to_string(),
        ];
        let mut mixed = 0usize;
        for cols in 40..=160u16 {
            let segments = layout_segments(cols, titles.len(), 0, false);
            let seats = seated(&segments, &titles, 0);
            if seats.iter().map(|(tab, _)| *tab).collect::<Vec<_>>() != [0, 1, 2]
                || !seats[2].1.contains('…')
                || !seats[1].1.starts_with("2 · ")
            {
                continue;
            }
            mixed += 1;
            let twin = &seats[1].1["2 · ".len()..];
            assert!(
                twin.starts_with('…') && twin != seats[2].1,
                "{cols} cols: a twin beside a sibling that differs keeps the \
                 ending that tells them apart: {seats:?}"
            );
        }
        assert!(mixed > 0, "vacuous: no width seated the mixed family cut");
    }

    /// An out-of-range `active` is clamped by the label pass as by
    /// `layout_segments`, so the widened last chip keeps its title.
    #[test]
    fn an_out_of_range_selection_resolves_like_the_clamped_one() {
        let titles: Vec<String> = (0..4)
            .map(|_| "user@m17-tower: ~/aterm".to_string())
            .collect();
        for cols in [40u16, 80, 130] {
            let segments = layout_segments(cols, titles.len(), 99, false);
            let clamped = layout_segments(cols, titles.len(), titles.len() - 1, false);
            assert_eq!(segments, clamped, "fixture: the LAYOUT already clamps");
            let out_of_range = distinct_chip_labels(&segments, &titles, None, 99, None);
            let in_range = distinct_chip_labels(&segments, &titles, None, titles.len() - 1, None);
            assert_eq!(
                out_of_range, in_range,
                "{cols} cols: the labels follow the same clamp the layout did"
            );
            assert!(
                out_of_range[titles.len() - 1]
                    .as_deref()
                    .is_some_and(|label| !label.starts_with('4')),
                "the chip the layout widened keeps its TITLE: {out_of_range:?}"
            );
        }
    }

    /// Titles for the one-dialect test (once a cluster on a pressure strip flips to
    /// tail cuts, the cut loners flip too; a roomy strip keeps distinct heads' head
    /// cuts). Twins and loners are interleaved so a seated page ([`strip_window`])
    /// mixes both. The 13-column seat keeps eight title cells on a chip-card band and
    /// nine on a padless one ([`on_band`]).
    fn one_dialect_titles() -> [&'static str; 10] {
        [
            "Settings.toml",
            "Settings.toml",
            "Settings.toml",
            "README.md",         // loner: shares no head
            "Settings.toml",     //
            "cargo build",       // loner
            "Settings.toml",     //
            "Setup.sh",          // clusters via the `Set` head
            "Settings.toml",     //
            "Settings.toml.bak", // clusters
        ]
    }

    #[test]
    fn a_pressure_strip_speaks_one_truncation_dialect() {
        let titles: Vec<String> = one_dialect_titles().iter().map(|t| t.to_string()).collect();
        // Selecting the last tab seats the page that mixes a flipped family with a
        // loner (five of ten seated, [`STRIP_SEATS_FLOOR`]).
        let segments = layout_segments(70, titles.len(), 9, false);
        let resolved = seated_labels(&segments, &titles, 9);
        // The two seated twins are ordinals with a tail; every other cut chip speaks
        // the tail dialect. (`Setup.sh` fits uncut.) The twins share a family with
        // `Settings.toml.bak`, so they keep the family's tail cut
        // ([`twin_tail`] does not apply).
        assert_eq!(
            resolved,
            [
                "…build",
                on_band("7 · …oml", "7 · …toml"),
                "Setup.sh",
                on_band("9 · …oml", "9 · …toml"),
                "….bak"
            ],
            "one strip, one dialect — and the twins are addressable"
        );

        // Two visible cells per chip: `README.md` and `cargo build` both tail-cut
        // to `…d`, so colliding candidates keep their head cuts (`R…`/`c…`).
        //
        // Built by hand: [`layout_segments`] seats a window instead of this
        // geometry ([`STRIP_SEATS_FLOOR`]), but the label pass is a pure function of
        // the segments it is handed and must hold for any of them.
        let segments: Vec<TabSegment> = std::iter::once(TabSegment {
            start_col: 0,
            end_col: 18,
            close_col: Some(16),
            kind: TabHit::Select(0),
            solo: false,
        })
        .chain((1..10u16).map(|i| {
            let start = 18 + (i - 1) * 6;
            TabSegment {
                start_col: start,
                end_col: start + 6,
                close_col: Some(start + 4),
                kind: TabHit::Select(usize::from(i)),
                solo: false,
            }
        }))
        .collect();
        let resolved = seated_labels(&segments, &titles, 0);
        assert_eq!(
            resolved,
            [
                "Settings.toml",
                "2",
                "3",
                "R…",
                "5",
                "c…",
                "7",
                "…h",
                "9",
                "…k"
            ],
            "distinctness outranks dialect when the tail cut cannot distinguish"
        );
        for (i, a) in resolved.iter().enumerate() {
            for (j, b) in resolved.iter().enumerate() {
                if i != j {
                    assert_ne!(a, b, "tabs {i} and {j} collide at two visible cells");
                }
            }
        }

        // A roomy strip: the shared-prompt family flips to tail cuts, but the
        // unrelated loner keeps its head cut.
        let roomy = [
            "user@m17-tower: ~/aterm".to_string(),
            "user@m17-tower: $HOME/trust".to_string(),
            "user@m17-tower: ~".to_string(),
            "user@m17-tower: ~/aterm/crates".to_string(),
            "zzz-not-a-prompt-here".to_string(),
        ];
        let segments = layout_segments(80, roomy.len(), 0, false);
        let resolved: Vec<String> = distinct_chip_labels(&segments, &roomy, None, 0, None)
            .into_iter()
            .map(Option::unwrap)
            .collect();
        assert!(
            resolved[0].starts_with('…') && resolved[1].starts_with('…'),
            "the family still flips together: {resolved:?}"
        );
        assert_eq!(
            resolved[4],
            on_band("zzz-not-a…", "zzz-not-a-…"),
            "a roomy strip's distinct-headed loner keeps the shipped head cut"
        );
    }

    /// `paint_strip` distinguishes the active tab (raised bg, full-strength fg,
    /// accent underline) from inactive ones (dimmed fg that recedes). (Title cells:
    /// `terminal_tab_removes_icon_raster_and_recovers_title_cells`; the `✕`:
    /// `the_close_mark_is_hover_revealed_and_resident_on_the_selected_card`.)
    #[test]
    fn paint_active_inactive_and_title() {
        let theme = Theme::default();
        let cols = 40usize;
        let mut row = vec![strip_cell(' ', &strip_colors(theme), StripRole::Inactive); cols];
        let segs = layout_segments(cols as u16, 2, 0, false);
        let titles = vec!["zsh".to_string(), "vim".to_string()];
        paint_strip(&mut row, &segs, &titles, None, 0, theme);
        let fg_rgb = [
            ((theme.fg >> 16) & 0xff) as u8,
            ((theme.fg >> 8) & 0xff) as u8,
            (theme.fg & 0xff) as u8,
        ];
        let bg_rgb = [
            ((theme.bg >> 16) & 0xff) as u8,
            ((theme.bg >> 8) & 0xff) as u8,
            (theme.bg & 0xff) as u8,
        ];
        let t0 = &segs[0];
        // The active tab's card cell: on a chip-card band the leading cell is the
        // gutter, so the card starts one column in.
        let card0 = t0.start_col as usize + usize::from(STRIP_CHIP_CARDS);
        // Tab 0 is active: raised bg (on Windows the terminal's own ground, a
        // surface apart from the band), bold full-strength text, and the accent
        // underline except on a chip-card band, where the filled card alone marks
        // the selection.
        if SELECTED_TAB_IS_GROUND {
            assert_eq!(row[card0].bg, bg_rgb, "the selected tab is the ground");
            assert_ne!(
                row[card0].bg,
                strip_colors(theme).band_bg,
                "apart from the band"
            );
        } else {
            assert_ne!(
                row[card0].bg, bg_rgb,
                "active tab bg is raised above the body"
            );
        }
        assert_eq!(
            row[card0].fg, fg_rgb,
            "active tab fg = full-strength theme fg"
        );
        if STRIP_CHIP_CARDS {
            assert_eq!(
                row[card0].underline,
                UnderlineStyle::None,
                "a chip-card selection is a filled card, not an underline"
            );
        } else {
            assert_eq!(
                row[card0].underline,
                UnderlineStyle::Single,
                "active tab carries the underline accent"
            );
            assert_eq!(
                row[card0].underline_color,
                Some([
                    ((theme.cursor >> 16) & 0xff) as u8,
                    ((theme.cursor >> 8) & 0xff) as u8,
                    (theme.cursor & 0xff) as u8,
                ]),
                "selected underline follows the theme accent"
            );
        }
        assert!(row[card0].bold, "active identity is explicit");
        // Tab 1 is inactive: distinct from the active card and not bold. On the flat
        // strip it recedes to the band; on a chip-card band it keeps a quiet card,
        // with the gutter cell staying band.
        let t1 = &segs[1];
        let card1 = t1.start_col as usize + usize::from(STRIP_CHIP_CARDS);
        let colors = strip_colors(theme);
        if STRIP_CHIP_CARDS {
            assert_eq!(
                row[t1.start_col as usize].bg, colors.band_bg,
                "the gutter column between cards is bare band"
            );
            assert_eq!(
                row[card1].bg, colors.chip_bg,
                "inactive tab bg = its own quiet chip card"
            );
        } else {
            assert_eq!(
                row[card1].bg, colors.band_bg,
                "inactive tab bg = the band (recedes)"
            );
        }
        assert_ne!(row[card1].bg, row[card0].bg, "inactive differs from active");
        assert!(!row[card1].bold, "inactive tab text is not bold");
    }

    /// A wide char in a tab title paints as a real lead + continuation pair, never
    /// a `·` placeholder or an overlapping narrow cell: emoji, CJK and a width-2
    /// BMP symbol, on a chip and on the solo title band.
    #[test]
    fn wide_title_chars_paint_as_lead_plus_continuation() {
        let theme = Theme::default();
        for title in ["🚀 build", "日本語シェル", "✅ done"] {
            let titles = [title.to_string(), "plain".to_string()];
            let segs = layout_segments(80, 2, 0, false);
            let mut row = vec![blank_cell(theme); 80];
            paint_strip(&mut row, &segs, &titles, None, 0, theme);
            assert!(
                row.iter().all(|cell| cell.ch != '·'),
                "{title:?}: the middle-dot placeholder is gone"
            );
            let t0 = segs[0];
            let layout = tab_content_layout(
                &t0,
                TabStripMetadata::from_presentation(&crate::tab_model::TabPresentation::terminal(
                    title,
                )),
            );
            // Every wide char owns its lead cell plus one `wide` continuation, and
            // widths line up so nothing overlaps the close/status/next chip.
            let mut col = layout.title_start;
            for ch in title.chars() {
                let w = strip_char_cells(ch);
                if col + w > layout.title_end {
                    break;
                }
                assert_eq!(
                    row[col as usize].ch, ch,
                    "{title:?}: the char itself is painted at its display column"
                );
                assert!(!row[col as usize].wide, "{title:?}: the lead is a lead");
                if w == 2 {
                    let tail = row[col as usize + 1];
                    assert!(tail.wide, "{title:?}: width-2 char carries a continuation");
                    assert_eq!(tail.ch, ' ', "{title:?}: the continuation has no glyph");
                }
                col += w;
            }
            // And the span the walk consumed never escapes the title box.
            assert!(col <= layout.title_end);

            // The solo band's own painter: the title's first char leads its pair.
            let metadata = [TabStripMetadata::from_presentation(
                &crate::tab_model::TabPresentation::terminal(title),
            )];
            let segments = forced_solo_segments(40, &metadata);
            let mut row = vec![blank_cell(theme); 40];
            paint_strip_with_metadata(
                &mut row,
                &segments,
                &[title.to_string()],
                &metadata,
                StripPaint::default(),
                0,
                theme,
                None,
            );
            let first = row
                .iter()
                .position(|cell| cell.ch != ' ')
                .expect("the solo title is drawn");
            let lead = title.chars().next().unwrap();
            assert_eq!(row[first].ch, lead, "{title:?} on the solo band");
            assert!(
                row[first + 1].wide,
                "{title:?}: solo lead + continuation, not a narrow cell"
            );
        }
    }

    /// A wide title on a narrow segment drops a straddling wide char whole rather
    /// than bleeding over the close `✕` or the neighbouring chip.
    #[test]
    fn a_wide_char_never_straddles_the_title_boundary() {
        let theme = Theme::default();
        // 24 cols / 2 tabs: the two-chip pressure split (12 and 9), small title spans.
        let segs = layout_segments(24, 2, 0, false);
        let titles = ["日本語シェルの長い名前".to_string(), "第二".to_string()];
        let mut row = vec![blank_cell(theme); 24];
        paint_strip(&mut row, &segs, &titles, None, 0, theme);
        for seg in segs.iter().filter(|s| matches!(s.kind, TabHit::Select(_))) {
            let boundary = match seg.close_col {
                Some(cx) => cx.saturating_sub(1),
                None => seg.end_col.saturating_sub(1),
            };
            // The boundary cell (the pad before the ✕ / the edge) is never the
            // continuation of a glyph that started inside the title.
            assert!(
                !row[boundary as usize].wide || boundary == 0,
                "a continuation leaked onto the pad at {boundary}"
            );
        }
        // No orphaned continuation anywhere: every `wide` cell follows a
        // non-space lead.
        for col in 1..row.len() {
            if row[col].wide {
                assert_ne!(
                    row[col - 1].ch,
                    ' ',
                    "continuation at {col} has no lead glyph"
                );
            }
        }
    }

    /// Three realistic strips: eight tabs with one identical title, eight different
    /// jobs, and twins (a trio and a pair) among strangers. Tests below run all three.
    fn realistic_strips() -> [(&'static str, Vec<String>); 3] {
        let same: Vec<String> = (0..8)
            .map(|_| "claude --resume release-notes".to_string())
            .collect();
        let distinct: Vec<String> = [
            "vim src/main.rs",
            "cargo test",
            "~/aterm",
            "ssh build-01",
            "psql aterm_dev",
            "docker compose",
            "git log",
            "claude",
        ]
        .iter()
        .map(|title| (*title).to_string())
        .collect();
        // Duplicates long enough to be cut: twins whose whole title fits are
        // painted whole and take no ordinal.
        let mixed: Vec<String> = [
            "claude --resume release-notes",
            "claude --resume release-notes",
            "vim TODO",
            "~/src/aterm/crates/aterm-gui",
            "~/src/aterm/crates/aterm-gui",
            "cargo test",
            "git log",
            "claude --resume release-notes",
        ]
        .iter()
        .map(|title| (*title).to_string())
        .collect();
        [("same", same), ("distinct", distinct), ("mixed", mixed)]
    }

    /// A painted label with its furniture (a leading ordinal and the elision marks)
    /// taken off, leaving only the characters the chip spent on text.
    fn painted_text(label: &str) -> &str {
        label
            .trim_start_matches(|c: char| c.is_ascii_digit())
            .trim_start_matches(' ')
            .trim_start_matches('\u{00b7}')
            .trim_start_matches(' ')
            .trim_matches('…')
    }

    /// Whether this chip says something of its own tab: a cut of its title (the
    /// tail dialect, the ordinal's tail, a head cut, or a command cut's head and
    /// file, [`command_cut`]).
    fn says_its_own(label: &str, title: &str) -> bool {
        let painted = painted_text(label);
        if let Some((head, tail)) = painted.rsplit_once(MIDDLE_MARK) {
            return title.starts_with(head) && title.ends_with(tail);
        }
        !painted.is_empty() && title.contains(painted)
    }

    /// Sweep of 1 to 20 tabs, 40 to 300 columns, the selection at each end and in the
    /// middle, with and without the leading `↻`. Six laws:
    ///
    /// 1. Nothing panics and no segment leaves the strip.
    /// 2. Chips stay ordered and disjoint, so a column names one tab.
    /// 3. The selected chip is always seated while any chip is.
    /// 4. No chip below [`STRIP_SEATS_FLOOR`] once the strip is windowing, unless it
    ///    is down to the [`STRIP_MIN_SEATS`] pair.
    /// 5. Every overflow mark lands on a column [`hit_test`] calls bare band.
    /// 6. A mark appears on exactly the side with unseated tabs (or neither, on a
    ///    band with no column to spare).
    #[test]
    fn the_windowed_strip_degrades_sanely_at_every_width_and_tab_count() {
        for tabs in 1..=20usize {
            for cols in (40..=300u16).step_by(1) {
                for active in [0, tabs / 2, tabs - 1] {
                    for update in [false, true] {
                        let segs = layout_segments(cols, tabs, active, update);
                        let chips: Vec<&TabSegment> = segs
                            .iter()
                            .filter(|seg| matches!(seg.kind, TabHit::Select(_)))
                            .collect();
                        let where_ =
                            format!("{tabs} tabs, {cols} cols, active {active}, update {update}");
                        // 1 + 2.
                        for pair in segs.windows(2) {
                            assert!(
                                pair[0].end_col <= pair[1].start_col,
                                "{where_}: segments overlap: {segs:?}"
                            );
                        }
                        for seg in &segs {
                            assert!(seg.start_col < seg.end_col, "{where_}: empty segment");
                            assert!(seg.end_col <= cols, "{where_}: segment past the strip");
                        }
                        // 3.
                        if !chips.is_empty() {
                            assert!(
                                chips.iter().any(|seg| seg.kind == TabHit::Select(active)),
                                "{where_}: the selected tab is not on the strip: {chips:?}"
                            );
                        }
                        let seated: Vec<usize> = chips
                            .iter()
                            .filter_map(|seg| match seg.kind {
                                TabHit::Select(i) => Some(i),
                                _ => None,
                            })
                            .collect();
                        // The window is one contiguous run of tabs.
                        for pair in seated.windows(2) {
                            assert_eq!(
                                pair[1],
                                pair[0] + 1,
                                "{where_}: the window has a hole: {seated:?}"
                            );
                        }
                        // 4.
                        if seated.len() < tabs && seated.len() > STRIP_MIN_SEATS {
                            for seg in &chips {
                                assert!(
                                    seg.end_col - seg.start_col >= STRIP_SEATS_FLOOR,
                                    "{where_}: a windowed strip seated a chip below \
                                     the floor: {chips:?}"
                                );
                            }
                        }
                        // 5 + 6.
                        let marks = strip_overflow(&segs, tabs);
                        for (col, _) in marks.columns() {
                            assert!(col < cols, "{where_}: mark off the strip");
                            assert_eq!(
                                hit_test(&segs, col),
                                None,
                                "{where_}: the mark at {col} sits on a hit target"
                            );
                        }
                        if let Some(&first) = seated.first() {
                            assert_eq!(
                                marks.left.is_some(),
                                first > 0 && chips[0].start_col > 0,
                                "{where_}: left mark disagrees with the window"
                            );
                        }
                        if let Some(&last) = seated.last() {
                            let end = chips[chips.len() - 1].end_col;
                            let free = !segs
                                .iter()
                                .any(|seg| end >= seg.start_col && end < seg.end_col);
                            assert_eq!(
                                marks.right.is_some(),
                                last + 1 < tabs && free,
                                "{where_}: right mark disagrees with the window"
                            );
                        }
                    }
                }
            }
        }
    }

    /// The selected chip names its tab at every width: never empty, never a bare
    /// elision mark, always text of its own title.
    #[test]
    fn the_selected_chip_names_its_tab_at_every_width() {
        for (set, titles) in realistic_strips() {
            for cols in (40..=300u16).step_by(1) {
                for active in [0, titles.len() / 2, titles.len() - 1] {
                    let segs = layout_segments(cols, titles.len(), active, false);
                    let labels = distinct_chip_labels(&segs, &titles, None, active, None);
                    let Some(label) = labels[active].as_deref() else {
                        // Only a strip with no chips at all may fail to seat it.
                        assert!(
                            !segs.iter().any(|s| matches!(s.kind, TabHit::Select(_))),
                            "{set} at {cols} cols: the selection lost its chip"
                        );
                        continue;
                    };
                    let where_ = format!("{set} at {cols} cols, active {active}");
                    assert!(!label.is_empty(), "{where_}: empty label");
                    assert_ne!(label, "…", "{where_}: the selection names nothing");
                    assert!(
                        says_its_own(label, &titles[active]),
                        "{where_}: the selection keeps nothing of its own title: {label:?}"
                    );
                }
            }
        }
    }

    /// The overflow mark is painted where the layout reserved it, on a column the
    /// hit map calls bare band.
    #[test]
    fn the_overflow_mark_paints_on_bare_band_and_only_there() {
        let titles: Vec<String> = (0..10)
            .map(|i| format!("job-{i} · Ready in aterm"))
            .collect();
        let theme = Theme::default();
        for cols in [60u16, 80, 100] {
            for active in [0, 4, 9] {
                let segs = layout_segments(cols, titles.len(), active, false);
                let marks = strip_overflow(&segs, titles.len());
                let mut row = vec![blank_cell(theme); usize::from(cols)];
                paint_strip(&mut row, &segs, &titles, None, active, theme);
                let where_ = format!("{cols} cols, active {active}");
                assert!(
                    marks.left.is_some() || marks.right.is_some(),
                    "{where_}: ten tabs cannot fit, so an edge must be marked"
                );
                for (col, glyph) in marks.columns() {
                    assert_eq!(
                        row[usize::from(col)].ch,
                        glyph,
                        "{where_}: no {glyph:?} painted at {col}"
                    );
                    assert_eq!(hit_test(&segs, col), None, "{where_}: {col} is a target");
                }
                // The two mark columns are the only ones carrying these glyphs.
                for (col, cell) in row.iter().enumerate() {
                    if cell.ch == STRIP_OVERFLOW_LEFT || cell.ch == STRIP_OVERFLOW_RIGHT {
                        assert!(
                            marks.columns().any(|(mark, _)| usize::from(mark) == col),
                            "{where_}: a stray edge mark at column {col}"
                        );
                    }
                }
            }
        }
    }

    /// More tabs than the band can hold at a cell apiece still lay out inside the
    /// strip without panicking; `cols == 0` yields no segments.
    #[test]
    fn extreme_tab_counts_stay_inside_the_band() {
        assert!(layout_segments(0, 3, 0, false).is_empty());
        for (cols, count, active) in [(130u16, 150usize, 0usize), (130, 150, 149), (12, 40, 39)] {
            let segs = layout_segments(cols, count, active, false);
            for seg in &segs {
                assert!(seg.end_col <= cols, "{cols} cols / {count} tabs");
                assert!(seg.start_col < seg.end_col);
            }
            for pair in segs.windows(2) {
                assert!(pair[0].end_col <= pair[1].start_col);
            }
        }
    }

    /// `active_tab_color` recolours only the selected tab, never the `+`.
    #[cfg(not(target_os = "macos"))]
    #[test]
    fn the_new_tab_button_ignores_the_active_tab_override() {
        let theme = Theme::default();
        let colors = strip_colors(theme);
        let plus = strip_cell('+', &colors, StripRole::NewTab);
        let picked = [200, 30, 90];
        let overridden = strip_colors_with_active(theme, Some(picked));
        assert_eq!(overridden.active_bg, picked, "the chosen tab colour lands");
        assert_eq!(
            strip_cell('+', &overridden, StripRole::NewTab).bg,
            plus.bg,
            "and stops at the tab: the `+` keeps the theme's own surface"
        );
    }

    /// `strip_char` blanks only controls; CJK, emoji and plane-15 Nerd Font icons
    /// pass through, with [`strip_char_cells`] carrying their display width.
    #[test]
    fn strip_char_sanitizes() {
        assert_eq!(strip_char('a'), 'a');
        assert_eq!(strip_char('\t'), ' ');
        assert_eq!(strip_char('世'), '世'); // wide CJK stays itself…
        assert_eq!(strip_char_cells('世'), 2); // …and costs its two cells
        assert_eq!(strip_char('\u{1F680}'), '\u{1F680}'); // 🚀 non-BMP too
        assert_eq!(strip_char_cells('\u{1F680}'), 2);
        assert_eq!(strip_char('\u{2705}'), '\u{2705}'); // ✅ BMP-wide symbol
        assert_eq!(strip_char_cells('\u{2705}'), 2);
        assert_eq!(strip_char('\u{F0154}'), '\u{F0154}'); // Nerd Font plane-15
        assert_eq!(strip_char_cells('\u{F0154}'), 1);
        // Zero-width chars keep a cell (the caret's char-index ↔ column invariant).
        assert_eq!(strip_char_cells('\u{FE0F}'), 1);
        assert_eq!(
            strip_display_cells("日本語"),
            6,
            "display width, not char count"
        );
    }

    /// Every built-in theme keeps the strip's text above its 3.0:1 legibility
    /// floor ([`STRIP_LEGIBILITY_FLOOR`], deliberately below AA): active-tab text on
    /// the raised card, the `+`, inactive labels on the band, the hover wash and the
    /// quiet chip, on dark and light themes alike; and its accent marks (the active
    /// underline, the update rule, the status marks) clear the 3:1 non-text floor on
    /// the card and the band, derived or authored, and the seam clears
    /// [`aterm_messages::tokens::SEAM_FLOOR`] on the band. The text floors hold under
    /// each forced chrome side too ([`crate::native_appearance::forced_chrome_theme`],
    /// Linux `window_theme = light|dark` against the terminal theme), whose fg/bg are
    /// authored constants.
    #[test]
    fn strip_inks_clear_the_legibility_floor() {
        use aterm_types::Rgb;
        let rgb = |c: [u8; 3]| Rgb::new(c[0], c[1], c[2]);
        let rgb_of = crate::settings::u32_rgb;
        // The raw-theme assertions below hold only while no OS palette forces
        // chrome; fail clearly if one leaked onto this worker. The High-Contrast
        // twin is `strip_contrast_holds_under_forced_high_contrast_palettes`.
        assert_eq!(
            crate::chrome_band::forced_chrome(),
            None,
            "the theme-derived strip is only defined with no OS-forced palette"
        );
        for name in aterm_types::scheme::builtin_names() {
            let s = aterm_types::scheme::builtin(name).expect("builtin exists");
            let terminal = Theme::from(&s);
            for (side, theme) in [
                ("terminal", terminal),
                (
                    "forced light",
                    crate::native_appearance::forced_chrome_theme(terminal, false),
                ),
                (
                    "forced dark",
                    crate::native_appearance::forced_chrome_theme(terminal, true),
                ),
            ] {
                let at = format!("{name} ({side})");
                let raw = side == "terminal";
                if !raw {
                    assert_eq!(
                        theme_is_dark(theme.bg),
                        side == "forced dark",
                        "{at}: the forced side classifies as the side it forces"
                    );
                }
                let c = strip_colors(theme);
                // Each pair is the ink and the surface `strip_cell` puts together.
                let floor = |what: &str, ink: [u8; 3], surface: [u8; 3]| {
                    let ratio = rgb(ink).contrast(rgb(surface));
                    assert!(ratio >= 3.0, "{at}: {what} contrast {ratio:.2} < 3.0:1");
                };
                floor("active-tab text", c.active_fg, c.active_bg);
                floor("inactive-tab label", c.inactive_fg, c.band_bg);
                if STRIP_IS_CHROME_BAND {
                    // The hover wash is a step toward the ink, so it is the pair
                    // likeliest to slip under the floor on a muted scheme.
                    floor("hovered-tab label", c.hover_fg, c.hover_bg);
                }
                if STRIP_CHIP_CARDS {
                    floor("quiet-chip label", c.chip_fg, c.chip_bg);
                    floor("'+' affordance", c.chip_button_fg, c.chip_bg);
                }
                // The active card must be a DISTINCT surface from the body, or the
                // focused tab vanishes into the strip.
                assert_ne!(c.active_bg, c.band_bg, "{at}: active card is the body");
                // The inactive dim never out-contrasts full strength on the same
                // surface: the floor is one-way and could drag a dim past its own
                // ink; only `strip_colors`' ceiling prevents it. Closest bundled
                // case: Solarized Light on a band, dim 3.50:1 against 3.69:1.
                let inactive = rgb(c.inactive_fg).contrast(rgb(c.band_bg));
                let full = rgb(c.fg).contrast(rgb(c.band_bg));
                assert!(
                    inactive <= full + 1e-9,
                    "{at}: the dim ({inactive:.2}:1) was floored past what it dims \
                     ({full:.2}:1)"
                );
                if !raw {
                    continue;
                }
                // The accent marks: the underline and the update rule on the raised
                // card, the status marks on the band.
                floor("update rule on its card", c.update_rule, c.update_bg);
                // The accent clears 3:1 on the card and the band and the seam clears
                // the seam floor on the band, derived or authored; an authored
                // accent (here a hostile one, the background itself) is still
                // floored on both its grounds. NEGATIVE CONTROL: the authored
                // accent is not the derived one.
                let hostile = rgb_of(theme.bg);
                let authored = strip_colors(ChromePalette {
                    authored: aterm_messages::tokens::AuthoredChrome {
                        accent: Some([0xff, 0x00, 0xaa]),
                        border: Some(hostile),
                        ..aterm_messages::tokens::AuthoredChrome::NONE
                    },
                    ..ChromePalette::from(theme)
                });
                let at_bg = strip_colors(ChromePalette {
                    authored: aterm_messages::tokens::AuthoredChrome {
                        accent: Some(hostile),
                        ..aterm_messages::tokens::AuthoredChrome::NONE
                    },
                    ..ChromePalette::from(theme)
                });
                assert_ne!(authored.accent, c.accent, "{at}: CONTROL");
                for (what, colors) in [("derived", &c), ("authored", &authored), ("at bg", &at_bg)]
                {
                    // The accent on the selected surface (the raised card, or on
                    // Windows the terminal's ground), the band and a hovered tab;
                    // the update rule on its card.
                    for (mark, on) in [
                        (colors.accent, colors.active_bg),
                        (colors.accent, colors.band_bg),
                        (colors.accent, colors.hover_bg),
                        (colors.update_rule, colors.update_bg),
                    ] {
                        let ratio = rgb(mark).contrast(rgb(on));
                        assert!(ratio >= 3.0, "{at}: {what} accent {ratio:.2} < 3.0:1");
                    }
                    if let Some(seam) = colors.seam {
                        let ratio = rgb(seam).contrast(rgb(colors.band_bg));
                        assert!(
                            ratio >= aterm_messages::tokens::SEAM_FLOOR,
                            "{at}: {what} seam {ratio:.2}"
                        );
                    }
                }
                if !STRIP_CHIP_CARDS {
                    let (fg, bg) = if STRIP_IS_CHROME_BAND {
                        (c.raise_fg, c.raise_bg)
                    } else {
                        (c.fg, c.band_bg)
                    };
                    floor("'+' affordance", fg, bg);
                }
                if STRIP_IS_CHROME_BAND {
                    // The `+` under the pointer, and the hover is a REAL surface
                    // change that never impersonates the quiet or SELECTED card.
                    floor("hovered '+'", c.button_hover_fg, c.hover_bg);
                    assert_ne!(c.hover_bg, c.chip_bg, "{at}: hover is the quiet chip");
                    assert_ne!(c.hover_bg, c.active_bg, "{at}: hover is the selection");
                }
                // The band's own full-strength ink is never floored on a bundled
                // scheme; needing it would mean the band itself is too heavy.
                let fg = [
                    ((theme.fg >> 16) & 0xff) as u8,
                    ((theme.fg >> 8) & 0xff) as u8,
                    (theme.fg & 0xff) as u8,
                ];
                assert_eq!(c.fg, fg, "{at}: the BAND is too heavy for its own ink");
            }
        }
    }

    /// Catppuccin Latte's cursor (a deepened rosewater) clears 3:1 on its background
    /// but not on the raised card, which steps toward it, so the strip's accent (the
    /// active underline and the update rule) is floored rather than drawn raw. The
    /// raw cursor's miss on the card is the negative control.
    #[test]
    fn latte_strip_accent_is_floored_against_the_card() {
        let s = aterm_types::scheme::builtin("Catppuccin Latte").expect("builtin exists");
        let tp = s.to_theme_parts();
        let theme = Theme {
            fg: tp.fg,
            bg: tp.bg,
            cursor: tp.cursor,
            selection: tp.selection,
        };
        let c = strip_colors(theme);
        let raw = [
            ((tp.cursor >> 16) & 0xff) as u8,
            ((tp.cursor >> 8) & 0xff) as u8,
            (tp.cursor & 0xff) as u8,
        ];
        let contrast = crate::chrome_band::contrast;
        // The surfaces the accent marks sit on: the selected one (the raised
        // card, or on Windows the terminal's ground, `SELECTED_TAB_IS_GROUND`),
        // the band and a hovered tab.
        let surfaces = [c.active_bg, c.band_bg, c.hover_bg];
        if !SELECTED_TAB_IS_GROUND {
            // The raised card steps toward the ink, where the cursor fades.
            assert!(
                contrast(raw, c.raise_bg) < 3.0,
                "negative control: Latte's raw cursor already clears 3:1 on the card"
            );
        }
        assert!(surfaces.iter().all(|on| contrast(c.accent, *on) >= 3.0));
        assert!(contrast(c.update_rule, c.update_bg) >= 3.0);
    }

    /// The High-Contrast twin of [`strip_inks_clear_the_legibility_floor`]: under every stock
    /// HC palette the strip's four ink/surface pairs stay above the UI-text floor and
    /// the selected chip is a distinct surface, with a hostile theme underneath to
    /// prove no theme byte leaks through. Contrast assertions only: the sibling's
    /// raw-ink checks are about aterm's own tuning, which the OS palette replaces.
    #[test]
    fn strip_contrast_holds_under_forced_high_contrast_palettes() {
        use aterm_types::Rgb;
        let rgb = |c: [u8; 3]| Rgb::new(c[0], c[1], c[2]);
        // Every channel identical, so any theme-derived blend collapses to one
        // colour and a leak shows as a 1.0:1 ratio.
        let hostile = Theme {
            fg: 0x0080_8080,
            bg: 0x0080_8080,
            cursor: 0x0080_8080,
            selection: 0x0080_8080,
        };
        for (name, palette) in crate::chrome_band::hc_fixtures::STOCK {
            crate::chrome_band::hc_fixtures::with_forced(palette, || {
                let c = strip_colors(hostile);
                // The trailing version label's pairs, as its painter resolves
                // them ([`version_label_material`]): its card, or the bare band
                // it sits on at rest.
                let version = |attention: bool, hovered: bool| {
                    let (card, ink) = version_label_material(&c, attention, hovered);
                    (ink, card.unwrap_or(c.band_bg))
                };
                let (rest_ink, rest_on) = version(false, false);
                let (hover_ink, hover_on) = version(false, true);
                let (cta_ink, cta_on) = version(true, false);
                for (role, ink, on) in [
                    ("active-tab label", c.active_fg, c.active_bg),
                    ("'+' affordance", c.raise_fg, c.raise_bg),
                    ("inactive label", c.inactive_fg, c.band_bg),
                    ("hovered label", c.hover_fg, c.hover_bg),
                    ("'↻' update CTA", c.update_fg, c.update_bg),
                    // The trailing version label's three surfaces (Windows):
                    // bare band at rest, the hover wash, the update CTA.
                    ("version label", rest_ink, rest_on),
                    ("hovered version label", hover_ink, hover_on),
                    ("version label under attention", cta_ink, cta_on),
                    // The CTA's underline is a rule on its own chip: HIGHLIGHT drawn
                    // on a HIGHLIGHT background is not a rule, it is nothing.
                    ("'↻' update rule", c.update_rule, c.update_bg),
                ] {
                    let ratio = rgb(ink).contrast(rgb(on));
                    assert!(
                        ratio >= 3.0,
                        "{name}: {role} contrast {ratio:.2} < 3.0:1 under High Contrast"
                    );
                }
                assert_ne!(
                    c.active_bg, c.band_bg,
                    "{name}: the selected chip must stay a distinct surface"
                );
                // The band, the seam and the chip come from the OS, not the theme.
                assert_eq!(c.band_bg, palette.btn_face, "{name}: band is BTNFACE");
                assert_eq!(c.active_bg, palette.highlight, "{name}: chip is HIGHLIGHT");
                assert_eq!(
                    c.seam,
                    Some(palette.window_text),
                    "{name}: the seam is a WINDOWTEXT border, not a blend"
                );
                // The OS selection colour means selected: the `+` must not wear it.
                assert_ne!(
                    c.raise_bg, palette.highlight,
                    "{name}: the '+' must not borrow the selection surface"
                );
                assert_ne!(
                    c.hover_bg, palette.highlight,
                    "{name}: hover must not impersonate the selection"
                );
                // Nor may the `↻` update CTA: it would copy the selected tab and its
                // HIGHLIGHT underline would vanish into its own background.
                assert_ne!(
                    c.update_bg, palette.highlight,
                    "{name}: the update CTA must not borrow the selection surface"
                );
                assert_ne!(
                    c.update_bg, c.active_bg,
                    "{name}: the update CTA must not be a copy of the selected tab"
                );
                // …and the user's `active_tab_color` does not overrule it.
                let overridden = strip_colors_with_active(hostile, Some([0xFF, 0x00, 0xFF]));
                assert_eq!(
                    overridden.active_bg, palette.highlight,
                    "{name}: active_tab_color must not override an HC palette"
                );
            });
        }
    }

    /// THE AUTOMATIC SELECTED-TAB COLOUR IS WHAT THE STRIP PAINTS: for every
    /// built-in theme it is the selected tab the strip draws while no
    /// `active_tab_color` key is set, over each floor; a colour the theme
    /// authors is that colour, under the native pill too; under every stock
    /// High Contrast scheme it is the highlight drawn there; over macOS's
    /// toolbar it is the system pill at its alpha over the band. Negative
    /// control: off Windows it is never the band under the card.
    #[test]
    fn the_automatic_tab_color_is_the_card_the_strip_paints() {
        let selection = [0x0A, 0x64, 0xD8];
        for name in aterm_types::scheme::builtin_names() {
            let theme = Theme::from(&aterm_types::scheme::builtin(name).expect("builtin"));
            let header = ChromePalette::from(theme);
            for floor in [StripFloor::Ground, StripFloor::Surface([0xEA, 0xEB, 0xEB])] {
                let painted = strip_colors_for_on(header, floor).active_bg;
                assert_eq!(
                    automatic_active_tab_color(header, floor, None),
                    painted,
                    "{name} {floor:?}"
                );
            }
            if !SELECTED_TAB_IS_GROUND {
                assert_ne!(
                    automatic_active_tab_color(header, StripFloor::Ground, None),
                    strip_colors(theme).band_bg,
                    "{name}: the card, not the band under it"
                );
            }
            assert_eq!(
                automatic_active_tab_color(header, StripFloor::Ground, Some(selection)),
                crate::chrome_band::mix3(
                    strip_colors(theme).band_bg,
                    selection,
                    NATIVE_PILL_AUTO_ALPHA
                ),
                "{name}: the native pill over the band"
            );
            let authored = ChromePalette {
                authored: aterm_messages::tokens::AuthoredChrome {
                    active_tab: Some([0x33, 0x66, 0xCC]),
                    ..aterm_messages::tokens::AuthoredChrome::NONE
                },
                ..header
            };
            assert_eq!(
                automatic_active_tab_color(authored, StripFloor::Ground, Some(selection)),
                [0x33, 0x66, 0xCC],
                "{name}: the theme's own selected-tab colour"
            );
        }
        for (name, palette) in crate::chrome_band::hc_fixtures::STOCK {
            crate::chrome_band::hc_fixtures::with_forced(palette, || {
                let header = ChromePalette::from(Theme::default());
                assert_eq!(
                    automatic_active_tab_color(header, StripFloor::Ground, Some(selection)),
                    palette.highlight,
                    "{name}"
                );
                assert_eq!(
                    automatic_active_tab_color(header, StripFloor::Ground, None),
                    strip_colors_for(header).active_bg,
                    "{name}"
                );
            });
        }
    }

    /// `strip_colors` is appearance-aware. Off Windows the active card steps
    /// toward the fg: brighter than the body on a dark theme, darker on a light
    /// one. On Windows the selected tab is the terminal's own ground, so it is
    /// darker than the band on a dark theme and lighter on a light one, the
    /// update CTA rises toward white off a light band, and the hover wash steps
    /// toward the ink on every band ([`LIGHT_TAB_HOVER_T`]), so a hovered quiet
    /// tab is never a second selected one. Either way it is a distinct surface.
    #[test]
    fn strip_colors_raise_direction_follows_appearance() {
        let luma = |c: [u8; 3]| {
            0.299 * f32::from(c[0]) + 0.587 * f32::from(c[1]) + 0.114 * f32::from(c[2])
        };
        let parts = |name: &str| {
            let s = aterm_types::scheme::builtin(name).expect("builtin exists");
            strip_colors(Theme::from(&s))
        };
        let dark = parts("Dracula");
        let light = parts("Solarized Light");
        if SELECTED_TAB_IS_GROUND {
            assert_eq!(dark.active_bg, dark.foot, "the selected tab is the ground");
            assert!(
                luma(dark.active_bg) < luma(dark.band_bg),
                "dark: below the band"
            );
            assert!(
                luma(light.active_bg) > luma(light.band_bg),
                "light: above the band"
            );
            assert!(
                luma(light.raise_bg) > luma(light.band_bg),
                "light update CTA rises toward white"
            );
            assert!(
                luma(dark.hover_bg) > luma(dark.band_bg),
                "dark hover rises toward fg"
            );
            // The light hover is the opposite step from the selected tab: below
            // the band, a visible step, never toward the selection. NEGATIVE
            // CONTROL: the 2026-10-18 strip rose it toward white, #eeeeef on
            // Default Light's #e2e3e5 band beside the selected #f6f7f9.
            for name in aterm_types::scheme::builtin_names() {
                let c = parts(name);
                if !bg_is_light(c.foot) {
                    continue;
                }
                assert!(
                    luma(c.hover_bg) < luma(c.band_bg) && luma(c.active_bg) > luma(c.band_bg),
                    "{name}: hover {:?} must step below the band {:?}, the selected tab {:?} above",
                    c.hover_bg,
                    c.band_bg,
                    c.active_bg
                );
                assert!(
                    crate::chrome_band::contrast(c.hover_bg, c.band_bg) >= 1.08,
                    "{name}: the hover wash {:?} hardly leaves the band {:?}",
                    c.hover_bg,
                    c.band_bg
                );
            }
        } else {
            // Dark builtin: the active card is brighter than the body (a raised step).
            assert!(
                luma(dark.active_bg) > luma(dark.band_bg),
                "dark theme active card should be brighter than the body"
            );
            // Light builtin: the active card is darker than the body (a subtle card).
            assert!(
                luma(light.active_bg) < luma(light.band_bg),
                "light theme active card should be darker than the body"
            );
        }
    }

    /// EVERY SELECTED TAB OF A LIGHT BAND IS ONE SHAPE ([`selected_tab_rim`]):
    /// over the grid and over the Settings page alike, every light built-in
    /// theme's selected Windows tab wears the seam's outline, as WinUI's
    /// selected `TabViewItem` carries its card stroke whatever it holds; a
    /// dark built-in's keeps the [`TAB_RIM_BELOW`] rule, so Default's tabs
    /// stay bare. NEGATIVE CONTROL: by that contrast rule alone Default
    /// Light's terminal tab (1.20:1) went bare while its Settings tab (1.08:1)
    /// wore the outline (the 2026-10-18 judge's
    /// `J4F-4-light-selected-tab-two-idioms`).
    #[test]
    fn every_selected_tab_of_a_light_band_wears_one_outline() {
        use crate::chrome_band::contrast;
        if !SELECTED_TAB_IS_GROUND {
            return;
        }
        let theme_of =
            |name: &str| Theme::from(&aterm_types::scheme::builtin(name).expect("builtin"));
        let mut light = 0;
        for name in aterm_types::scheme::builtin_names() {
            let theme = theme_of(name);
            let page = crate::settings::Roles::from_theme(theme).surface;
            for floor in [StripFloor::Ground, StripFloor::Surface(page)] {
                let c = strip_colors_for_on(theme, floor);
                let ratio = contrast(c.active_bg, c.band_bg);
                if bg_is_light(c.band_bg) {
                    assert!(c.seam.is_some(), "{name}: a band has a seam");
                    assert_eq!(c.active_rim, c.seam, "{name} {floor:?} {ratio:.2}:1");
                } else {
                    assert_eq!(
                        c.active_rim.is_some(),
                        ratio < TAB_RIM_BELOW,
                        "{name} {floor:?} {ratio:.2}:1: a dark band keeps its rule"
                    );
                }
            }
            light += usize::from(bg_is_light(strip_colors(theme).band_bg));
        }
        assert!(light >= 2, "fixture: light built-ins ({light})");
        let grid = strip_colors_for_on(theme_of("Default Light"), StripFloor::Ground);
        assert!(
            contrast(grid.active_bg, grid.band_bg) >= TAB_RIM_BELOW,
            "CONTROL: the contrast rule alone leaves Default Light's terminal tab bare"
        );
        assert!(
            strip_colors(Theme::default()).active_rim.is_none(),
            "Default's terminal tab stays bare"
        );
    }

    // The strip under a stationary pointer.

    /// Widths a real window is dragged through and tab counts it holds. Rules below
    /// are asserted over the whole grid, not only the width they were designed at.
    const STRIP_GRID_COLS: [u16; 9] = [40, 60, 80, 100, 120, 140, 200, 240, 300];
    const STRIP_GRID_TABS: [usize; 4] = [1, 2, 8, 20];

    /// A tab wearing `busy` and nothing else, as `claude` shells mid-command do.
    fn busy_tab() -> TabStripMetadata {
        TabStripMetadata {
            busy: true,
            ..PLAIN_TAB
        }
    }

    /// The four strips rules below are asserted over: identical and distinct titles,
    /// each idle and mid-command (the busy/idle difference changes title width).
    fn strip_cases(tabs: usize) -> Vec<(&'static str, Vec<String>, Vec<TabStripMetadata>)> {
        let same: Vec<String> = (0..tabs).map(|_| "claude".to_string()).collect();
        let distinct: Vec<String> = (0..tabs)
            .map(|i| format!("job-{i} · Ready in aterm"))
            .collect();
        let idle = vec![PLAIN_TAB; tabs];
        let busy = vec![busy_tab(); tabs];
        vec![
            ("same/idle", same.clone(), idle.clone()),
            ("same/busy", same, busy.clone()),
            ("distinct/idle", distinct.clone(), idle),
            ("distinct/busy", distinct, busy),
        ]
    }

    /// Selecting a chip the strip already shows produces the identical strip (same
    /// chips, columns, close columns, marks) over the whole grid, so a click never
    /// moves targets under a still pointer ([`strip_window`]). Implies the
    /// click/relayout/re-hit-test drift check.
    #[test]
    fn selecting_a_seated_chip_re_lays_out_the_identical_strip() {
        let mut compared = 0usize;
        for &tabs in &STRIP_GRID_TABS {
            for (case, titles, metadata) in strip_cases(tabs) {
                for &cols in &STRIP_GRID_COLS {
                    for active in 0..tabs {
                        let before = layout_segments_with_metadata(
                            cols,
                            titles.len(),
                            &metadata,
                            active,
                            false,
                        );
                        for seg in &before {
                            let TabHit::Select(seated) = seg.kind else {
                                continue;
                            };
                            compared += 1;
                            assert_eq!(
                                layout_segments_with_metadata(
                                    cols,
                                    titles.len(),
                                    &metadata,
                                    seated,
                                    false
                                ),
                                before,
                                "{case} at {cols} cols, {tabs} tabs: selecting seated tab \
                                 {seated} (from {active}) re-anchored the strip"
                            );
                        }
                    }
                }
            }
        }
        assert!(compared > 2_000, "vacuous: {compared} selections compared");
    }

    /// [`strip_window`] is a partition, over every seat and tab count: every page
    /// contains its selection, no page is wider than the seats, pages are as few and
    /// as even as possible, and the page computed for any tab it seats is the same
    /// page (closure, the zero-drift property).
    #[test]
    fn the_strip_window_is_an_even_partition_of_the_tab_list() {
        for tab_count in 1..=24usize {
            for seats in 1..=tab_count {
                let windows: Vec<(usize, usize)> = (0..tab_count)
                    .map(|active| strip_window(active, seats, tab_count))
                    .collect();
                let mut sizes: Vec<usize> = Vec::new();
                let mut covered = vec![0usize; tab_count];
                for (active, &(first, len)) in windows.iter().enumerate() {
                    let where_ = format!("{tab_count} tabs, {seats} seats, active {active}");
                    assert!(len > 0 && len <= seats, "{where_}: {len} chips seated");
                    assert!(
                        first <= active && active < first + len,
                        "{where_}: the page {first}..{} does not seat it",
                        first + len
                    );
                    assert!(
                        first + len <= tab_count,
                        "{where_}: the page runs past the end"
                    );
                    // Closure: every tab this page seats computes this page.
                    for seated in first..first + len {
                        assert_eq!(
                            strip_window(seated, seats, tab_count),
                            (first, len),
                            "{where_}: seated tab {seated} would turn the page"
                        );
                    }
                    if windows[..active].iter().all(|&(f, _)| f != first) {
                        sizes.push(len);
                        for seat in covered.iter_mut().skip(first).take(len) {
                            *seat += 1;
                        }
                    }
                }
                assert!(
                    covered.iter().all(|&n| n == 1),
                    "{tab_count} tabs, {seats} seats: the pages are not a partition ({covered:?})"
                );
                // As few pages as the seats allow, as even as that count allows (no
                // runt page as `(active / seats) * seats` paging would leave).
                assert_eq!(
                    sizes.len(),
                    tab_count.div_ceil(seats),
                    "{tab_count} tabs, {seats} seats: not the fewest pages ({sizes:?})"
                );
                let (lo, hi) = (
                    *sizes.iter().min().expect("a page"),
                    *sizes.iter().max().expect("a page"),
                );
                assert!(
                    hi - lo <= 1,
                    "{tab_count} tabs, {seats} seats: uneven pages {sizes:?}"
                );
                assert!(
                    sizes.windows(2).all(|pair| pair[0] >= pair[1]),
                    "{tab_count} tabs, {seats} seats: the longer pages come first ({sizes:?})"
                );
            }
        }
    }

    // The floor is a title width.

    /// The floor is measured on the title, not the chip: a busy chip spends three
    /// more cells on its status canvas than an idle one. Clauses: the floor read off
    /// a plain chip matches [`PREFERRED_MIN_TAB_COLS`]'s doc; a plain chip's seat is
    /// that constant exactly; every chrome's seat is the minimal width that keeps
    /// the floor.
    #[test]
    fn the_legibility_floor_is_a_title_width_and_every_seat_is_minimal() {
        let floor = preferred_min_title_cols();
        assert_eq!(
            floor,
            chip_title_cols(PREFERRED_MIN_TAB_COLS, PLAIN_TAB),
            "the floor IS what a plain chip keeps at the constant"
        );
        assert_eq!(
            chip_seat_cols(PLAIN_TAB),
            PREFERRED_MIN_TAB_COLS,
            "an idle strip's floor is the shipped one, to the column"
        );

        // Every chrome a chip can wear, including the ones no test names.
        let mut wider = 0usize;
        for icon in [None, Some(TabIconKind::Settings)] {
            for busy in [false, true] {
                for closable in [false, true] {
                    let chrome = TabStripMetadata {
                        icon,
                        busy,
                        closable,
                        ..PLAIN_TAB
                    };
                    let seat = chip_seat_cols(chrome);
                    assert!(
                        chip_title_cols(seat, chrome) >= floor,
                        "{chrome:?}: a {seat}-column seat keeps only {} title cells",
                        chip_title_cols(seat, chrome)
                    );
                    assert!(
                        seat == PREFERRED_MIN_TAB_COLS || chip_title_cols(seat - 1, chrome) < floor,
                        "{chrome:?}: {seat} columns is not the NARROWEST seat that \
                         keeps the floor"
                    );
                    assert!(
                        seat < SEAT_COLS_CEILING
                            && chip_title_cols(SEAT_COLS_CEILING, chrome) >= floor,
                        "{chrome:?}: the seat scan's bound is binding — \
                         [`SEAT_COLS_CEILING`] has to be past the widest chrome, \
                         or a seat is whatever the bound happened to be"
                    );
                    wider += usize::from(seat > PREFERRED_MIN_TAB_COLS);
                }
            }
        }
        assert!(
            wider > 0,
            "vacuous: no chrome costs a chip anything, so the gate cannot be wrong"
        );
    }

    /// No seated chip is ever a bare ordinal, over the whole grid and all four
    /// strips: the layout seats fewer chips instead. The bare digits remain
    /// [`ordinal_chip_label`]'s own floor. One tab on a [`SOLO_TITLE_BAND`]
    /// platform is not a chip; see the solo arm.
    #[test]
    fn no_width_seats_a_chip_that_can_only_say_a_number() {
        let mut seats = 0usize;
        for &tabs in &STRIP_GRID_TABS {
            for (case, titles, metadata) in strip_cases(tabs) {
                for &cols in &STRIP_GRID_COLS {
                    for active in [0usize, tabs / 2, tabs - 1] {
                        let segments = layout_segments_with_metadata(
                            cols,
                            titles.len(),
                            &metadata,
                            active,
                            false,
                        );
                        let labels =
                            distinct_chip_labels(&segments, &titles, Some(&metadata), active, None);
                        for seg in &segments {
                            let TabHit::Select(i) = seg.kind else {
                                continue;
                            };
                            // A solo band is the window title, composed by
                            // [`solo_band_text`]; this pass returns `None` for it.
                            // Assert that contract and keep walking.
                            if seg.solo {
                                assert!(
                                    labels[i].is_none(),
                                    "{case} at {cols} cols: the label pass claimed the solo \
                                     title band ({:?})",
                                    labels[i]
                                );
                                continue;
                            }
                            let label = labels[i].as_deref().expect("a seated chip is labelled");
                            seats += 1;
                            assert!(
                                !label.chars().all(|c| c.is_ascii_digit()),
                                "{case} at {cols} cols, {tabs} tabs, active {active}: chip {i} \
                                 is the bare ordinal {label:?} — the photographed defect"
                            );
                            assert!(
                                !label.is_empty() && label != "…",
                                "{case} at {cols} cols: chip {i} says nothing ({label:?})"
                            );
                        }
                    }
                }
            }
        }
        assert!(seats > 500, "vacuous: only {seats} chips were labelled");
    }

    /// The version label widths the layout tests below reason in: symbolic stand-ins
    /// for a measured slot, because the rule under test is how the strip yields to the
    /// label, not one face's advances. The measured widths have their own test
    /// (`pixel_band::tests::version_slot_cols_cover_the_painted_width`).
    const SLOT: VersionSlotCols = VersionSlotCols {
        full: 22,
        short: 13,
        attention: false,
    };

    /// The laid-out version label: `(rung, start_col, end_col)`.
    fn version_of(segments: &[TabSegment]) -> Option<(crate::menu::VersionRung, u16, u16)> {
        segments.iter().find_map(|seg| match seg.kind {
            TabHit::Version { rung, .. } => Some((rung, seg.start_col, seg.end_col)),
            _ => None,
        })
    }

    fn without_version(segments: &[TabSegment]) -> Vec<TabSegment> {
        segments
            .iter()
            .copied()
            .filter(|seg| !matches!(seg.kind, TabHit::Version { .. }))
            .collect()
    }

    fn slot_width(rung: crate::menu::VersionRung, slot: VersionSlotCols) -> u16 {
        match rung {
            crate::menu::VersionRung::Full => slot.full,
            crate::menu::VersionRung::Short => slot.short,
        }
    }

    /// Every strip the sweeps below lay out: `(tabs, metadata name, metadata,
    /// show_update, active, cols)` over cols 0..=220, tabs 0..=14, the `↻`
    /// shown or not, the first and the last tab selected, plain and busy chips.
    fn version_sweep(mut each: impl FnMut(usize, &str, &[TabStripMetadata], bool, usize, u16)) {
        for tabs in 0..=14usize {
            for (name, chip) in [("plain", PLAIN_TAB), ("busy", busy_tab())] {
                let metadata = vec![chip; tabs];
                for show_update in [false, true] {
                    for active in [0, tabs.saturating_sub(1)] {
                        for cols in 0..=220u16 {
                            each(tabs, name, &metadata, show_update, active, cols);
                        }
                    }
                }
            }
        }
    }

    /// Everything left of the label is, to the column, the label-free layout of the
    /// `cols - w` columns left of it; with no label laid out, the label-free layout of
    /// all `cols`. [`VersionSlotCols::NONE`] is the label-free strip, and for plain
    /// chips the metadata-free entry's too.
    #[test]
    fn the_layout_left_of_the_slot_is_todays_layout_to_the_column() {
        let mut laid = 0usize;
        version_sweep(|tabs, name, metadata, show_update, active, cols| {
            let today = layout_segments_with_metadata(cols, tabs, metadata, active, show_update);
            let none = layout_segments_with_version(
                cols,
                tabs,
                metadata,
                active,
                show_update,
                VersionSlotCols::NONE,
            );
            assert_eq!(none, today, "{name}: NONE is today's strip");
            if name == "plain" {
                assert_eq!(
                    none,
                    layout_segments(cols, tabs, active, show_update),
                    "plain chips at {cols} cols, {tabs} tabs"
                );
            }
            let segs =
                layout_segments_with_version(cols, tabs, metadata, active, show_update, SLOT);
            match version_of(&segs) {
                Some((rung, start, end)) => {
                    laid += 1;
                    let w = slot_width(rung, SLOT);
                    assert_eq!((start, end), (cols - w, cols), "flush right at {cols}");
                    assert_eq!(
                        without_version(&segs),
                        layout_segments_with_metadata(
                            cols - w,
                            tabs,
                            metadata,
                            active,
                            show_update
                        ),
                        "{name}, {cols} cols, {tabs} tabs, update {show_update}, active {active}"
                    );
                    assert!(
                        matches!(segs.last().map(|s| s.kind), Some(TabHit::Version { .. })),
                        "the label is the strip's last segment"
                    );
                }
                None => assert_eq!(segs, today, "{name}: no label is today's strip"),
            }
        });
        assert!(laid > 2_000, "vacuous: only {laid} strips laid a label out");
    }

    /// The label yields before any chip does: wherever it is laid out beside two or
    /// more tabs, every chip still has at least its seat ([`strip_seat_cols`]), every
    /// tab is seated, and no edge is marked.
    #[test]
    fn the_slot_yields_before_any_chip_drops_below_its_seat() {
        let mut checked = 0usize;
        version_sweep(|tabs, name, metadata, show_update, active, cols| {
            let segs =
                layout_segments_with_version(cols, tabs, metadata, active, show_update, SLOT);
            if tabs < 2 || version_of(&segs).is_none() {
                return;
            }
            checked += 1;
            let seat = strip_seat_cols(tabs, metadata);
            let chips: Vec<&TabSegment> = segs
                .iter()
                .filter(|s| matches!(s.kind, TabHit::Select(_)))
                .collect();
            assert_eq!(chips.len(), tabs, "{name} at {cols}: every tab seated");
            for chip in chips {
                assert!(
                    chip.end_col - chip.start_col >= seat,
                    "{name} at {cols} cols, {tabs} tabs: a chip of {} < seat {seat}",
                    chip.end_col - chip.start_col
                );
            }
            assert_eq!(
                strip_overflow(&segs, tabs),
                StripOverflow::NONE,
                "{name} at {cols}: no marked edge"
            );
        });
        assert!(checked > 1_000, "vacuous: only {checked} strips checked");
    }

    /// A lone chip never shrinks for the label: at every width it is exactly the chip
    /// it would be with no label, and a label beside it means the chip has its full
    /// [`LONE_CHIP_COLS`].
    #[cfg(not(target_os = "macos"))]
    #[test]
    fn a_lone_chip_never_shrinks_for_the_slot() {
        let chip = |segs: &[TabSegment]| {
            segs.iter()
                .find(|s| s.kind == TabHit::Select(0))
                .map(|s| (s.start_col, s.end_col, s.close_col))
        };
        let mut beside = 0usize;
        for show_update in [false, true] {
            for cols in 0..=220u16 {
                let plain = layout_segments_with_metadata(cols, 1, &[PLAIN_TAB], 0, show_update);
                let segs =
                    layout_segments_with_version(cols, 1, &[PLAIN_TAB], 0, show_update, SLOT);
                assert_eq!(
                    chip(&segs),
                    chip(&plain),
                    "{cols} cols, update {show_update}"
                );
                if version_of(&segs).is_some() {
                    beside += 1;
                    let (start, end, _) = chip(&segs).expect("the lone chip");
                    assert_eq!(end - start, LONE_CHIP_COLS, "{cols} cols");
                }
            }
        }
        assert!(beside > 100, "vacuous: {beside}");
    }

    /// The label steps down, then hides: full while it fits, the short rung when only
    /// that fits, and nothing once even that would cost a tab its seat (at 132
    /// columns 8/9/10 tabs, at 77 columns 4/5/6).
    #[test]
    fn the_slot_steps_down_a_rung_then_hides() {
        use crate::menu::VersionRung::{Full, Short};
        for (cols, tabs, want) in [
            (132, 8, Some(Full)),
            (132, 9, Some(Short)),
            (132, 10, None),
            (77, 4, Some(Full)),
            (77, 5, Some(Short)),
            (77, 6, None),
        ] {
            let metadata = vec![PLAIN_TAB; tabs];
            let segs = layout_segments_with_version(cols, tabs, &metadata, 0, false, SLOT);
            assert_eq!(
                version_of(&segs).map(|v| v.0),
                want,
                "{tabs} tabs at {cols} columns"
            );
        }
    }

    /// The cell lane's label (before the UI face is installed, and wherever the pixel
    /// band does not reach): right-aligned with one trailing pad cell; bare band at
    /// rest, the band's hover material under the pointer, the update CTA's surface
    /// under attention; the short rung when the cells cannot hold the full one, and no
    /// text below that.
    #[test]
    fn the_cell_lane_paints_the_version_label_right_aligned() {
        use crate::menu::{VersionRung, strip_version_label};
        let theme = Theme::default();
        let colors = strip_colors(theme);
        let metadata = [PLAIN_TAB; 2];
        let titles = vec!["one".to_string(), "two".to_string()];
        let paint = |segs: &[TabSegment], version_hovered: bool| {
            let mut row = vec![blank_cell(theme); 132];
            paint_strip_with_metadata(
                &mut row,
                segs,
                &titles,
                &metadata,
                StripPaint {
                    version_hovered,
                    caption_hovered: None,
                    ..StripPaint::default()
                },
                0,
                theme,
                None,
            );
            row
        };
        let cells = |label: &str| u16::try_from(strip_display_cells(label)).expect("short");
        for (attention, hovered, role) in [
            (false, false, StripRole::Inactive),
            (false, true, StripRole::Hover),
            (true, false, StripRole::Update),
        ] {
            let full = strip_version_label(VersionRung::Full, attention).expect("full");
            let slot = VersionSlotCols {
                full: cells(&full) + 4,
                short: 0,
                attention,
            };
            let segs = layout_segments_with_version(132, 2, &metadata, 0, false, slot);
            let (_, start, end) = version_of(&segs).expect("the label fits at 132 columns");
            let row = paint(&segs, hovered);
            let want = strip_cell(' ', &colors, role);
            for col in start..end {
                assert_eq!(
                    row[usize::from(col)].bg,
                    want.bg,
                    "attention {attention}, hovered {hovered}: column {col}"
                );
            }
            if hovered {
                // The band's one hover material (`strip_hover_bg_for_test`'s
                // answer, read directly: that helper exists only where a
                // chrome band does, and this test runs on every host).
                assert_eq!(want.bg, colors.hover_bg, "the band's hover material");
            }
            let text_end = usize::from(end) - 1;
            assert_eq!(row[text_end].ch, ' ', "one trailing pad cell");
            let text_start = text_end - usize::from(cells(&full));
            let painted: String = row[text_start..text_end].iter().map(|c| c.ch).collect();
            assert_eq!(painted, full, "right-aligned against the pad");
            assert_eq!(row[text_start].fg, want.fg, "the role's own ink");
            assert_eq!(row[text_start - 1].ch, ' ', "nothing left of the text");
        }
        // Fewer cells than the full label: the short rung; fewer still: no text.
        let short = strip_version_label(VersionRung::Short, false).expect("a dev short rung");
        let label_segment = |width: u16| TabSegment {
            start_col: 132 - width,
            end_col: 132,
            close_col: None,
            kind: TabHit::Version {
                rung: VersionRung::Full,
                attention: false,
            },
            solo: false,
        };
        let row = paint(&[label_segment(cells(&short) + 1)], false);
        let painted: String = row[131 - usize::from(cells(&short))..131]
            .iter()
            .map(|c| c.ch)
            .collect();
        assert_eq!(
            painted, short,
            "the short rung when the full one cannot fit"
        );
        let row = paint(&[label_segment(cells(&short))], false);
        assert!(
            row[usize::from(132 - cells(&short))..]
                .iter()
                .all(|c| c.ch == ' '),
            "no room for either rung: no text, never a cut label"
        );
    }

    /// The window buttons the tests lay out: all three, four 9 px cells each.
    fn caption_slot() -> CaptionSlot {
        CaptionSlot::measure(
            Some(crate::client_frame::ClientFrame {
                minimize: true,
                maximize: true,
                window_menu: true,
                maximized: false,
                edges: true,
                header: true,
                layout: crate::client_frame::ButtonLayout::FALLBACK,
            }),
            9,
            1.0,
        )
    }

    /// THE WINDOW BUTTONS SIT FLUSH RIGHT AND HIT AS THEMSELVES (LW.5), and
    /// everything left of them is today's strip of the columns they leave; no
    /// buttons ([`CaptionSlot::NONE`], every window the window system decorates)
    /// is today's strip, to the column.
    #[test]
    fn window_buttons_are_flush_right_and_the_tabs_yield_to_them() {
        let caption = caption_slot();
        assert_eq!(caption.cols(), 12);
        for tabs in [0usize, 1, 2, 5, 40] {
            let metadata = vec![PLAIN_TAB; tabs];
            let none = VersionSlotCols::NONE;
            let segs = layout_segments_with_caption(80, tabs, &metadata, 0, false, none, caption);
            let today = layout_segments_with_version(68, tabs, &metadata, 0, false, none);
            assert_eq!(segs[..segs.len() - 3], today[..], "{tabs} tabs");
            let buttons: Vec<_> = segs[segs.len() - 3..]
                .iter()
                .map(|s| (s.kind, s.start_col, s.end_col, s.close_col, s.solo))
                .collect();
            assert_eq!(
                buttons,
                [
                    (
                        TabHit::Caption(CaptionButton::Minimize),
                        68,
                        72,
                        None,
                        false
                    ),
                    (
                        TabHit::Caption(CaptionButton::Maximize),
                        72,
                        76,
                        None,
                        false
                    ),
                    (TabHit::Caption(CaptionButton::Close), 76, 80, None, false),
                ]
            );
            for col in 76..80 {
                assert_eq!(
                    hit_test(&segs, col),
                    Some(TabHit::Caption(CaptionButton::Close)),
                    "{tabs} tabs, column {col}"
                );
            }
            assert_eq!(
                layout_segments_with_caption(
                    80,
                    tabs,
                    &metadata,
                    0,
                    false,
                    none,
                    CaptionSlot::NONE
                ),
                layout_segments_with_version(80, tabs, &metadata, 0, false, none),
                "{tabs} tabs: no buttons is today's strip"
            );
        }
    }

    /// THE DESKTOP'S BUTTON LAYOUT PLACES THEM (sctk-adwaita's
    /// `button-layout`, [`crate::client_frame::ButtonLayout`]): buttons the
    /// layout puts at the left end sit flush left in its order, the strip moves
    /// right past them to the column, and the right end's sit flush right; the
    /// cell lane paints each mark where its segment is.
    #[test]
    fn a_left_end_layout_puts_buttons_flush_left_and_the_strip_moves_past_them() {
        let layout =
            crate::client_frame::ButtonLayout::parse("close,minimize:maximize").expect("a layout");
        let caption = CaptionSlot::measure(
            Some(crate::client_frame::ClientFrame {
                minimize: true,
                maximize: true,
                window_menu: true,
                maximized: false,
                edges: true,
                header: true,
                layout,
            }),
            9,
            1.0,
        );
        let none = VersionSlotCols::NONE;
        for tabs in [0usize, 1, 2, 5, 40] {
            let metadata = vec![PLAIN_TAB; tabs];
            let segs = layout_segments_with_caption(80, tabs, &metadata, 0, false, none, caption);
            let today = layout_segments_with_version(68, tabs, &metadata, 0, false, none);
            let (strip, buttons) = segs.split_at(segs.len() - 3);
            let moved: Vec<_> = today
                .iter()
                .map(|s| TabSegment {
                    start_col: s.start_col + 8,
                    end_col: s.end_col + 8,
                    close_col: s.close_col.map(|c| c + 8),
                    ..*s
                })
                .collect();
            assert_eq!(strip, &moved[..], "{tabs} tabs");
            let buttons: Vec<_> = buttons
                .iter()
                .map(|s| (s.kind, s.start_col, s.end_col))
                .collect();
            assert_eq!(
                buttons,
                [
                    (TabHit::Caption(CaptionButton::Close), 0, 4),
                    (TabHit::Caption(CaptionButton::Minimize), 4, 8),
                    (TabHit::Caption(CaptionButton::Maximize), 76, 80),
                ],
                "{tabs} tabs"
            );
            for col in 0..4 {
                assert_eq!(
                    hit_test(&segs, col),
                    Some(TabHit::Caption(CaptionButton::Close))
                );
            }
        }
        // NEGATIVE CONTROL: the fallback's buttons are all at the right end.
        let segs =
            layout_segments_with_caption(80, 1, &[PLAIN_TAB], 0, false, none, caption_slot());
        assert!(hit_test(&segs, 0) != Some(TabHit::Caption(CaptionButton::Close)));
        let theme = Theme::default();
        let segs = layout_segments_with_caption(80, 1, &[PLAIN_TAB], 0, false, none, caption);
        let mut row = vec![blank_cell(theme); 80];
        paint_strip_with_metadata(
            &mut row,
            &segs,
            &["one".to_string()],
            &[PLAIN_TAB],
            StripPaint::default(),
            0,
            theme,
            None,
        );
        assert_eq!(row[2].ch, '✕');
        assert_eq!(row[6].ch, '_');
        assert_eq!(row[78].ch, '□');
    }

    /// A NARROW HEADER keeps its close button: minimize goes first, then
    /// maximize, and the tabs keep [`CAPTION_STRIP_FLOOR`] cells.
    #[test]
    fn a_narrow_header_sheds_minimize_then_maximize() {
        let caption = caption_slot();
        let kinds = |cols: u16| -> Vec<TabHit> {
            layout_segments_with_caption(
                cols,
                1,
                &[PLAIN_TAB],
                0,
                false,
                VersionSlotCols::NONE,
                caption,
            )
            .into_iter()
            .filter_map(|s| matches!(s.kind, TabHit::Caption(_)).then_some(s.kind))
            .collect()
        };
        assert_eq!(kinds(80).len(), 3);
        assert_eq!(
            kinds(CAPTION_STRIP_FLOOR + 8),
            [
                TabHit::Caption(CaptionButton::Maximize),
                TabHit::Caption(CaptionButton::Close)
            ]
        );
        assert_eq!(
            kinds(CAPTION_STRIP_FLOOR + 4),
            [TabHit::Caption(CaptionButton::Close)]
        );
    }

    /// THE CELL LANE'S WINDOW BUTTONS — the frame before the UI face lands: each
    /// slot bare band at rest and the band's hover material under the pointer,
    /// with its mark in the slot's middle cell.
    #[test]
    fn the_cell_lane_paints_each_window_button_with_its_mark() {
        let theme = Theme::default();
        let colors = strip_colors(theme);
        let metadata = [PLAIN_TAB];
        let titles = vec!["one".to_string()];
        let segs = layout_segments_with_caption(
            80,
            1,
            &metadata,
            0,
            false,
            VersionSlotCols::NONE,
            caption_slot(),
        );
        if STRIP_IS_CHROME_BAND {
            assert_ne!(
                strip_cell(' ', &colors, StripRole::Hover).bg,
                strip_cell(' ', &colors, StripRole::Inactive).bg,
                "the hover material differs from bare band"
            );
        }
        for hovered in [None, Some(CaptionButton::Close)] {
            let mut row = vec![blank_cell(theme); 80];
            paint_strip_with_metadata(
                &mut row,
                &segs,
                &titles,
                &metadata,
                StripPaint {
                    caption_hovered: hovered,
                    ..StripPaint::default()
                },
                0,
                theme,
                None,
            );
            for (button, mark, start) in [
                (CaptionButton::Minimize, '_', 68usize),
                (CaptionButton::Maximize, '□', 72),
                (CaptionButton::Close, '✕', 76),
            ] {
                assert_eq!(row[start + 2].ch, mark, "{button:?}");
                let role = if hovered == Some(button) {
                    StripRole::Hover
                } else {
                    StripRole::Inactive
                };
                let want = strip_cell(' ', &colors, role).bg;
                for (i, cell) in row[start..start + 4].iter().enumerate() {
                    assert_eq!(cell.bg, want, "{button:?} hovered {hovered:?}, cell {i}");
                }
            }
        }
    }
}

/// THE IN-GRID NEEDS PILL (owner surfaces §5): its regions are cut from the
/// engine's layout by the engine's hit rule, and painted by that same layout,
/// so a click lands on what the strip shows there.
#[cfg(test)]
mod pill_strip_tests {
    use super::*;
    use crate::presence::{NeedSeverity, PillAct, PillHit, PillKind, PillWords};

    fn words(label: &str, said: &str, act: Option<PillAct>, more: usize) -> PillWords {
        PillWords {
            glyph: '\u{25c6}',
            label: label.to_string(),
            words: said.to_string(),
            room: said.to_string(),
            act,
            more: (more > 0).then(|| format!("+{more}")),
        }
    }

    fn theme() -> Theme {
        Theme::default()
    }

    /// The strip with `pill` laid out and painted, at `cols` columns, three
    /// plain tabs.
    fn strip(cols: u16, pill: &PillWords) -> (Vec<TabSegment>, Vec<RenderCell>) {
        let metadata = [PLAIN_TAB; 3];
        let segments = layout_segments_with_pill(
            cols,
            3,
            &metadata,
            0,
            false,
            VersionSlotCols::NONE,
            Some(pill),
            TabSizing::FIXED,
        );
        let theme = theme();
        let titles: Vec<String> = ["one", "two", "three"].map(String::from).to_vec();
        let mut row = vec![blank_cell(theme); usize::from(cols)];
        let _ = paint_strip_with_metadata(
            &mut row,
            &segments,
            &titles,
            &metadata,
            StripPaint::default(),
            0,
            theme,
            None,
        );
        paint_strip_pill(
            &mut row,
            &segments,
            StripPill {
                kind: PillKind::Need(NeedSeverity::Stuck),
                words: pill,
            },
            theme.into(),
        );
        (segments, row)
    }

    /// The columns whose painted characters spell `text`, starting at the
    /// first that does.
    fn painted_at(row: &[RenderCell], text: &str) -> std::ops::Range<u16> {
        let chars: Vec<char> = text.chars().collect();
        let start = (0..row.len())
            .find(|&at| {
                chars
                    .iter()
                    .enumerate()
                    .all(|(i, ch)| row.get(at + i).is_some_and(|c| c.ch == *ch))
            })
            .unwrap_or_else(|| panic!("{text:?} is painted"));
        let start = u16::try_from(start).expect("a strip column");
        start..start + u16::try_from(chars.len()).expect("short")
    }

    /// EVERY PAINTED PIECE RESOLVES TO ITS OWN ACT: the glyph, the label and
    /// the words to the body, the act's word to the act, the `+N` to the
    /// next need — columns found by what the painter drew, resolved by the
    /// strip's own hit test.
    #[test]
    fn each_painted_piece_of_the_pill_resolves_to_its_act() {
        let pill = words("fmt (tab 4)", "never started", Some(PillAct::Restart), 2);
        let (segments, row) = strip(120, &pill);
        let at = |text: &str| painted_at(&row, text);
        for (text, want) in [
            ("\u{25c6}", PillHit::Go),
            ("fmt (tab 4)", PillHit::Go),
            ("never started", PillHit::Go),
            ("Restart", PillHit::Act(PillAct::Restart)),
            ("+2", PillHit::More),
        ] {
            for col in at(text) {
                assert_eq!(
                    hit_test(&segments, col),
                    Some(TabHit::Pill(want)),
                    "{text:?} at {col}"
                );
            }
        }
        // Every column of the pill is one of its regions, none the bare band.
        let pill_cols: Vec<_> = segments
            .iter()
            .filter(|s| matches!(s.kind, TabHit::Pill(_)))
            .collect();
        let (from, to) = (pill_cols[0].start_col, pill_cols.last().unwrap().end_col);
        assert!((from..to).all(|col| matches!(hit_test(&segments, col), Some(TabHit::Pill(_)))));
        // At most 40% of the strip, and the tabs and the `+` left of it.
        assert!(to - from <= 120 * 2 / 5, "{from}..{to}");
        assert!(
            segments
                .iter()
                .filter(|s| !matches!(s.kind, TabHit::Pill(_)))
                .all(|s| s.end_col <= from)
        );
    }

    /// THE PILL'S AGE MOVES NO TAB (§5): two pills that differ only in their
    /// age, within its unit (3s and 47s), lay the strip out to the same
    /// column — the words' span is measured at their room, the age's widest
    /// figure — while the age itself is painted as it is. NEGATIVE CONTROL:
    /// measured by the words as drawn, the two pills' widths differ.
    #[test]
    fn a_pills_age_moves_no_tab_within_its_unit() {
        let t0 = std::time::Instant::now();
        let pill = aterm_messages::presence::Pill {
            kind: PillKind::Need(NeedSeverity::Stuck),
            session: 1,
            label: "fmt".into(),
            tab: Some(4),
            what: "never started".into(),
            since: Some(t0),
            act: None,
            more: 0,
        };
        let at = |secs: u64| {
            crate::presence::pill_words(&pill, t0 + std::time::Duration::from_secs(secs))
        };
        let (young, old) = (at(3), at(47));
        assert_ne!(young.words, old.words, "the ages differ");
        let metadata = [PLAIN_TAB; 3];
        let lay = |w: &PillWords| {
            layout_segments_with_pill(
                120,
                3,
                &metadata,
                0,
                false,
                VersionSlotCols::NONE,
                Some(w),
                TabSizing::FIXED,
            )
        };
        assert_eq!(lay(&young), lay(&old), "the tabs hold still");
        let (_, row) = strip(120, &old);
        let _ = painted_at(&row, "47s");
        // CONTROL: by the words as drawn the two would not hold still.
        let drawn = |w: &PillWords| PillWords {
            room: w.words.clone(),
            ..w.clone()
        };
        assert_ne!(lay(&drawn(&young)), lay(&drawn(&old)));
    }

    /// The share clips the words first, then the label, never the glyph; the
    /// act stays whole while the glyph fits beside it.
    #[test]
    fn a_narrow_strip_clips_the_words_then_the_label_never_the_glyph() {
        let long = "a very long need sentence that will not fit anywhere at all";
        let pill = words("a long tab label (tab 9)", long, Some(PillAct::Restart), 0);
        let (segments, row) = strip(60, &pill);
        assert!(
            !painted_at(&row, "\u{25c6}").is_empty(),
            "the glyph is drawn"
        );
        let act = painted_at(&row, "Restart");
        assert_eq!(
            hit_test(&segments, act.start),
            Some(TabHit::Pill(PillHit::Act(PillAct::Restart)))
        );
        let text: String = row.iter().map(|c| c.ch).collect();
        assert!(!text.contains(long), "the words were clipped");
        let span: u16 = segments
            .iter()
            .filter(|s| matches!(s.kind, TabHit::Pill(_)))
            .map(|s| s.end_col - s.start_col)
            .sum();
        assert!(span <= 60 * 2 / 5, "{span}");
    }

    /// NEGATIVE CONTROL: with no pill the shipping layout is the pill-less
    /// one to the column, and a version label keeps the strip's right edge
    /// beside a pill.
    #[test]
    fn no_pill_is_the_pill_less_strip_and_the_version_label_stays_flush_right() {
        let metadata = [PLAIN_TAB; 3];
        let slot = VersionSlotCols {
            full: 8,
            short: 4,
            attention: false,
        };
        assert_eq!(
            layout_segments_with_pill(120, 3, &metadata, 1, false, slot, None, TabSizing::FIXED),
            layout_segments_with_version(120, 3, &metadata, 1, false, slot)
        );
        let pill = words("", "frozen", None, 0);
        let segs = layout_segments_with_pill(
            120,
            3,
            &metadata,
            1,
            false,
            slot,
            Some(&pill),
            TabSizing::FIXED,
        );
        let version = segs
            .iter()
            .find(|s| matches!(s.kind, TabHit::Version { .. }))
            .expect("the label still fits");
        assert_eq!(version.end_col, 120, "flush right");
        let pill_end = segs
            .iter()
            .filter(|s| matches!(s.kind, TabHit::Pill(_)))
            .map(|s| s.end_col)
            .max()
            .expect("the pill is laid out");
        assert_eq!(pill_end, version.start_col, "the pill sits left of it");
    }
}

/// WHO DRIVES (owner surfaces §5, S9): the strip's team marks.
#[cfg(test)]
mod team_title_tests {
    use super::*;

    /// A worker's title opens with `↳`, a manager's ends with `⇉N`, a tab
    /// in both wears both; a strip whose tabs wear no mark borrows its
    /// titles whole (the negative control: nothing is allocated for it).
    #[test]
    fn workers_and_managers_wear_their_marks_and_a_plain_strip_borrows() {
        let worker = TabTeam {
            worker: true,
            workers: 0,
        };
        let manager = TabTeam {
            worker: false,
            workers: 3,
        };
        assert!(team_title("bench", worker).starts_with('\u{21b3}'));
        assert!(team_title("lab-sweep", manager).ends_with("\u{21c9}3"));
        let both = team_title(
            "mid",
            TabTeam {
                worker: true,
                workers: 2,
            },
        );
        assert!(both.starts_with('\u{21b3}') && both.ends_with("\u{21c9}2"));
        assert!(matches!(
            team_title("plain", TabTeam::NONE),
            std::borrow::Cow::Borrowed("plain")
        ));
        let titles = vec!["a".to_string(), "b".to_string()];
        assert!(matches!(
            team_titles(&titles, &[PLAIN_TAB; 2]),
            std::borrow::Cow::Borrowed(_)
        ));
        let marked = team_titles(
            &titles,
            &[
                PLAIN_TAB,
                TabStripMetadata {
                    team: worker,
                    ..PLAIN_TAB
                },
            ],
        );
        assert_eq!(marked[0], "a");
        assert!(marked[1].starts_with('\u{21b3}'));
    }
}

/// A tab's need mark wears the border's tone on the in-grid strip.
#[cfg(test)]
mod status_mark_ink_tests {
    use super::*;

    /// A stuck need's mark is the stop ink and a decide need's the wait ink
    /// (the border's two tones, as the native chip draws them); a dirty tab
    /// with no need keeps the accent and a quiet one the label's ink, and so
    /// does the tab's own note (a failed command): no need, no presence tone.
    /// CONTROL: the two need inks differ from each other and from the accent.
    #[test]
    fn a_needs_mark_wears_the_borders_tone() {
        let theme = Theme::default();
        let colors = strip_colors_with_active(theme, None);
        let inks = aterm_messages::presence::inks(
            crate::chrome_band::presence_tones(theme),
            colors.band_bg,
        );
        let ink = [1, 2, 3];
        let of = |attention, dirty| {
            status_mark_ink(
                TabStripMetadata {
                    attention,
                    dirty,
                    ..PLAIN_TAB
                },
                &colors,
                theme.into(),
                ink,
            )
        };
        let stuck = of(ChipLevel::Stop(crate::presence::StopCause::Wall), true);
        let decide = of(ChipLevel::Wait, false);
        assert_eq!(stuck, inks.stop);
        assert_eq!(decide, inks.wait);
        assert_eq!(of(ChipLevel::Done, true), colors.accent);
        assert_eq!(of(ChipLevel::Off, false), ink);
        let note = of(ChipLevel::Note, false);
        assert_eq!(note, ink);
        assert!(note != inks.stop && note != inks.wait && note != inks.yours);
        assert!(stuck != decide && stuck != colors.accent && decide != colors.accent);
    }
}
