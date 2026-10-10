// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A clickable tab strip composed at the GUI level (no renderer pass).
//!
//! Tabs are state ([`crate::TabIndex`] + one [`crate::pane::PaneTree`] per tab).
//! This module reserves `tab_strip_rows` rows (config, default 1) at the top of
//! the window and draws one segment per top-level tab: its title, an active-tab
//! highlight and a close `x`, plus a trailing `+` to open a tab.
//!
//! Every chrome band uses chip cards ([`STRIP_CHIP_CARDS`]): each tab is an inset
//! card separated by gutter columns, the selection is a filled card, the selected
//! card's `✕` is resident, and truncation keeps what distinguishes a title from
//! its neighbours' ([`distinct_chip_labels`]). [`pixel_band`] paints the cards in
//! pixels on a canvas lifted to the window's top edge, so insets, gutters and
//! centring are not bound to the cell grid.
//!
//! Below a legible chip width ([`STRIP_SEATS_FLOOR`]) the strip stops shrinking
//! and seats a window of tabs containing the selection, marking continued edges
//! with `‹`/`›` ([`strip_overflow`]). The window is a function of
//! `(cols, tab_count, active)` with no stored scroll offset, so the hit map and
//! the painter cannot disagree.
//!
//! Layout is pure, mirroring [`crate::pane`]: [`layout_segments`] produces the
//! segments (column range, optional close column, [`TabHit`] target) and is
//! testable without a window; [`paint_strip`] writes them into the composed
//! frame's top rows with [`RenderCell`] + [`Theme`] colours. A click in
//! `row < tab_strip_rows` maps through [`hit_test`] before the focused pane's
//! cell mapping.
//!
//! With `tab_strip_rows == 0` nothing here runs and the composed frame is the
//! terminal grid unchanged. The strip is spliced above the terminal content in
//! the composed `RenderInput` only; session grids are never shifted.

use std::cell::RefCell;
use std::sync::Arc;

use aterm_core::grid::extra::{ImageData, ImageFormat, ImageRef, ImageScaling};
use aterm_core::terminal::{RenderCell, UnderlineStyle};
use aterm_render::Theme;

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
    /// The attention mark level: `Wait` is the hollow diamond, `Stop` a filled
    /// one (held or at a limit), `Story` a dot (something happened while the
    /// user was away). `App::stamp_presence_chips` folds the session's level over it.
    pub(crate) attention: ChipLevel,
    /// The connection-mark role; one shape per role. The `Hash` derive folds it
    /// into the in-grid strip fingerprint.
    pub(crate) conn: Option<TabConnRole>,
    pub(crate) closable: bool,
    /// This tab hosts the session under a live drag-to-connect cursor. Transient
    /// App state: always `false` from [`Self::from_presentation`], stamped by
    /// `App::stamp_conn_drop_target` after each build. Not a status mark: it never
    /// counts toward `status_count` or the connector cell.
    pub(crate) drop_target: bool,
}

impl TabStripMetadata {
    #[must_use]
    pub(crate) fn from_presentation(presentation: &crate::tab_model::TabPresentation) -> Self {
        Self {
            icon: presentation.icon,
            dirty: presentation.indicators.dirty,
            busy: presentation.indicators.busy,
            // The strip draws ONE attention mark, so the two owners fold here.
            attention: crate::app_presence::chip_of_attention(
                presentation.indicators.wants_attention(),
            ),
            conn: presentation.conn,
            closable: presentation.closable,
            drop_target: false,
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

    #[cfg(test)]
    const fn clean(icon: TabIconKind) -> Self {
        Self {
            icon: Some(icon),
            dirty: false,
            busy: false,
            attention: ChipLevel::Off,
            conn: None,
            closable: true,
            drop_target: false,
        }
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

/// Horizontal centre for one status mark in a 16-unit design box. Active marks pack
/// toward the middle without leaving mystery gaps when only one or two states exist.
#[must_use]
pub(crate) fn tab_status_center(ordinal: usize, count: usize) -> f32 {
    match count {
        0 => 8.0,
        1 => 8.0,
        // The busy ring (half-extent 2.625) and the attention diamond (3.075,
        // 3.31 mitred by AppKit) need 5.94 between centres; 5.5 left the two
        // marks of a working Claude tab touching, 7.0 leaves 1.3 units (about
        // 2.6 px at 2x).
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
    // Largest half-extent is the busy ring's 2.625; 0.75 keeps adjacent pairs
    // clear on the 4-unit pitch and edge marks (centres 2.0/14.0) inside the box.
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
    /// Switch the window to this tab index (a click anywhere on the segment that
    /// is NOT the close `x`).
    Select(usize),
    /// Close this tab index (a click on the segment's close `x`).
    Close(usize),
    /// Open a NEW tab (a click on the trailing `+`).
    NewTab,
    /// A staged update is ready — a click on the LEADING update icon (`↻`) applies
    /// it. Shown as an alert only while a build is staged; matches the `+` affordance.
    Update,
    /// The tab's connector: its status-mark cell. Arm-on-press, commit-on-release:
    /// release within the movement threshold opens the session Connections menu
    /// (never selecting the tab); movement past it starts drag-to-connect
    /// (`crate::conn_drag`). Exists only while the segment painted a status canvas
    /// ([`TabSegment::connector_col`]).
    Connector(usize),
    /// The build's VERSION LABEL at the strip's trailing end (Windows only —
    /// [`STRIP_VERSION_SLOT`]): the macOS menu bar's Version title without its
    /// colour emoji ([`crate::menu::strip_version_label`] at `rung`, ` ↑` after
    /// it while `attention` holds). A BUTTON, committed on the press like the
    /// `+` and the `↻`: it fires [`crate::menu::version_slot_click`] — Settings ▸
    /// About — and is never caption, so it neither drags nor maximizes the
    /// window. Laid out only where it fits without costing any tab its seat
    /// ([`layout_segments_with_version`]).
    Version {
        /// Which length of the label this layout had room for.
        rung: crate::menu::VersionRung,
        /// An update is staged ([`crate::menu::staged_for_version_menu`]).
        attention: bool,
    },
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
    /// The column of the painted STATUS-MARK canvas, if this segment drew one —
    /// the tab's connection CONNECTOR (design §3.1 [v5], the `close_col`
    /// precedent). A click here is a [`TabHit::Connector`]. Set only by the
    /// metadata-aware layout ([`layout_segments_with_version`]) from the SAME
    /// math the painter uses, so hit geometry equals painted geometry by
    /// construction; the plain [`layout_segments`] leaves it `None`.
    pub connector_col: Option<u16>,
    /// The action a plain (non-close, non-connector) click on this segment
    /// performs.
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
/// Columns the LEADING update icon (`↻`) occupies: ` ↻ ` — same width as `+` so it
/// reads as a sibling affordance.
const UPDATE_W: u16 = 3;

/// How wide a lone tab's chip may grow, in columns; `None` where a lone tab is the
/// window title band and spends the whole strip (macOS; see [`SOLO_TITLE_BAND`]).
///
/// Elsewhere a full-width raised card would read as a heavy grey slab, so the lone
/// chip is capped and stays leading, leaving band to its right for the `+`. A fixed
/// width because `layout_segments` never sees titles (a content-sized chip would
/// also move the `✕` on every OSC-title change). 28 leaves ~24 title cells. A
/// narrow window still gets the smaller equal share: this is a maximum.
const SOLO_CHIP_MAX_COLS: Option<u16> = if cfg!(target_os = "macos") {
    None
} else {
    Some(LONE_CHIP_COLS)
};

/// The lone chip's width off macOS ([`SOLO_CHIP_MAX_COLS`]), named on its own
/// because the trailing version label ([`TabHit::Version`]) is admitted beside a
/// lone tab only while the band left of it still holds a chip this wide — so the
/// label can never be the reason a one-tab window's chip shrinks.
const LONE_CHIP_COLS: u16 = 28;

/// The EQUAL-SHARE legibility floor, in columns. While every tab's equal share
/// stays at or above this, tabs split the band evenly; once a share would drop
/// below it, [`layout_segments`] switches to the ACTIVE-PRIORITY layout — the
/// selected tab reserves a useful width and the inactive tabs compress around it.
///
/// The native strip's `toolbar::PREFERRED_MIN_TAB_WIDTH` (64 pt) in cells: after
/// the pads and the reserved `✕` slot ([`tab_content_layout`]), 12 columns keep
/// 8 title cells ("Settings").
const PREFERRED_MIN_TAB_COLS: u16 = 12;

/// The most band the active tab may reserve under the pressure layout: the native
/// strip's 96 pt cap (`toolbar::native_tab_cells`) at the same 96:64 ratio
/// (1.5 × 12 = 18). Below the cap the active share is bounded by 60% of the band.
const ACTIVE_TAB_PRESSURE_CAP_COLS: u16 = 18;

/// The width, in columns, below which the strip stops compressing and shows a
/// window of its tabs instead ([`layout_segments`]).
///
/// Equal to [`PREFERRED_MIN_TAB_COLS`], the module's one legibility floor. Below
/// it no title cut can name a tab, so the strip seats what fits and scrolls past
/// the rest, as native tab strips do, marking the continued edges with `‹`/`›`
/// like the inline rename well ([`paint_rename_field`]).
///
/// It is the plain chip's floor: where the seat search starts
/// ([`chip_seat_cols`]). A chip that also carries a status canvas or an app icon
/// needs more columns for the same title width.
const STRIP_SEATS_FLOOR: u16 = PREFERRED_MIN_TAB_COLS;

/// The fewest chips a windowed strip seats, however narrow the band. One chip is a
/// title bar, not a switcher. Seats are also capped at one per column, so a band
/// too narrow for two chips seats one rather than emitting zero-width segments.
const STRIP_MIN_SEATS: usize = 2;

/// How many chips `band` columns seat: one per `seat` columns, where `seat` is the
/// narrowest chip this strip's chrome can carry a title in ([`strip_seat_cols`]).
///
/// Clamped into `1..=tab_count` and to one seat per column. No active
/// reservation: seated chips share the band equally ([`strip_window`]).
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
    // the WIDER edge sets both insets, because the title is centred in the cell.
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
    drop_target: false,
    closable: true,
};

/// Title/icon/status slots for one laid-out tab segment. The segment and
/// `close_col` stay authoritative hit geometry; this only divides paint space.
/// When width is scarce, title width goes first, then the dirty mark, then the icon.
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

    // All status marks share one shape-coded status canvas, shown only when a
    // title cell survives; paint never overwrites the close affordance.
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
        connector_col: None,
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
/// - tabs divide the band into equal shares with no maximum width;
/// - a lone tab is the window title: one full-band [`TabSegment::solo`] segment
///   with no close column (macOS only; elsewhere it is a leading chip capped at
///   [`SOLO_CHIP_MAX_COLS`], see [`SOLO_TITLE_BAND`]);
/// - under pressure (equal share below [`PREFERRED_MIN_TAB_COLS`]) the active tab
///   reserves 60% of the band, capped at [`ACTIVE_TAB_PRESSURE_CAP_COLS`], and
///   the inactive tabs compress around it;
/// - below [`STRIP_SEATS_FLOOR`] compression stops and the strip seats a window
///   of tabs containing the selection, marking continued edges with `‹`/`›`
///   ([`strip_overflow`]). Unseated tabs stay reachable by cycling and
///   `switch_tab_<n>`.
///
/// PLAIN CHROME. This entry point measures the legibility floor against a
/// [`PLAIN_TAB`] chip, because it is handed no metadata to measure anything
/// else. The shipping strip goes through [`layout_segments_with_version`],
/// which measures it against the chrome its tabs actually wear
/// ([`strip_seat_cols`]) — the two agree exactly wherever every tab is plain.
///
/// The metadata-free entry the ~40 layout tests hold the pure-geometry contract
/// against; the shipping strip reaches the same code through the metadata-aware
/// door.
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
    )
}

/// [`layout_segments`] with the seat width (`seat`, from [`strip_seat_cols`]):
/// the one number the two entry points disagree about.
fn layout_segments_seated(
    cols: u16,
    tab_count: usize,
    active: usize,
    show_update: bool,
    seat: u16,
) -> Vec<TabSegment> {
    let mut segs = Vec::new();
    if cols == 0 {
        return segs;
    }
    // LEADING update icon (`↻`) when a build is staged — a small alert affordance at
    // the far left of the strip, shifting the tabs right by its width.
    let lead = if show_update && cols > UPDATE_W {
        segs.push(TabSegment {
            start_col: 0,
            end_col: UPDATE_W,
            close_col: None,
            connector_col: None,
            kind: TabHit::Update,
            solo: false,
        });
        UPDATE_W
    } else {
        0
    };
    // Reserve the trailing `+` when there's room for at least one tab AND the `+`.
    let plus_room = cols > lead + NEW_TAB_W;
    let avail = if plus_room { cols - NEW_TAB_W } else { cols };
    // One tab is the window title where the window has no title of its own
    // (macOS, `SOLO_TITLE_BAND`): the whole band, no close column. Elsewhere a
    // lone tab is an ordinary chip with its card, selection rule and close column.
    let solo = tab_count == 1 && SOLO_TITLE_BAND;
    let mut x: u16 = lead;
    if tab_count > 0 {
        let band = avail.saturating_sub(lead);
        // Equal shares of the band past the leading update icon, no maximum for
        // two or more tabs; floored so a tab is at least 1 cell.
        let share = band / tab_count as u16;
        // One tab off macOS: capped chip ([`SOLO_CHIP_MAX_COLS`]) kept leading,
        // so the band and the `+` show to its right.
        let per = match SOLO_CHIP_MAX_COLS {
            Some(cap) if tab_count == 1 => share.min(cap),
            _ => share,
        };
        // Active priority under pressure (`toolbar::native_tab_cells`, in cells):
        // below the legibility floor the selected tab gets up to 60% of the band,
        // capped, and inactive tabs split the rest, floored at one cell. Above the
        // floor shares are equal and selection-independent.
        //
        // When even compression cannot seat every tab at the floor
        // ([`STRIP_SEATS_FLOOR`]), the strip seats a page of `seats` chips from
        // `first` ([`strip_window`]) with a marked column at each continued end.
        // When every tab fits, `seats == tab_count`, `first == 0` and nothing is
        // marked.
        let active = active.min(tab_count - 1);
        let mut seats = tab_count;
        let mut first = 0usize;
        let (mut mark_left, mut mark_right) = (false, false);
        let (active_w, inactive_w) = if tab_count > 1 && share < seat {
            if strip_seats(band, tab_count, seat) >= tab_count {
                // Every tab clears the floor: the pressure split. u32 because
                // `band * 3` can overflow u16.
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
        x += u16::from(mark_left);
        for i in first..(first + seats).min(tab_count) {
            let per = if i == active { active_w } else { inactive_w };
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
                connector_col: None,
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
            connector_col: None,
            kind: TabHit::NewTab,
            solo: false,
        });
    }
    segs
}

/// Canonical-presentation-aware layout — the shipping strip's, less its version
/// label ([`layout_segments_with_version`]).  The base segment
/// order, select hit areas, reorder geometry, and trailing `+` remain exactly
/// [`layout_segments`]'s.  Only a tab explicitly marked non-closable loses its close
/// column; no icon/status field can shrink or move a hit target.
///
/// ...AND THE LEGIBILITY FLOOR IS MEASURED ON THIS STRIP'S CHIPS. The one thing
/// the metadata is allowed to move is the WIDTH AT WHICH THE STRIP GIVES UP ON
/// EQUAL SHARES ([`strip_seat_cols`]) — because a busy chip's title is three
/// cells shorter than an idle one's at the same width, and a floor tested
/// against the segment promised those three cells to a title that never got
/// them. Eight busy shells at 100 columns each held a chip exactly at the floor
/// and painted `1  2  3  4  5  6  7`. Above the floor nothing moves: while the
/// shares are legible for the chrome the tabs wear, this function's output is
/// [`layout_segments`]'s to the column, so the hover-reserved `✕` still cannot
/// be moved by a tab going busy on a strip that has width to spare.
///
/// The strip with NO version label ([`VersionSlotCols::NONE`]) — the entry the
/// layout tests hold the strip's own contract against. The shipping splice calls
/// [`layout_segments_with_version`].
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

/// How many columns each rung of the strip's VERSION LABEL needs at this
/// window's metrics ([`pixel_band::version_slot_cols`] measures them in the
/// band's own UI face and pen), and whether the label carries the update mark.
/// A width of 0 is a rung that does not exist here — a release's short rung, or
/// any rung before the UI face is installed — so [`Self::NONE`] lays out no
/// label at all and the strip is exactly the one this module always drew.
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

/// THE SHIPPING STRIP LAYOUT: [`layout_segments_seated`] and the metadata pass,
/// plus — where it fits — the build's VERSION LABEL flush against the strip's
/// right edge ([`TabHit::Version`], `[cols - w, cols)`).
///
/// THE LABEL YIELDS; THE TABS NEVER DO. Everything left of the label is today's
/// layout of `cols - w` columns, to the column — the `+` (clamped to
/// `cols - w - NEW_TAB_W`), the leading `↻`, the `‹`/`›` marks (which only take
/// columns no segment covers) — so nothing can land in the label's columns, and
/// the label is admitted only while it costs no tab anything
/// ([`version_rung_that_fits`]): a lone chip keeps its full [`LONE_CHIP_COLS`],
/// and two or more tabs keep an equal share at least their seat
/// ([`strip_seat_cols`]), so the strip never enters its pressure or windowed
/// layout on the label's account. The full rung is tried first, then the short
/// one; if neither fits there is no label and the layout is today's whole strip.
#[must_use]
pub(crate) fn layout_segments_with_version(
    cols: u16,
    tab_count: usize,
    metadata: &[TabStripMetadata],
    active: usize,
    show_update: bool,
    slot: VersionSlotCols,
) -> Vec<TabSegment> {
    let seat = strip_seat_cols(tab_count, metadata);
    let version = version_rung_that_fits(cols, tab_count, seat, show_update, slot);
    let laid_cols = version.map_or(cols, |(_, w)| cols - w);
    let mut segments = layout_segments_seated(laid_cols, tab_count, active, show_update, seat);
    for segment in &mut segments {
        let TabHit::Select(index) = segment.kind else {
            continue;
        };
        if metadata.get(index).is_some_and(|item| !item.closable) {
            segment.close_col = None;
        }
        // The connector hit region: the status-mark cell the painter draws, from
        // the same math. Solo band marks paint at `end_col - 2` (span > 6);
        // chips use `tab_content_layout`'s `status_col`.
        let Some(item) = metadata.get(index).copied().filter(|it| it.has_status()) else {
            continue;
        };
        segment.connector_col = if segment.solo {
            (segment.end_col.saturating_sub(segment.start_col) > 6)
                .then(|| segment.end_col.saturating_sub(2))
        } else {
            tab_content_layout(segment, item).status_col
        };
    }
    if let Some((rung, w)) = version {
        segments.push(TabSegment {
            start_col: laid_cols,
            end_col: laid_cols + w,
            close_col: None,
            connector_col: None,
            kind: TabHit::Version {
                rung,
                attention: slot.attention,
            },
            solo: false,
        });
    }
    segments
}

/// Which rung of the version label fits beside `tab_count` tabs on a `cols`-wide
/// strip, and how wide it is — or `None`, and the strip shows no label.
///
/// Full first, then short; a rung of width 0 (absent) or one at least as wide as
/// the strip is skipped. A rung fits when the strip LEFT of it (`inner`) still
/// holds the `↻` (if shown) and the `+`, and the band between them still gives:
/// no tab — always; one tab — a whole [`LONE_CHIP_COLS`] chip, so the lone chip
/// never shrinks for the label; `n ≥ 2` tabs — an equal share of at least `seat`
/// ([`strip_seat_cols`]) each, so the label yields before the strip would start
/// compressing or windowing its tabs.
fn version_rung_that_fits(
    cols: u16,
    tab_count: usize,
    seat: u16,
    show_update: bool,
    slot: VersionSlotCols,
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
            1 => band >= LONE_CHIP_COLS,
            n => {
                let n = u32::try_from(n).unwrap_or(u32::MAX);
                u32::from(band) / n >= u32::from(seat)
            }
        }
    })
}

/// Map a strip click at column `col` to its [`TabHit`], or `None` for a click on
/// bare strip background (between/after segments). A click on a segment's
/// `close_col` is a [`TabHit::Close`], on its `connector_col` a
/// [`TabHit::Connector`] (the status-mark cell — design §3.1 [v5]); any other
/// column of a tab segment selects it; the `+` segment opens a tab.
#[must_use]
pub(crate) fn hit_test(segments: &[TabSegment], col: u16) -> Option<TabHit> {
    for seg in segments {
        if col >= seg.start_col && col < seg.end_col {
            if let (Some(cx), TabHit::Select(i)) = (seg.close_col, seg.kind)
                && col == cx
            {
                return Some(TabHit::Close(i));
            }
            if let (Some(cx), TabHit::Select(i)) = (seg.connector_col, seg.kind)
                && col == cx
            {
                return Some(TabHit::Connector(i));
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
        // The SOLO band is the window's title, not a switcher, and it is the
        // one tab there is: nothing to overflow.
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
    /// The trailing `+` new-tab affordance: a BUTTON — full-strength fg on the body
    /// (NOT the dim inactive treatment), so it meets WCAG-AA contrast on every theme.
    NewTab,
    /// The `+` under the pointer: the band's hover material
    /// ([`StripColors::hover_bg`]) under the button's full-strength ink.
    NewTabHover,
    /// The solo band's title: full-strength fg on the body, no raised card and no
    /// selection underline.
    Title,
    /// The leading `↻` update-ready alert: a RAISED highlighted button (the active-tab
    /// treatment) so it stands out from the flat `+` and the receded tabs.
    Update,
}

/// Whether the in-grid strip paints itself as a chrome band (own surface tone, a
/// hairline seam against the grid, a raised `+`) instead of receding into the
/// terminal background.
///
/// Not on macOS: there the in-grid strip is a fallback for the native AppKit
/// toolbar (`DEFAULT_TAB_STRIP_ROWS` is 0), composites over titlebar vibrancy, and
/// a grey band looked heavy (see [`strip_colors`]). Elsewhere it is the window's
/// only chrome, so it takes a surface of its own.
const STRIP_IS_CHROME_BAND: bool = !cfg!(target_os = "macos");

/// Does the strip carry the build's VERSION LABEL at its trailing end
/// ([`TabHit::Version`])?
///
/// WINDOWS ONLY, and spelled `cfg!(windows)` rather than borrowed from
/// [`STRIP_IS_CHROME_BAND`], which covers Linux too. macOS shows the version as
/// the last menu of its menu bar (`menu::version_menu_bar_title`); Windows has no
/// menu bar, so before this label the version was nowhere at a glance (owner,
/// 2026-10-01: "aterm on OSX has a menu bar that shows the version. where is that
/// for Windows?"). Linux keeps its strip as it was until that lane is reviewed;
/// turning it on there is this one word. A window with no strip
/// (`tab_strip_rows == 0`) has no label.
pub(crate) const STRIP_VERSION_SLOT: bool = cfg!(windows);

/// The trailing VERSION LABEL's card and ink ([`TabHit::Version`]) in each
/// state: the update CTA's card and ink while an update is staged (it outranks
/// the pointer, as the `↻` it echoes does); the band's hover card and ink under
/// the pointer; no card (bare band) and the inactive ink at rest — strip
/// content, not a tab and not a caption button. The ONE answer: the band raster
/// paints from it, and the High-Contrast contrast rows hold exactly these pairs
/// to the UI-text floor.
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

/// Does a LONE tab collapse into a full-width window-TITLE band (centred title +
/// description, no card, no `✕`) instead of painting an ordinary selected chip?
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
/// On Windows [`pixel_band`] repaints the band in pixels over these cell tones.
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

/// The contrast floor every strip ink is held to against its surface. 3.0:1 rather
/// than AA's 4.5 because muted schemes cannot reach 4.5 even at full strength
/// (Solarized Light's fg is ~4.13:1). A no-op on bundled schemes; it keeps a user
/// theme from putting illegible ink on the band.
const STRIP_INK_FLOOR: f64 = 3.0;

/// How far the bottom seam steps off the band toward the foreground: enough to read
/// as the band's edge at 12 px type, not enough to compete with the active accent.
const STRIP_SEAM_T: f32 = 0.30;

/// The strip tones derived from a theme, computed ONCE per [`paint_strip`]
/// (hoisted out of the per-cell [`strip_cell`] — the blends are frame-invariant).
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
    /// The active tab's raised-button background. Equal to [`Self::raise_bg`]
    /// unless the user's `active_tab_color` replaced it.
    active_bg: [u8; 3],
    /// The theme-derived raise, before any `active_tab_color` override. The `+`
    /// uses this rather than [`Self::active_bg`].
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
    /// Theme accent used by the selected underline and state marks.
    accent: [u8; 3],
    /// Surface / ink / rule for the `↻` update CTA ([`StripRole::Update`]).
    ///
    /// Separate from the selection's colours because under an OS-forced palette
    /// reusing them made the CTA identical to the selected tab and its rule
    /// invisible (see [`forced_strip_colors`]). With a theme-derived strip these
    /// equal the selected pair plus the theme accent.
    update_bg: [u8; 3],
    /// Ink for [`Self::update_bg`].
    update_fg: [u8; 3],
    /// The CTA's underline rule. Always contrasting against [`Self::update_bg`] —
    /// a rule the same colour as its own surface is not a rule.
    update_rule: [u8; 3],
    /// The hairline closing the bottom strip row against the terminal content
    /// ([`seal_strip_bottom`]). `None` without a band ([`STRIP_IS_CHROME_BAND`]).
    seam: Option<[u8; 3]>,
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
fn forced_strip_colors(hc: crate::chrome_band::ForcedChrome) -> StripColors {
    let ink = crate::chrome_band::forced_ink;
    let band_fg = ink(hc.window_text, hc.btn_face);
    let plain_fg = ink(hc.window_text, hc.window);
    StripColors {
        fg: band_fg,
        band_bg: hc.btn_face,
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
    }
}

/// Derive the strip tones from a theme.
///
/// On macOS bare strip and inactive tabs sit on the terminal background, so a
/// sparse strip recedes into the content; the active tab is a raised card (bg
/// stepped toward fg) with bold fg.
///
/// Elsewhere ([`STRIP_IS_CHROME_BAND`]) the strip takes a band surface and the
/// raise is anchored on it: `mix3(band_bg, fg, active_t)`, so the selected card
/// sits the same visible step above the band that macOS puts it above the body
/// (default theme: grid `#111318`, band `#303135`, card `#525256`). On macOS
/// `band_bg == theme.bg`, so the expression is the same.
///
/// Each ink is floored to [`STRIP_INK_FLOOR`] against the surface it is drawn on
/// (band ink against `band_bg`, card ink against `raise_bg`).
///
/// The raise follows `bg_is_light(theme.bg)`: 0.21 toward fg on dark themes, 0.14
/// on light ones, where a deeper step reads as a slab. The inactive-label dim is a
/// mild 0.15 on both, since the raised card carries the focus cue.
/// `strip_contrast_meets_wcag_aa` guards active, inactive and `+` contrast.
///
/// Under an OS-forced chrome palette (Windows High Contrast) [`forced_strip_colors`]
/// answers instead.
fn strip_colors(theme: Theme) -> StripColors {
    // High Contrast: the OS palette owns chrome, returned whole. The terminal grid
    // is untouched (see `chrome_band`'s module comment).
    if let Some(hc) = crate::chrome_band::forced_chrome() {
        return forced_strip_colors(hc);
    }
    let rgb = |c: u32| {
        [
            ((c >> 16) & 0xff) as u8,
            ((c >> 8) & 0xff) as u8,
            (c & 0xff) as u8,
        ]
    };
    // Gentler raise on light themes. The inactive-label dim is mild (0.15): the
    // raised card, not label brightness, carries the focus cue. `hover_t` is a third
    // of the raise, like the native strip's hover pill: present, clearly not
    // selected.
    //
    // On a chip-card band ([`STRIP_CHIP_CARDS`]) quiet chips get their own card, so
    // the ladder re-spaces: band (0) → quiet chip (~1/3 of the raise) → hover wash
    // (~2/3) → selected card (the raise), each step visibly distinct. Light themes
    // take a slightly deeper chip step so quiet cards stay visible on muted schemes
    // (Solarized).
    let light = bg_is_light(rgb(theme.bg));
    let (active_t, inactive_t, hover_t, chip_t) = match (light, STRIP_CHIP_CARDS) {
        (true, false) => (0.14, 0.15, 0.05, 0.0),
        (true, true) => (0.14, 0.15, 0.10, 0.055),
        (false, false) => (0.21, 0.15, 0.07, 0.0),
        (false, true) => (0.21, 0.15, 0.13, 0.06),
    };
    let fg = rgb(theme.fg);
    // The one platform decision in the colour work: which surface the strip is.
    let band_bg = if STRIP_IS_CHROME_BAND {
        crate::chrome_band::band_colors(theme).bar_bg
    } else {
        rgb(theme.bg)
    };
    // Surface blends take the RAW theme fg (a surface is not ink and must not be
    // dragged around by an ink floor); only the inks below are floored.
    let raise_bg = crate::chrome_band::mix3(band_bg, fg, active_t);
    // Each ink is floored against the surface it lands on: band and raised card are
    // two surfaces, and the raise also moves the card toward its ink.
    let ink = |c: [u8; 3], on: [u8; 3]| crate::chrome_band::ensure_contrast(c, on, STRIP_INK_FLOOR);
    let raise_fg = ink(fg, raise_bg);
    // Full-strength ink on the band. Also the CEILING for the dim below.
    let band_fg = ink(fg, band_bg);
    // The inactive dim, floored, then capped at the band ink. `ensure_contrast` only
    // pushes ink away from its surface, so on a muted light scheme it can lift the
    // dim back toward full strength; past the band ink, unfocused labels would
    // outshine the focused one. Inert on bundled schemes
    // (`strip_contrast_meets_wcag_aa`); it guards re-tunes and user themes.
    let dim = ink(crate::chrome_band::mix3(fg, band_bg, inactive_t), band_bg);
    let inactive_fg = if crate::chrome_band::contrast(dim, band_bg)
        > crate::chrome_band::contrast(band_fg, band_bg)
    {
        band_fg
    } else {
        dim
    };
    // The hover wash is a surface (raw fg blend); its label is the inactive ink
    // re-floored on it.
    let hover_bg = crate::chrome_band::mix3(band_bg, fg, hover_t);
    let hover_fg = ink(inactive_fg, hover_bg);
    // The `+` on the wash: full-strength ink floored against the wash.
    let button_hover_fg = ink(fg, hover_bg);
    // The quiet chip's card, same construction. `chip_t` is 0.0 off chip-card
    // platforms, making these copies of their band counterparts.
    let chip_bg = crate::chrome_band::mix3(band_bg, fg, chip_t);
    let chip_fg = ink(inactive_fg, chip_bg);
    let chip_button_fg = ink(fg, chip_bg);
    StripColors {
        fg: band_fg,
        band_bg,
        active_bg: raise_bg,
        raise_bg,
        raise_fg,
        active_fg: raise_fg,
        inactive_fg,
        chip_bg,
        chip_fg,
        chip_button_fg,
        hover_bg,
        hover_fg,
        button_hover_fg,
        accent: rgb(theme.cursor),
        // The selected pair plus the theme accent as its rule; only the
        // forced-palette branch differs.
        update_bg: raise_bg,
        update_fg: raise_fg,
        update_rule: rgb(theme.cursor),
        seam: STRIP_IS_CHROME_BAND.then(|| crate::chrome_band::mix3(band_bg, fg, STRIP_SEAM_T)),
    }
}

/// [`strip_colors`] with the user's `active_tab_color` override: the active tab's
/// background becomes that colour and its label ink flips black/white by the
/// colour's luminance ([`bg_is_light`], the classifier the native toolbar pill
/// uses). `None` is the theme-derived strip.
fn strip_colors_with_active(theme: Theme, active_override: Option<[u8; 3]>) -> StripColors {
    let mut colors = strip_colors(theme);
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
        // The `↻` CTA also follows `active_tab_color`.
        colors.update_bg = active_bg;
        colors.update_fg = active_fg;
    }
    colors
}

/// Whether this background is light, by a perceptual-luma threshold. Every bundled
/// scheme sits well clear of it. The one dark/light classifier for all chrome
/// (this strip and the native toolbar via [`theme_is_dark`]).
///
/// Delegates to the engine's [`aterm_messages::ink::bg_is_light`]. Its `f32` luma
/// is deliberate: colours at a luma of exactly 150 are light there.
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
pub(crate) fn strip_bleed_tones(theme: Theme) -> Option<(u32, Option<u32>)> {
    STRIP_IS_CHROME_BAND.then(|| {
        let pack = |c: [u8; 3]| (u32::from(c[0]) << 16) | (u32::from(c[1]) << 8) | u32::from(c[2]);
        let colors = strip_colors(theme);
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
pub(crate) fn blank_cell(theme: Theme) -> RenderCell {
    strip_cell(' ', &strip_colors(theme), StripRole::Inactive)
}

/// Close the bottom of the strip's last row against the terminal content with a
/// hairline seam, in place. A no-op without a band ([`STRIP_IS_CHROME_BAND`]).
///
/// The seam is an underline on the strip's own last row, not an overline on the
/// first content row, which belongs to the session. On a chip-card band the
/// selection carries no rule, so this is one unbroken hairline.
///
/// Called once per strip rebuild from the row splice; recomputes the tones like
/// [`blank_cell`].
pub(crate) fn seal_strip_bottom(row: &mut [RenderCell], theme: Theme) {
    let Some(seam) = strip_colors(theme).seam else {
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

/// Build the [`RenderCell`] for a strip cell from precomputed [`StripColors`] and the
/// cell's [`StripRole`].
fn strip_cell(ch: char, colors: &StripColors, role: StripRole) -> RenderCell {
    // The active tab is a raised bg with a full-width accent underline, which also
    // seams it against the content below. Inactive tabs and the `+` are flat labels.
    let (fg, bg, bold, underline, underline_color) = match role {
        // On a chip-card band the filled card alone marks the selection: a cell
        // underline would graze descenders, and [`seal_strip_bottom`] closes the
        // band unbroken.
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
        subtitle: None,
        rename: None,
    };
    let _ = paint_strip_impl(row, segments, titles, None, paint, active, theme, None);
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
    /// Is the pointer on the trailing VERSION LABEL ([`TabHit::Version`])? Its
    /// own flag for the reason [`Self::new_tab_hovered`] is one — the label is
    /// not a tab — fed from the same per-motion hit test and folded into the
    /// same strip cache key, so at most one of a chip, the `+` and the label is
    /// lit, and the label takes the band's one hover material.
    pub version_hovered: bool,
    /// The lone tab's one-line description ([`solo_subtitle`]), drawn dim after
    /// the centred title. `None` = nothing the title does not already say.
    pub subtitle: Option<&'a str>,
    /// The live inline SESSION-RENAME field, when this window has one and the
    /// in-grid strip is what presents it. `None` = paint labels as usual.
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
    /// Caret position in `text`, as a BYTE offset on a char boundary.
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
        theme,
        active_override,
    )
    .0
}

/// [`paint_strip_with_metadata`] plus the rename well's painted caret column
/// (`None` when no rename field was drawn). The splice anchors the OS IME
/// candidate window there while the rename field owns a composition
/// ([`crate::app_input::PreeditOwner::Rename`]).
#[allow(
    clippy::too_many_arguments,
    reason = "see `paint_strip_with_metadata`; identical inputs"
)]
pub(crate) fn paint_strip_with_metadata_and_rename_caret(
    row: &mut [RenderCell],
    segments: &[TabSegment],
    titles: &[String],
    metadata: &[TabStripMetadata],
    paint: StripPaint<'_>,
    active: usize,
    theme: Theme,
    active_override: Option<[u8; 3]>,
) -> (Vec<(usize, ImageRef)>, Option<usize>) {
    paint_strip_impl(
        row,
        segments,
        titles,
        Some(metadata),
        paint,
        active,
        theme,
        active_override,
    )
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
    theme: Theme,
    active_override: Option<[u8; 3]>,
) -> (Vec<(usize, ImageRef)>, Option<usize>) {
    // Derive the strip tones ONCE (frame-invariant), not per cell.
    let colors = strip_colors_with_active(theme, active_override);
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
        // Drag-to-connect drop target: the chip under a live connection drag
        // paints raised and accent-underlined (the `Update` treatment). Paint only.
        let drop = match seg.kind {
            TabHit::Select(i) => metadata
                .and_then(|items| items.get(i))
                .is_some_and(|item| item.drop_target),
            _ => false,
        };
        let tab_role = if drop {
            StripRole::Update
        } else if is_active {
            StripRole::Active
        } else if is_hovered {
            StripRole::Hover
        } else if STRIP_CHIP_CARDS && !seg.solo && matches!(seg.kind, TabHit::Select(_)) {
            // A quiet tab on a chip-card band is a CARD, not bare band.
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
                // The lone tab is still a drop target: the whole band takes the
                // raised accent-underlined treatment during a drag.
                let band_role = if drop {
                    StripRole::Update
                } else {
                    StripRole::Inactive
                };
                let title_role = if drop {
                    StripRole::Update
                } else {
                    StripRole::Title
                };
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
                            theme,
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
                // on the segment's leading pad cell (costs no title width, moves no
                // hit target), as the native strip does ([`TabGeometry::separates`]).
                // No platform currently takes this arm: every band uses chip cards.
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
                    let color = if item.dirty || item.attention >= ChipLevel::Wait {
                        colors.accent
                    } else if is_active {
                        colors.fg
                    } else {
                        colors.inactive_fg
                    };
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
                        theme,
                        // Only the active chip keeps its selected seam; a context-menu
                        // rename can target a background tab.
                        is_active.then_some(colors.accent),
                    );
                } else {
                    // The distinct pass's label when it ran; otherwise (solo,
                    // renaming, no band) the head-truncated title.
                    let label = labels.get_mut(i).and_then(Option::take).unwrap_or_else(|| {
                        let avail = layout.title_end.saturating_sub(layout.title_start);
                        truncate_title(raw, avail as usize)
                    });
                    // In display cells, so a title never bleeds into the status
                    // canvas, the `✕` or the next chip.
                    put_text(row, layout.title_start, layout.title_end, &label, tab_role);
                }
                if let Some(cx) = seg.close_col
                    && (paint.hovered == Some(i) || (STRIP_CHIP_CARDS && is_active))
                {
                    // Hover-only on the flat strip and the pixel band, as on the
                    // native strip; a chip-card band keeps the selected card's ✕
                    // resident. The column stays reserved either way, and
                    // `close_col` answers `Close` regardless of paint.
                    //
                    // ✕ (U+2715) is East-Asian-Width Neutral, so it is single-cell
                    // under CJK fonts, unlike × (U+00D7, Ambiguous). Hit-testing
                    // keys off `close_col`, not the glyph.
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
                // every theme.
                //
                // Gutter (chrome band only): `layout_segments` places the `+` flush
                // against the last tab, and on a band `NewTab` and `Active` share
                // `raise_bg`, so a selected last tab would fuse with it. The `+`
                // gives its leading pad cell to the band. Paint only: `hit_test`
                // still routes the whole segment to `TabHit::NewTab`. macOS's flat
                // strip keeps the whole fill.
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
            // The trailing VERSION LABEL — the cell lane's transitional frame
            // for it (the pixel band, once the UI face lands, draws it in that
            // face over these cells). Bare band at rest, the hover wash under
            // the pointer, the update CTA's surface while an update is staged;
            // the text right-aligned with one trailing pad cell, stepping down
            // to the short rung (and then to no text) when the cells are fewer
            // than the measured slot (the slot is measured in the UI face).
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
            // `Close` / `Connector` are never a segment `kind` (only derived
            // hits on `close_col` / `connector_col`).
            TabHit::Close(_) | TabHit::Connector(_) => {}
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
            // Attention level: hollow diamond for wait, filled for stop, a dot for
            // story; the same marks the native chip draws (`toolbar.rs`).
            TabStatusKind::Attention => {
                let w = 1.15 * s;
                let r = 2.5 * s;
                match metadata.attention {
                    ChipLevel::Off => continue,
                    ChipLevel::Story => {
                        primitives.push(TabIconPrimitive::Dot {
                            center: [x, 8.0],
                            radius: 1.5 * s,
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
/// Names exactly the inputs the pixels depend on: [`tab_icon_primitives`] is a
/// `const fn` of the kind, and [`status_primitives`] reads only the three status
/// bools (pinned by `memoized_strip_glyphs_match_a_fresh_raster_for_every_key`).
/// The variants keep the `cols` split (`ICON_COLS` vs 1) structural; both
/// renderers' image-cache keys embed `cols * cell_w`.
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
/// raster: both renderers decode through `aterm_render::ImageCache`, which keys
/// a decode on the source `Arc`'s address, and placements key on `Arc::as_ptr`
/// for the `image_plane` reuse fast path. Nothing mutates through these `Arc`s or branches on their count.
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
/// caret on the character at `cursor`.
///
/// The same machine as [`crate::find_bar`]'s well: the caret costs no cell, the
/// view scrolls the minimum needed to keep the caret inside, and `‹`/`›` mark
/// clipped edges without overwriting the caret. An empty field shows the fallback
/// label, dimmed.
///
/// `underline` keeps the active chip's accent seam under the field while editing.
///
/// Characters go through [`strip_char`] and each keeps at least one display cell
/// ([`strip_char_cells`]); the caret is placed by char index into that projection,
/// so wide or non-BMP characters cannot desync it from the stored byte offset.
///
/// Returns the caret's painted column, or `None` on degenerate geometry. The caller
/// anchors the OS IME candidate window there.
fn paint_rename_field(
    row: &mut [RenderCell],
    field: std::ops::Range<usize>,
    text: &str,
    cursor: usize,
    placeholder: &str,
    theme: Theme,
    underline: Option<[u8; 3]>,
) -> Option<usize> {
    let width = field.len();
    if width == 0 || field.end > row.len() {
        return None;
    }
    let c = crate::chrome_band::band_colors(theme);
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
        // An empty field means "fall back down the ladder". Show WHAT it falls back
        // to, dimmed, after the caret — never as content, so Return still clears.
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

/// Blank cells between the SOLO band's title and its description.
const SOLO_GAP_COLS: usize = 3;
/// Cells kept clear at EACH end of the solo band, so the centred group never
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
/// - Every label is first cut head-first ([`truncate_title`]). A cut title sheds
///   its own state clause first ([`cut_core`]); a title that fits is shown whole.
/// - Cut tabs cluster when their common prefix is at least half the smaller label
///   window. A cluster sheds its common suffix (snapped to a word boundary,
///   [`snap_shed`]) and each member's state clause, then cuts at the last word
///   boundary inside the common head when the rest fits (`…~/aterm`), else a plain
///   tail cut. Clusters flip together, so one strip speaks one dialect.
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
///   re-cuts it; only twins still take an ordinal ([`path_ordinal_label`]).
fn distinct_chip_labels(
    segments: &[TabSegment],
    titles: &[String],
    metadata: Option<&[TabStripMetadata]>,
    active: usize,
    renaming: Option<usize>,
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
    // Every tab's subject (title less its state clause), seated or not: what a
    // path chip's cut is told apart from ([`path_chip_label`]).
    let subjects: Vec<&str> = titles.iter().map(|title| cut_core(title, 0)).collect();
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
        let avail = usize::from(layout.title_end.saturating_sub(layout.title_start));
        let raw = titles[i].as_str();
        let over = strip_display_cells(raw) > avail;
        // Every cut tab sheds its own state clause here, including on a pressured
        // strip with no cluster, which the cluster arm and the flip never reach.
        // A title with no separator sheds nothing, and a title that fits is shown
        // whole.
        let source = if over { cut_core(raw, 0) } else { raw };
        // A path keeps its end: its head is ancestry every tab shares. The path's
        // own cut is the same whichever branch labels it, so its chip holds still.
        path_cut[i] = over && is_path_subject(source);
        let path = (path_cut[i] && strip_display_cells(source) > avail)
            .then(|| path_chip_label(i, &subjects, avail));
        labels[i] = Some(path.unwrap_or_else(|| truncate_title(source, avail)));
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
        // the whole title, so this contributes nothing there and the state shed
        // below answers.
        let mut suffix = usize::MAX;
        for window in members.windows(2) {
            suffix = suffix.min(common_suffix_bytes(&titles[window[0]], &titles[window[1]]));
        }
        // Snapped to a word boundary, so the shed takes whole words
        // ([`snap_shed`]).
        let suffix = snap_shed(&titles[i], suffix);
        // Each member also sheds its own state clause ([`state_clause_bytes`]),
        // which the pairwise shed cannot find when members are twins or differ
        // only in state. Whichever shed reaches further wins.
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
        // The other cluster members' titles: what the survivor check and the
        // ordinal's tail are measured against.
        let siblings: Vec<&str> = members
            .iter()
            .filter(|&&m| m != i)
            .map(|&m| titles[m].as_str())
            .collect();
        // Twins by subject: no cut tells this title from members with the same
        // subject (the state shed removed their only difference), so a non-active
        // twin is labelled by ordinal. Subject, not title, so the label does not
        // flip as a tab goes mid-command. The active twin keeps its subject.
        let twins = members
            .iter()
            .filter(|&&m| cut_core(&titles[m], suffix) == core)
            .count();
        // A family made only of twins has no sibling ending to be told from, so it
        // paints context ([`twin_tail`]). A mixed family keeps the tail dialect
        // ([`family_tail`]), where a twin's ending distinguishes it.
        let family_of_twins = twins == members.len();
        let tail_cut: fn(&str, &str, usize) -> String = if family_of_twins {
            twin_tail
        } else {
            family_tail
        };
        let label = if twins >= 2 && i != active {
            if path_cut[i] && !family_of_twins {
                // A path twin among other members carries the path's own cut
                // behind its ordinal; the family cut would split the directory
                // name.
                path_ordinal_label(i, &subjects, avail)
            } else {
                ordinal_chip_label(i, &titles[i], core, remainder, avail, &siblings, tail_cut)
            }
        } else if path_cut[i] {
            // A path keeps the label its first cut gave it ([`path_chip_label`]),
            // independent of which neighbours are cut or seated. Twins by subject
            // still take their ordinal, above.
            labels[i].clone().unwrap_or_default()
        } else {
            furniture_survivor_recut(
                &titles[i],
                &siblings,
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
    // A cluster that relabelled in the TAIL dialect: every cluster but a family
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
            // A loner can share its ending without sharing a head (a composed
            // ` · Ready` on every idle chip). Shed the longest tail this loner
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
            // Also shed its own state clause unconditionally ([`cut_core`]): a
            // loner mid-work shares no ending, so the pairwise shed finds nothing,
            // and the state clause distinguishes nothing.
            let core = cut_core(&titles[i], suffix);
            let mut cand = truncate_title_tail(core, avail);
            // Survivor check: a word-boundary snap can move the cut past the
            // distinguishing characters, leaving only shared furniture
            // (`…· Ready`). Then shed the furniture and re-cut.
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
/// The cluster sheds the suffix shared by every member, so one member in a
/// different state collapses it and the cut can keep the furniture (`…· Ready`).
/// When the painted text is only the tail this title shares with a sibling, shed
/// that suffix and cut again; an empty re-cut is declined.
///
/// `siblings` are the cluster's other titles. Byte-identical twins are skipped
/// (`n < title.len()`): they share everything and get an ordinal instead. Callers
/// have already shed the state clause ([`state_clause_bytes`]).
fn furniture_survivor_recut(
    title: &str,
    siblings: &[&str],
    budget: usize,
    label: String,
) -> String {
    let painted = label.trim_start_matches('…').trim_start().to_string();
    if painted.is_empty() {
        return label;
    }
    let pair = siblings
        .iter()
        .map(|other| common_suffix_bytes(title, other))
        .filter(|&n| n > 0 && n < title.len())
        .max()
        .unwrap_or(0);
    if pair > 0 && title[title.len() - pair..].trim_start().ends_with(&painted) {
        let shed = &title[..title.len() - pair];
        let recut = truncate_title_tail(shed, budget);
        if !recut.trim_start_matches('…').trim().is_empty() {
            return recut;
        }
    }
    label
}

/// The label a twin paints: its 1-based position, with a tail of the title when
/// the window affords one (`2 · …oml`), bare (`2`) when not. `core`/`remainder`
/// are the cluster's suffix-shed title and word-boundary tail; `tail_cut` is
/// [`family_tail`] for a twin among other members or [`twin_tail`] for a family of
/// twins. `siblings` feed the same [`furniture_survivor_recut`] rule the family
/// cut uses.
///
/// The number is the tab's position in the window's tab order, not among painted
/// chips, so it stays put while a windowed strip ([`STRIP_SEATS_FLOOR`]) pages,
/// and it is the number `switch_tab_<n>` takes for the first nine.
/// [`crate::keybinding::Action::parse`] accepts `switch_tab_1`..`switch_tab_9` on
/// every platform, but `Keybindings::PLATFORM_DEFAULT_PAIRS` seeds no default
/// chord, and `app_input::HARDCODED_SUPER_CHORDS` is false on Linux. On Windows the
/// hardcoded Win+digit arms exist. macOS paints no band
/// ([`STRIP_IS_CHROME_BAND`]), so these ordinals never appear there.
///
/// When even the digits do not fit `avail`, the label is the bare `…`: a clipped
/// ordinal would name the wrong tab and a title cut would be a meaningless shared
/// stub. That happens for a one-cell window with ten or more tabs, or a two-cell
/// one with a hundred (`every_strip_width_labels_a_twin_honestly`).
fn ordinal_chip_label(
    tab: usize,
    title: &str,
    core: &str,
    remainder: &str,
    avail: usize,
    siblings: &[&str],
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
    // ` · ` — the separator composed titles already use — costs 3 cells; a
    // tail below 2 cells (one char and its mark) says nothing worth the space.
    let room = avail.saturating_sub(digit_cells + 3);
    if room >= 2 {
        let tail = tail_cut(core, remainder, room);
        let tail = furniture_survivor_recut(title, siblings, room, tail);
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
/// and alone on the active twin. The family cut ([`family_tail`]) where it keeps a
/// directory's name whole; where it would split a path component:
///
/// 1. the path's trailing components, whole, behind the mark, as many as fit
///    (`…work\api`, then `…api`);
/// 2. else the head of `core`, cut by [`truncate_title`] (`~\at…`).
///
/// A directory's name is what identifies the place, so it is never shown as a
/// fragment (`tabs_that_differ_only_in_state_are_told_apart_by_position`). Both
/// `/` and `\` separate components. Only paths get this: a subject with no
/// separator keeps its family cut (`…toml`). The head is `core`'s, so a prompt
/// head (`user@host: `) is never dropped without its mark.
fn twin_tail(core: &str, remainder: &str, room: usize) -> String {
    let family = family_tail(core, remainder, room);
    let subject = if remainder.is_empty() {
        core
    } else {
        remainder
    };
    if !subject.contains(['/', '\\']) || !splits_a_component(core, &family) {
        return family;
    }
    // The FIRST separator leaves the longest tail, so this finds the most
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
/// computed for it or its own composed state clause, whichever reaches further
/// ([`state_clause_bytes`], [`distinct_chip_labels`]).
///
/// Every tail cut goes through here (the cluster arm with the family's common
/// suffix, the one-dialect flip with the loner's shared furniture), so a chip's
/// label does not depend on which path reached it. A tail cut of the raw title
/// would spend the window on the state clause.
///
/// `shared` is the pairwise suffix in bytes. When it covers the whole title
/// (byte-identical twins) it is ignored and the state clause answers alone.
fn cut_core(title: &str, shared: usize) -> &str {
    let pairwise = if shared >= title.len() { 0 } else { shared };
    let shed = pairwise.max(state_clause_bytes(title));
    &title[..title.len() - shed]
}

/// The characters a label may be cut AFTER without splitting a name: the two
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
/// `subjects` are every tab's subject in the window ([`cut_core`]); `tab` indexes
/// them.
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

/// The paths a PATH chip's cut is told apart from: every subject in the window
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

/// Byte length of a composed title's trailing state clause, separator included, or
/// `0` when there is none a cut may shed.
///
/// `App::tab_titles` joins the session title and its activity summary with
/// [`crate::title_summary::TAB_LABEL_SEPARATOR`], and `compose_parts` puts the half
/// `tab_title_format` ranks second after it. Shedding the last clause keeps the
/// half the user asked to lead with. The state repeats across idle tabs and is
/// already shown by the status marks ([`TabStripMetadata::has_status`]), while
/// the tail dialect would otherwise keep exactly it.
///
/// `0` for a title with no separator, an empty clause, or a clause that is the
/// whole title (a shed must leave a subject).
///
/// `pub(crate)` for `title_summary`: its test splits a composed label back through
/// this in both title formats, and `title_summary::whole_label` splits a strip
/// label on this seam to cap each half.
pub(crate) fn state_clause_bytes(title: &str) -> usize {
    let Some(at) = title.rfind(crate::title_summary::TAB_LABEL_SEPARATOR) else {
        return 0;
    };
    // `rfind` with a `&str` pattern lands on a char boundary, and both halves
    // have to say something for the split to be a shed rather than a deletion.
    if title[..at].trim().is_empty()
        || title[at + crate::title_summary::TAB_LABEL_SEPARATOR.len()..]
            .trim()
            .is_empty()
    {
        return 0;
    }
    title.len() - at
}

/// Byte length of the longest common PREFIX of `a` and `b`, aligned to char
/// boundaries — [`common_suffix_bytes`]'s mirror.
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
/// (Windows and Linux).
///
/// The cell painter can only draw in the terminal's monospace face, and on
/// Windows and Linux the in-grid strip is the window's only chrome (macOS uses the
/// native toolbar). `RenderCell` has no face selector, so the band's text and
/// surfaces become pixels instead: band fill, rounded cards, hover wash and the `+`
/// button, measured in px rather than whole cells. The canvas is the full optical
/// band (`band_top_px` of chrome lip plus the strip's cell rows), reached through
/// the inline-image seam's chrome-band lift
/// ([`aterm_core::grid::extra::ImageData::band_lift_px`]), so cards centre in the
/// band rather than in the last cell row.
///
/// [`raster_band`] rasterizes the strip into one transparent straight-alpha RGBA
/// image with the settings tray's `DrawPrim` rasterizer (whose `TextFace::Ui` arm
/// resolves Segoe UI Variable / Segoe UI with per-char fallback into the terminal
/// cascade) and returns per-row [`ImageRef`] slices the splice lays over the strip.
/// It rides the existing inline-image seam (`RawRgba8` + `Arc`, shared with icon
/// marks, sixel and Kitty images): a `z >= 0` image draws over the cell's glyph and
/// under line decorations, so the seam hairline still paints over it.
///
/// Not a `FreeSprite` band (its atlas is owned per frame by the effects engine),
/// not a renderer face selector (a `RenderCell` contract change), and not per-chip
/// images (one band image is one GPU cache entry).
///
/// `layout_segments` stays the geometry authority and the cell painter still paints
/// every cell; this converts the same segments to pixels via `cell_w`, so paint and
/// hit-testing agree. Mono ink is suppressed only by image coverage, so a segment
/// this painter cannot render falls back to the cell paint by not being covered:
///   * the inline rename field (the find bar's cell-based machinery);
///   * a solo title band (macOS-only policy; kept total here);
///   * any title with a char neither the UI face nor the chrome cascade covers
///     (colour emoji, CJK beyond the cascade), which the terminal renderer paints.
///
/// The label is centred in the band the viewer sees
/// (`pad_top + head + strip_rows·cell_h`) from live metrics, so the synthetic
/// head (`App::synthetic_strip_head_px`, config `tab_band_height`) needs nothing
/// here: `head` is inside `band_top_px`, the band cache key and `ChromeBleed`'s
/// `[0, grid_top)` fill, which continues each column's background
/// (`top_extends_cells`). At 96 dpi the band is 32 px (head 11); at 150%, 48 px.
///
/// The label centres as high as the raster can reach; see the clamp in
/// `raster_band`.
#[cfg(any(windows, target_os = "linux"))]
pub(crate) mod pixel_band {
    use super::*;
    use crate::type_scale::TypeStep;
    use crate::widget::{DrawPrim, TextFace, TextWeight, rgba, text_prim};

    /// The band label's logical size: the settings tray's chrome base (13 px at
    /// 96 dpi ≈ 10 pt), not the terminal `font_px`. Scaled by the window's scale
    /// factor and capped against the band height.
    const STRIP_LABEL_LOGICAL_PX: f32 = 13.0;

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

    /// Device-pixel geometry of the strip band for ONE window (mixed-DPI: every
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
        /// Band pixels above the grid (`pad_top + head`): the lip `ChromeBleed`
        /// paints in the band tone. Part of the optical band and of the canvas
        /// (via the chrome-band lift).
        pub band_top_px: usize,
        /// The window's scale factor (drives the label size).
        pub scale: f32,
        /// Canvas y (device px from the optical band's top) of the seam underline's
        /// top row: `band_top_px + (strip_rows − 1)·cell_h + DecoMetrics::underline_y`,
        /// resolved through the renderer's deco law (`Backend::deco_metrics_for`).
        /// Cards are centred in `[0, seam_top_px)` and kept clear of the rule;
        /// `None` (no resolved deco law, test fixtures) clears a hairline off the
        /// canvas bottom instead.
        pub seam_top_px: Option<usize>,
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
        pub theme: Theme,
        pub active_override: Option<[u8; 3]>,
        pub geometry: BandGeometry,
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
        // The band's ONE reading of the selection ([`BandInput::selected`]) —
        // resolved at the entry, before anything paints or measures.
        let active = input.selected();
        let colors = strip_colors_with_active(input.theme, input.active_override);
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
        // midline, so ✕ / + / icons and the text read as ONE centred row).
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
                // The status-mark cell ([`TabHit::Connector`]) has no band design
                // yet, so those columns go to the cell painter, like solo.
                TabHit::Connector(_) => {
                    fallback.push((seg.start_col, end));
                }
                TabHit::Select(i) if seg.solo => {
                    // SOLO is macOS policy ([`SOLO_TITLE_BAND`]) — structurally
                    // unreachable on Windows, kept total: the cell painter owns it.
                    let _ = i;
                    fallback.push((seg.start_col, end));
                }
                TabHit::Select(i) => {
                    if input.paint.rename.is_some_and(|edit| edit.tab == i) {
                        // The inline rename WELL (field, caret, ‹› markers, IME
                        // preedit splice) stays the shipped cell machinery.
                        fallback.push((seg.start_col, end));
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
                    if !crate::tray_raster::strip_band_run_coverable(&label) {
                        fallback.push((seg.start_col, end));
                        continue;
                    }
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
                    }
                    // Inks follow the surface each label sits on (selected card,
                    // hover wash, quiet chip, or bare band). Semibold only where the
                    // design says the selection earns it.
                    let (ink, face) = {
                        let active_face = if design.active_semibold {
                            TextFace::UiBold
                        } else {
                            TextFace::Ui
                        };
                        if is_active {
                            (colors.active_fg, active_face)
                        } else if is_hovered {
                            (colors.hover_fg, TextFace::Ui)
                        } else if design.quiet_cards {
                            (colors.chip_fg, TextFace::Ui)
                        } else {
                            (colors.inactive_fg, TextFace::Ui)
                        }
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
                        let color = if item.dirty || item.attention >= ChipLevel::Wait {
                            colors.accent
                        } else {
                            // One ink per surface (see the icon above).
                            ink
                        };
                        draw_icon_prims(
                            &mut prims,
                            &status_primitives(item),
                            (f32::from(col) + 0.5) * cw,
                            cy,
                            (cw * 1.2).min(band_h * 0.8).max(5.0),
                            color,
                        );
                    }
                    // The title, clipped to its span so it never bleeds into the
                    // status canvas, the ✕ or the next chip. The Windows lane keeps
                    // left alignment; the design lane centres the run in its span.
                    let x0 = f32::from(layout.title_start) * cw;
                    let span_px = f32::from(avail) * cw - 1.0;
                    if span_px > 1.0 && !label.trim().is_empty() {
                        // The active label through the wght axis when the host has a
                        // variable UI face and the lane wants semibold. The whole
                        // label must shape on that face first; any char its cmap
                        // lacks sends the whole run to the static `UiBold` cascade
                        // below (per-char fallback), never a mixed-weight word or a
                        // fit that drops the uncoverable char. Only a run that
                        // shaped whole is fitted, at the glyph level.
                        let varied = if is_active && wants_semibold {
                            variable_pen.as_ref().and_then(|pen| {
                                let run = pen.shape(&label, label_px)?;
                                let run = pen.fit(run, span_px, label_px)?;
                                let x = ((span_px - run.width) * 0.5).max(0.0) + x0;
                                Some(variable_label_overlay(
                                    &run, x, baseline, img_h, span_px, ink,
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
                                // Centred by the same measure the pen draws with.
                                let width =
                                    crate::tray_raster::ui_text_width_for(face, &label, label_px);
                                let x = ((span_px - width) * 0.5).max(0.0) + x0;
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
                        let side = card_h.min(seg1 - seg0 - 2.0 * design.gap_h);
                        let hot = input.paint.new_tab_hovered;
                        if design.quiet_button && side > 4.0 {
                            prims.push(DrawPrim::Panel {
                                x: center - side * 0.5,
                                y: design.card_top + (card_h - side) * 0.5,
                                w: side,
                                h: side,
                                radius: design.radius.min(side * 0.5),
                                fill: rgba(if hot { colors.hover_bg } else { colors.chip_bg }, 255),
                            });
                        }
                        let ink = match (design.quiet_button, hot) {
                            (true, false) => colors.chip_button_fg,
                            (true, true) => colors.button_hover_fg,
                            (false, _) => colors.fg,
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
                    // Centred on the CARD just painted (the full segment span,
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
                // The trailing VERSION LABEL, in the UI face at the chip
                // titles' size and on their baseline, right-aligned `pad` px
                // inside the slot — the SAME face, size and pad
                // [`version_slot_cols`] measured the slot with, so the label
                // fills the hit region it was given. Quiet at rest (bare band,
                // the inactive ink — strip content, not a tab and not a
                // caption button); the band's hover card under the pointer;
                // the update CTA's card while an update is staged — each
                // state's card and ink are [`version_label_material`]'s. No
                // animation. A label the UI face and the chrome cascade cannot
                // cover goes to the cell painter, like any such title.
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
                TabHit::Close(_) => {}
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
            // Geometry above is already in DEVICE px (converted through the
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
            // OVER the covered cells' (mono) glyphs, under the decorations —
            // the suppression contract the whole design keys on.
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

    /// The label size for a `band_h`-px optical band at `scale`: the logical size
    /// scaled, capped against the band, floored at a legible minimum.
    fn band_label_px(band_h: f32, scale: f32) -> f32 {
        (STRIP_LABEL_LOGICAL_PX * scale)
            .round()
            .min((band_h * 0.62).floor())
            .max(6.0)
    }

    /// The trailing VERSION LABEL's slot, in columns, for each rung — measured,
    /// not guessed: the label in the band's own UI face at the chip titles' size
    /// ([`band_label_px`] for this `band_h`-px optical band at `scale`, normalised
    /// exactly as [`raster_band`] normalises it), plus [`version_pad_px`] on each
    /// side, rounded UP to whole cells of `cell_w` px. The painter draws with the
    /// same face, size and pad, so the hit region and the painted label are the
    /// same columns.
    ///
    /// [`VersionSlotCols::NONE`] until the UI face is installed (or with no cell
    /// width): the first frames keep today's strip rather than lay out a slot
    /// measured in a face that is not there yet. The chips still narrow ONCE
    /// when the face lands and the label takes its columns (at 132 columns two
    /// 64-column chips give up the label's columns between them) — on the same
    /// frame the strip already switches
    /// from the cell lane to the pixel band, so it is one change, not two.
    ///
    /// MEASURED (2026-10-01, a release build of 25a5963a4 plus this change — a
    /// dirty tree, so the labels read `v0.101.1576+g25a5963* · DEV` and
    /// `v0.101.1576* · DEV` — Windows 11, the UI face Segoe UI Variable, read off
    /// `aterm ctl chrome`'s `strip-version … cols=`): at 96 dpi (cell 9×21 px,
    /// label 13 px) full = 20 columns, short = 13; at 150% (`--scale 1.5`, cell
    /// 14×30 px, label 20 px) full = 20, short = 13. The layout tests reason in
    /// symbolic widths (22/13) instead, because their rule is how the strip
    /// yields, not one face's advances.
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

    /// The version label's inset from each end of its slot, in device px: 8
    /// logical px. A design choice, not a measurement — it leaves the text
    /// `8 − gap_h` px (5 at 96 dpi) inside the hover card, about the 6 px a chip
    /// title sits inside its own card (one cell less the card's inset).
    fn version_pad_px(scale: f32) -> f32 {
        (8.0 * scale).round()
    }

    /// The band's resolved metrics — every px the card row is drawn from,
    /// derived once per raster. All values
    /// are canvas coordinates (0 = the window's top edge; the canvas carries the
    /// chrome lip via the image lift).
    struct BandDesign {
        /// Cap-centred baseline for labels, in the content region.
        baseline: f32,
        /// Top edge of the card row.
        card_top: f32,
        /// Bottom edge of the card row — clear of the seam rule the cell lane
        /// stamps over this raster ([`BandGeometry::seam_top_px`]).
        card_bot: f32,
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
            // The content region: the canvas down to the seam rule (renderer's deco
            // law), or a hairline off the canvas bottom without a resolved seam.
            let content_h = geometry
                .seam_top_px
                .map(|y| y as f32)
                .unwrap_or_else(|| img_h as f32 - (2.0 * s).round())
                .clamp(label_px, img_h as f32);
            let baseline = crate::tray_raster::row_baseline(0.0, content_h, label_px).clamp(
                label_px * 0.9,
                (img_h as f32 - label_px * 0.28).max(label_px * 0.9),
            );
            // The card row: ~3.5 logical px of band above and below the cards,
            // centred in the content region. On a compact band the inset shrinks
            // first, down to a hairline of interior, before `band_label_px`'s cap
            // shrinks the type.
            let inset_v = ((3.5 * s).round())
                .max(3.0)
                .min(content_h * 0.2)
                .min(((content_h - label_px - 2.0) * 0.5).max(1.0));
            let card_top = inset_v;
            let card_bot = (content_h - inset_v).max(card_top + 1.0);
            let card_h = card_bot - card_top;
            // Corners round like a native tab's (6 logical px), never past a
            // pill's semicircle.
            let radius = (6.0 * s).round().min(card_h * 0.5);
            // Half the inter-card gutter (6 logical px between neighbours).
            let gap_h = (3.0 * s).round().max(2.0);
            BandDesign {
                baseline,
                card_top,
                card_bot,
                radius,
                gap_h,
                // The bake-off's verdict, hardwired — see each field's doc.
                quiet_cards: true,
                quiet_button: true,
                active_semibold: true,
            }
        }
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
    /// The cell pass proves distinctness in cell columns; [`fit_label`] then cuts
    /// again against the pixel span, which done per chip can collapse distinct
    /// labels (`…~/aterm` and `…$HOME/trust` both to `…~…`). So the fit runs here over
    /// the whole strip, and a collision it creates is re-cut by ordinal
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
        let mut cell = if STRIP_DISTINCT_LABELS {
            distinct_chip_labels(
                input.segments,
                input.titles,
                Some(input.metadata),
                active,
                input.paint.rename.map(|edit| edit.tab),
            )
        } else {
            Vec::new()
        };
        let mut resolved: Vec<Option<String>> = vec![None; input.titles.len()];
        let mut entries: Vec<BandLabel> = Vec::new();
        for seg in input.segments {
            let end = seg.end_col.min(cols as u16);
            let TabHit::Select(i) = seg.kind else {
                continue;
            };
            // Every skip the paint loop makes, made here too: a segment it
            // hands to the cell painter has no pixel label to fit.
            if seg.start_col >= end
                || seg.solo
                || i >= input.titles.len()
                || input.paint.rename.is_some_and(|edit| edit.tab == i)
            {
                continue;
            }
            let item = input.metadata.get(i).copied();
            let layout = band_content_layout(seg, item, end);
            let avail = layout.title_end.saturating_sub(layout.title_start);
            let raw = input.titles[i].as_str();
            let text: String = cell
                .get_mut(i)
                .and_then(Option::take)
                .unwrap_or_else(|| truncate_title(raw, usize::from(avail)))
                .chars()
                .map(strip_char)
                .collect();
            if !crate::tray_raster::strip_band_run_coverable(&text) {
                // The cell painter's segment: hand the paint loop the string it
                // checked coverage with, so it takes the same fallback branch.
                resolved[i] = Some(text);
                continue;
            }
            entries.push(BandLabel {
                tab: i,
                span_px: f32::from(avail) * cw - 1.0,
                text,
            });
        }
        // Measure with the face each label will be drawn with. The active label may
        // also go through [`VariablePen::fit`] (same candidates, marginally
        // different advances); the repair rewrites it only when the fit left it
        // naming nothing.
        let measure = |tab: usize, s: &str| {
            let face = if tab == active && wants_semibold {
                TextFace::UiBold
            } else {
                TextFace::Ui
            };
            crate::tray_raster::ui_text_width_for(face, s, label_px)
        };
        let taken: Vec<&str> = resolved.iter().filter_map(Option::as_deref).collect();
        for (tab, text) in fit_labels_distinctly(&entries, active, &taken, &measure) {
            resolved[tab] = Some(text);
        }
        resolved
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
    /// [`fit_labels_distinctly`]) and its ordinal fallback call it; the paint loop
    /// never does.
    ///
    /// Only a trailing mark is dropped, so a tail cut (`…and`) can shrink to `……`;
    /// the repair detects that ([`label_says_nothing`]).
    fn fit_label(label: String, span_px: f32, measure: impl Fn(&str) -> f32) -> String {
        if measure(&label) <= span_px {
            return label;
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
                let gid = self.vf.cmap.lookup_glyph_index(ch);
                if gid == 0 {
                    return None;
                }
                if let Some(previous) = prev {
                    pen += self
                        .vf
                        .cmap
                        .horizontal_kern(previous, ch, px)
                        .unwrap_or(0.0);
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
                        last.pen
                            + self
                                .vf
                                .cmap
                                .horizontal_kern(last.ch, '…', px)
                                .unwrap_or(0.0),
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
    /// most `span_px` wide, inked in `color` on a transparent ground.
    fn variable_label_overlay(
        run: &ShapedRun,
        x: f32,
        baseline: f32,
        height: usize,
        span_px: f32,
        color: [u8; 3],
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
        LabelOverlay {
            x: x.max(0.0) as usize,
            width,
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
                // The connector arrow. The band has no polygon fill, so the
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
                // These tests assert lane-specific pixels; the design lane's seam
                // fixtures pass a resolved value.
                seam_top_px: None,
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
                    drop_target: false,
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
                theme: Theme::default(),
                active_override: None,
                geometry,
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

        /// [`ink_bbox`] for the opaque design-lane canvas: a pixel is ink when its
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

        /// Install the host UI faces for this thread, or `None` when the host has no
        /// system UI face; the band then declines and the test asserts nothing.
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

        /// THE SLOT IS THE LABEL'S MEASURED WIDTH, ROUNDED UP TO CELLS: the
        /// label in the UI face at the chip titles' size plus its pad on each
        /// side fits in `w` cells and does not fit in `w - 1` — at every scale,
        /// for both rungs, with and without the update mark. Before the face
        /// lands (or with no cell width) there is no slot at all.
        #[test]
        fn version_slot_cols_cover_the_painted_width() {
            use crate::menu::{VersionRung, strip_version_label};
            if !with_ui_faces() {
                return;
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
                    assert!(slot.short < slot.full, "the short rung is shorter");
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

        /// THE BAND DRAWS THE LABEL: it covers the slot's columns, and the ink is
        /// right-aligned `pad` px inside the slot's right edge, on the chip
        /// titles' row.
        #[test]
        fn the_band_draws_the_version_label_in_the_ui_face() {
            if !with_ui_faces() {
                return;
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
            let (x0, y0, x1, y1) =
                ink_bbox_off_surfaces(&rgba, w, h, seg0, seg1, &[colors.band_bg])
                    .expect("the label inks its slot");
            let pad = version_pad_px(1.0) as usize;
            assert!(
                x0 >= seg0 + pad - 1,
                "ink starts inside the left pad: {x0} vs {seg0}"
            );
            assert!(
                x1 > (seg0 + seg1) / 2,
                "the run reaches the slot's right half"
            );
            assert!(
                (seg1 - pad).abs_diff(x1) <= 3,
                "right-aligned {pad} px inside the slot: ink ends at {x1}, slot at {seg1}"
            );
            // On the chip titles' row: the label's ink centre sits inside the
            // first chip title's ink rows.
            let chip = segments[0];
            let (_, ty0, _, ty1) = ink_bbox_off_surfaces(
                &rgba,
                w,
                h,
                usize::from(chip.start_col) * CELL_W,
                usize::from(chip.end_col) * CELL_W,
                &[colors.band_bg, colors.active_bg, colors.chip_bg],
            )
            .expect("the chip title inks");
            let mid = (y0 + y1) / 2;
            assert!(
                (ty0..ty1).contains(&mid),
                "label ink {y0}..{y1} vs chip title ink {ty0}..{ty1}"
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

        /// UNDER THE POINTER the label sits on the band's one hover material —
        /// the card a hovered chip and the hovered `+` wear — and at rest on bare
        /// band.
        #[test]
        fn version_hover_takes_the_band_hover_material() {
            if !with_ui_faces() {
                return;
            }
            let (segments, metadata, label) = version_strip(132, false);
            let titles = vec!["one".to_string(), "two".to_string()];
            let colors = strip_colors_with_active(Theme::default(), None);
            for (hovered, want) in [(false, colors.band_bg), (true, colors.hover_bg)] {
                let paint = StripPaint {
                    version_hovered: hovered,
                    ..StripPaint::default()
                };
                let input = band(&segments, &titles, &metadata, paint, geometry(132, 1));
                let rows = raster_band(&input, &[]).expect("band");
                let (rgba, w, h) = image_of(&rows);
                // Inside the card, left of the text's pad: card surface only.
                let x = usize::from(label.start_col) * CELL_W + 5;
                let got = pixel(&rgba, w, x, h / 2);
                assert!(near(got, want), "hovered {hovered}: {got:?} vs {want:?}");
            }
            crate::tray_raster::clear_ui_fonts_for_test();
        }

        /// UNDER ATTENTION (an update staged) the label wears the update CTA's
        /// card — the `↻`'s own surface — and ends ` ↑`.
        #[test]
        fn version_attention_wears_the_update_cta() {
            if !with_ui_faces() {
                return;
            }
            let (segments, metadata, label) = version_strip(132, true);
            assert!(
                matches!(
                    label.kind,
                    TabHit::Version {
                        attention: true,
                        ..
                    }
                ),
                "the slot carries the mark into the layout"
            );
            let text = crate::menu::strip_version_label(crate::menu::VersionRung::Full, true)
                .expect("a full rung");
            assert!(text.ends_with(" \u{2191}"), "{text}");
            assert!(
                crate::tray_raster::strip_band_run_coverable(&text),
                "the UI face covers the plain arrow"
            );
            let titles = vec!["one".to_string(), "two".to_string()];
            let input = band(
                &segments,
                &titles,
                &metadata,
                StripPaint::default(),
                geometry(132, 1),
            );
            let rows = raster_band(&input, &[]).expect("band");
            let (rgba, w, h) = image_of(&rows);
            let colors = strip_colors_with_active(Theme::default(), None);
            let x = usize::from(label.start_col) * CELL_W + 5;
            let got = pixel(&rgba, w, x, h / 2);
            assert!(
                near(got, colors.update_bg),
                "{got:?} vs {:?}",
                colors.update_bg
            );
            crate::tray_raster::clear_ui_fonts_for_test();
        }

        #[test]
        fn covers_every_column_with_one_band_image_of_the_strip_footprint() {
            if !with_ui_faces() {
                return;
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
            let (_, w, h) = image_of(&rows);
            // The canvas is the cell footprint PLUS the chrome lip, carried as
            // the image's band lift so the renderers seat it at the window top.
            let lift = BAND_TOP;
            assert_eq!((w, h), (80 * CELL_W, CELL_H + lift), "canvas vs footprint");
            assert_eq!((first.cols, first.rows, first.z_index), (80, 1, 0));
            assert_eq!(usize::from(first.band_lift_px), lift);
            crate::tray_raster::clear_ui_fonts_for_test();
        }

        /// The window's edge marks share the optical centre of every other piece of
        /// strip furniture, not the cell centre half a lip lower.
        ///
        /// Twenty tabs with the selection on an interior page: eighty columns seat
        /// six chips, paging twenty tabs into four fives ([`strip_window`]), and
        /// tab 7 is on the second, so both edges are marked.
        #[test]
        fn the_overflow_marks_share_the_strip_furniture_centre() {
            if !with_ui_faces() {
                return;
            }
            let metadata = plain(20);
            let segments = layout_segments_with_metadata(80, 20, &metadata, 7, false);
            let titles: Vec<String> = (0..20).map(|i| format!("job-{i}")).collect();
            let marks = strip_overflow(&segments, titles.len());
            assert_eq!(
                (marks.left.is_some(), marks.right.is_some()),
                (true, true),
                "twenty tabs on eighty columns, selected on an interior page: \
                 both edges clip"
            );
            let mut input = band(
                &segments,
                &titles,
                &metadata,
                StripPaint::default(),
                geometry(80, 1),
            );
            input.active = 7;
            let rows = raster_band(&input, &[]).expect("UI faces installed ⇒ a band");
            let (rgba, w, h) = image_of(&rows);
            let colors = strip_colors_with_active(Theme::default(), None);
            let surfaces = [colors.band_bg, colors.active_bg, colors.chip_bg];
            let ink_mid_y = |px0: usize, px1: usize, what: &str| {
                let (_, y0, _, y1) = ink_bbox_off_surfaces(&rgba, w, h, px0, px1, &surfaces)
                    .unwrap_or_else(|| {
                        panic!(
                            "{what} leaves no ink on the band — a mark handed to \
                             the cell painter is drawn on the CELL baseline, \
                             which is the very placement this asserts against"
                        )
                    });
                (y0 + y1) as f32 / 2.0
            };
            // The `+` glyph: code-native strokes on `cy`, the reference the marks
            // are judged against.
            let plus = segments
                .iter()
                .find(|seg| matches!(seg.kind, TabHit::NewTab))
                .expect("the strip ends with a `+`");
            let plus_mid = ink_mid_y(
                usize::from(plus.start_col) * CELL_W,
                usize::from(plus.end_col) * CELL_W,
                "the `+`",
            );
            for (col, mark) in marks.columns() {
                let mid = ink_mid_y(
                    usize::from(col) * CELL_W,
                    usize::from(col + 1) * CELL_W,
                    "the overflow mark",
                );
                assert!(
                    (mid - plus_mid).abs() <= 1.0,
                    "{mark:?} at column {col} inks around y {mid}, the `+` around \
                     y {plus_mid} — the mark is off the strip's line"
                );
            }
            // …and it is the BAND that owns those columns now: a punched-through
            // column would paint the mark on the cell baseline again.
            let covered: Vec<usize> = rows[0].iter().map(|(c, _)| *c).collect();
            assert_eq!(
                covered,
                (0..80).collect::<Vec<_>>(),
                "the band owns every column, marks included"
            );
            crate::tray_raster::clear_ui_fonts_for_test();
        }

        #[test]
        fn label_ink_lands_inside_its_title_span_and_is_vertically_centred() {
            if !with_ui_faces() {
                return;
            }
            let metadata = plain(2);
            let segments = layout_segments_with_metadata(80, 2, &metadata, 0, false);
            let titles = vec!["Alpha".to_string(), "Beta".to_string()];
            let input = band(
                &segments,
                &titles,
                &metadata,
                StripPaint::default(),
                geometry(80, 1),
            );
            let rows = raster_band(&input, &[]).expect("band");
            let (rgba, w, h) = image_of(&rows);
            let seg = segments[0];
            let layout = tab_content_layout(&seg, metadata[0]);
            let span = (
                usize::from(layout.title_start) * CELL_W,
                usize::from(layout.title_end) * CELL_W,
            );
            // The canvas is opaque, so classify ink as pixels far from every
            // surface tone, then hold the centred run to its span and to the
            // content region's optical centre.
            let colors = strip_colors_with_active(Theme::default(), None);
            let surfaces = [colors.band_bg, colors.active_bg, colors.chip_bg];
            let (x0, y0, x1, y1) = ink_bbox_off_surfaces(&rgba, w, h, span.0, span.1, &surfaces)
                .expect("the active title inks its span");
            assert!(
                x0 >= span.0 && x1 <= span.1,
                "ink stays in span: {x0}..{x1} vs {span:?}"
            );
            // Centred: the run's middle within a couple px of the span's.
            let span_mid = (span.0 + span.1) as f32 / 2.0;
            let ink_mid_x = (x0 + x1) as f32 / 2.0;
            assert!(
                (ink_mid_x - span_mid).abs() <= 3.0,
                "run centred in span: {ink_mid_x} vs {span_mid}"
            );
            // Optically centred in the content region (no resolved seam here, so
            // the fallback hairline clearance applies).
            let content_h = h as f32 - 2.0;
            let ink_mid_y = (y0 + y1) as f32 / 2.0;
            assert!(
                (ink_mid_y - content_h / 2.0).abs() <= 3.0,
                "ink mid {ink_mid_y} vs content centre {} (bbox y {y0}..{y1})",
                content_h / 2.0
            );
            crate::tray_raster::clear_ui_fonts_for_test();
        }

        #[test]
        fn rename_segment_is_left_uncovered_for_the_cell_well() {
            if !with_ui_faces() {
                return;
            }
            let metadata = plain(2);
            let segments = layout_segments_with_metadata(80, 2, &metadata, 0, false);
            let titles = vec!["alpha".to_string(), "beta".to_string()];
            let paint = StripPaint {
                hovered: None,
                new_tab_hovered: false,
                version_hovered: false,
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
                return;
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
            // A legacy icon raster under the fallback segment must survive; one
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
                return;
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
            // Where the pen put the label: the band centres the run in its
            // span by the very measure the pen advances with.
            let layout = tab_content_layout(&seg, metadata[1]);
            let label_px = band_label_px(h as f32, 1.0);
            let measure =
                |s: &str| crate::tray_raster::ui_text_width_for(TextFace::Ui, s, label_px);
            let x0 = f32::from(layout.title_start) * CELL_W as f32;
            let span_px = f32::from(layout.title_end - layout.title_start) * CELL_W as f32 - 1.0;
            let x = ((span_px - measure(title)) * 0.5).max(0.0) + x0;
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

        /// Until the chain face is parsed, a CJK title takes the cell lane: the
        /// coverage rung admits only faces a renderer already parsed (no parse on
        /// the UI thread), and the band re-rasters once one lands
        /// (`strip_band_font_epoch_moves_when_a_chain_face_lands`).
        #[test]
        fn a_cjk_title_falls_to_the_cell_lane_until_the_chain_face_lands() {
            // A settled cascade with NO chain face: the embedded primary alone.
            let mut renderer = aterm_render::Renderer::from_bytes(
                aterm_render::embedded_font(),
                14.0,
                Theme::default(),
            )
            .expect("embedded renderer");
            renderer.set_runtime_font_discovery(false);
            crate::tray_raster::install_settled_chrome_fonts_for_test(renderer);
            if !with_ui_faces() {
                return;
            }
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

        /// With a variable UI face (`wght` axis, e.g. Segoe UI Variable), the active
        /// label shapes through the wght-600 instance, which carries more coverage
        /// than the regular for the same string.
        #[test]
        fn active_label_instances_the_variable_semibold_when_the_host_has_one() {
            if !with_ui_faces() {
                return;
            }
            let Some(vf) = crate::tray_raster::strip_band_variable_semibold() else {
                // Static UI regular (Win10 Segoe UI, CI without SegUIVar): the
                // fontdue `UiBold` path is the contract there.
                crate::tray_raster::clear_ui_fonts_for_test();
                return;
            };
            use aterm_render::variation::WGHT_TAG;
            assert!(
                vf.coords
                    .iter()
                    .any(|&(tag, weight)| tag == WGHT_TAG && weight >= 500.0),
                "semibold coords: {:?}",
                vf.coords
            );
            let ink = |run: &ShapedRun| -> u64 {
                run.glyphs
                    .iter()
                    .flat_map(|g| g.cov.iter().map(|&a| u64::from(a)))
                    .sum()
            };
            let semibold = shape_variable_label(&vf, "Alpha", 13.0).expect("Latin shapes");
            assert!(semibold.width > 0.0 && !semibold.glyphs.is_empty());
            let mut regular = vf.clone();
            regular.coords = vec![(WGHT_TAG, 400.0)];
            let regular = shape_variable_label(&regular, "Alpha", 13.0).expect("regular shapes");
            assert!(
                ink(&semibold) > ink(&regular),
                "wght 600 must lay more ink than wght 400 ({} vs {})",
                ink(&semibold),
                ink(&regular)
            );
            // A char outside the cmap sends the whole run to the fontdue path.
            assert!(shape_variable_label(&vf, "a\u{1F680}", 13.0).is_none());
            // And the band itself still inks the active title inside its span.
            let metadata = plain(2);
            let segments = layout_segments_with_metadata(80, 2, &metadata, 0, false);
            let titles = vec!["Alpha".to_string(), "Beta".to_string()];
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
            assert!(x0 >= span.0 && x1 <= span.1);
            crate::tray_raster::clear_ui_fonts_for_test();
        }

        /// A title the chrome cascade covers but the variable face's cmap does not
        /// (`✓`, a Nerd-Font PUA icon) draws in full on the active chip through the
        /// `UiBold` per-char fallback, never as `prefix…` or a lone `…`.
        #[test]
        fn active_label_the_variable_face_cannot_shape_draws_whole_through_the_cascade() {
            if !with_ui_faces() {
                return;
            }
            let Some(vf) = crate::tray_raster::strip_band_variable_semibold() else {
                // Static UI regular: there is no variable path to regress.
                crate::tray_raster::clear_ui_fonts_for_test();
                return;
            };
            // A char outside the variable face's cmap that the chrome cascade draws
            // as real ink, probed rather than assumed, so a future face that covers
            // it makes the test skip rather than lie.
            let odd = [
                '\u{2713}', '\u{2717}', '\u{2423}', '\u{2318}', '\u{23CE}', '\u{2500}', '\u{2588}',
                '\u{2591}',
            ]
            .into_iter()
            .find(|&ch| {
                vf.cmap.lookup_glyph_index(ch) == 0
                    && crate::tray_raster::strip_band_run_coverable(&ch.to_string())
            });
            let Some(odd) = odd else {
                crate::logging::stderr_line!(
                    "no cascade-only char on this host; nothing to regress"
                );
                crate::tray_raster::clear_ui_fonts_for_test();
                return;
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
                return;
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
        /// ([`distinct_chip_labels`]) made distinct.
        #[test]
        fn the_pixel_fit_cannot_undo_the_cell_pass_distinctness() {
            let titles: Vec<String> = ["alpha", "beta", "gamma", "delta"]
                .iter()
                .map(|leaf| format!("user@m17-tower: ~/work/service-{leaf}"))
                .collect();
            let metadata = plain(titles.len());
            let segments = layout_segments_with_metadata(80, titles.len(), &metadata, 0, false);
            let cell: Vec<String> =
                distinct_chip_labels(&segments, &titles, Some(&metadata), 0, None)
                    .into_iter()
                    .map(Option::unwrap)
                    .collect();
            // Fixture guard: the cell pass produced four distinct strings.
            for (i, a) in cell.iter().enumerate() {
                for (j, b) in cell.iter().enumerate() {
                    assert!(i == j || a != b, "cell fixture: {cell:?}");
                }
            }
            let entries: Vec<BandLabel> = segments
                .iter()
                .filter_map(|seg| match seg.kind {
                    TabHit::Select(i) => Some(BandLabel {
                        tab: i,
                        span_px: window(seg).1,
                        text: cell[i].clone(),
                    }),
                    _ => None,
                })
                .collect();
            // Fixture guard: the per-chip fit does collapse them at this span.
            let naive: Vec<String> = entries
                .iter()
                .map(|entry| {
                    fit_label(entry.text.clone(), entry.span_px, |s| {
                        wide_face(entry.tab, s)
                    })
                })
                .collect();
            assert!(
                naive
                    .iter()
                    .enumerate()
                    .any(|(a, x)| naive.iter().enumerate().any(|(b, y)| a != b && x == y)),
                "fixture: the naive per-chip fit must collide, else this test \
                 proves nothing: {naive:?}"
            );
            let fitted = fit_labels_distinctly(&entries, 0, &[], &wide_face);
            assert_eq!(fitted.len(), titles.len(), "every chip resolved");
            for (n, (tab, text)) in fitted.iter().enumerate() {
                assert!(!text.is_empty(), "a chip with a window says something");
                for (m, (_, other)) in fitted.iter().enumerate() {
                    assert!(
                        n == m || text != other,
                        "tabs must stay tellable apart THROUGH the pixel fit: \
                         {fitted:?}"
                    );
                }
                // A chip is either untouched or addressed by its own strip
                // position.
                if *tab != 0 {
                    assert!(
                        *text == naive[n] || text.starts_with(&(tab + 1).to_string()),
                        "a chip the fit collapsed carries its position: {text:?}"
                    );
                }
            }
            assert_eq!(
                fitted[0].1, naive[0],
                "the ACTIVE chip is never the one re-cut"
            );
            // Every fitted string still MEASURES inside its own span — the
            // repair must not buy distinctness with a clipped label.
            for (entry, (_, text)) in entries.iter().zip(&fitted) {
                assert!(
                    wide_face(entry.tab, text) <= entry.span_px || text == "…",
                    "{text:?} overflows its {}px span",
                    entry.span_px
                );
            }
        }

        /// Labels that arrive already ordinal-ed (ten shells in one cwd) do not
        /// collapse, and no chip claims a number other than its own position.
        #[test]
        fn ten_identical_tabs_stay_addressable_through_the_pixel_fit() {
            let titles: Vec<String> = (0..10)
                .map(|_| "user@m17-tower: ~/aterm · Typing a command".to_string())
                .collect();
            let metadata = plain(titles.len());
            for cols in [80u16, 130, 200] {
                let segments =
                    layout_segments_with_metadata(cols, titles.len(), &metadata, 0, false);
                // Indexed by tab; `None` for tabs the strip's window
                // ([`STRIP_SEATS_FLOOR`]) did not seat.
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
                let fitted = fit_labels_distinctly(&entries, 0, &[], &wide_face);
                for (n, (tab, text)) in fitted.iter().enumerate() {
                    for (m, (_, other)) in fitted.iter().enumerate() {
                        assert!(
                            n == m || text.is_empty() || text != other,
                            "{cols} cols: two chips paint {text:?}"
                        );
                    }
                    if *tab != 0 && !text.is_empty() {
                        let digits = text
                            .chars()
                            .take_while(char::is_ascii_digit)
                            .collect::<String>();
                        // Its own position, or the elision mark when the span seats
                        // no number; never another tab's number.
                        assert!(
                            digits == (tab + 1).to_string() || label_says_nothing(text),
                            "{cols} cols: tab {tab} paints {text:?}"
                        );
                    }
                    // The pass's answer is final, so each string measures inside
                    // its span, except the fit's floor (a bare mark on a span too
                    // small for one glyph).
                    assert!(
                        wide_face(*tab, text) <= entries[n].span_px || text == "…",
                        "{cols} cols: {text:?} would need a SECOND fit at paint \
                         time ({}px span)",
                        entries[n].span_px
                    );
                }
            }
        }

        /// A selected chip whose fit names nothing takes its ordinal, like a quiet
        /// chip on the same window would.
        #[test]
        fn the_selection_takes_its_ordinal_rather_than_paint_nothing() {
            // A window that seats the number and not a character more — the
            // pressure the strip's own layout reaches, stated as a span.
            let squeeze = wide_face(0, "……");
            let entries = vec![
                BandLabel {
                    tab: 0,
                    span_px: squeeze,
                    text: "…-676-2".to_string(),
                },
                BandLabel {
                    tab: 1,
                    span_px: squeeze,
                    text: "…-676-2".to_string(),
                },
            ];
            // Fixture guard: the per-chip fit leaves the selection saying nothing.
            let naive = fit_label(entries[0].text.clone(), squeeze, |s| wide_face(0, s));
            assert!(
                label_says_nothing(&naive),
                "fixture: the fit must strand the selection, not merely shorten \
                 it — {naive:?}"
            );
            let fitted = fit_labels_distinctly(&entries, 0, &[], &wide_face);
            assert_eq!(
                fitted[0].1, "1",
                "the selected chip carries its position: {fitted:?}"
            );
            assert!(
                !label_says_nothing(&fitted[0].1),
                "the selection names something: {fitted:?}"
            );
            for (n, (tab, text)) in fitted.iter().enumerate() {
                for (m, (_, other)) in fitted.iter().enumerate() {
                    assert!(
                        n == m || text != other,
                        "tab {tab} doubles another chip's label: {fitted:?}"
                    );
                }
                assert!(
                    wide_face(*tab, text) <= entries[n].span_px,
                    "{text:?} would need a SECOND fit at paint time"
                );
            }
        }

        /// Every quiet twin is numbered, including the first one painted: a name
        /// two quiet chips share names neither.
        #[test]
        fn every_quiet_twin_takes_its_position_the_first_painted_too() {
            let mono = |_tab: usize, s: &str| s.chars().count() as f32 * CELL_W as f32;
            let span = mono(0, "13 · ~\\aterm") + 1.0;
            let entries: Vec<BandLabel> = [
                (5, "…gui\\src"),
                (8, "~\\aterm"),
                (11, "~\\aterm"),
                (12, "~\\aterm"),
                (13, "C:\\Temp"),
            ]
            .into_iter()
            .map(|(tab, text)| BandLabel {
                tab,
                span_px: span,
                text: text.to_string(),
            })
            .collect();
            let fitted = fit_labels_distinctly(&entries, 5, &[], &mono);
            assert_eq!(
                fitted,
                [
                    (5, "…gui\\src".to_string()),
                    (8, "9 · ~\\aterm".to_string()),
                    (11, "12 · ~\\aterm".to_string()),
                    (12, "13 · ~\\aterm".to_string()),
                    (13, "C:\\Temp".to_string()),
                ],
                "every twin carries its position; the selection and the loner \
                 keep their names"
            );
            // CONTROL: a name no other quiet chip carries is left alone.
            let lone = fit_labels_distinctly(&entries[..2], 5, &[], &mono);
            assert_eq!(lone[1].1, "~\\aterm");
        }

        /// A window too small for the digits leaves the selection with the mark,
        /// as for every other chip ([`ordinal_chip_label`]).
        #[test]
        fn a_selection_whose_window_seats_no_number_keeps_the_mark() {
            let entries = vec![BandLabel {
                tab: 9,
                span_px: wide_face(9, "1") * 0.5,
                text: "…-676-2".to_string(),
            }];
            let fitted = fit_labels_distinctly(&entries, 9, &[], &wide_face);
            assert!(
                label_says_nothing(&fitted[0].1),
                "not even `10` fits, so the mark stands: {fitted:?}"
            );
        }

        /// A label of only elision marks (`……`, from a tail cut the fit shrank) is
        /// detected structurally ([`label_says_nothing`]) and replaced by the
        /// chip's position.
        #[test]
        fn a_stub_of_nothing_but_marks_is_repaired_like_the_bare_mark() {
            // FIXTURE: the exact string the shipped fit leaves — the defect
            // itself, not a story about it.
            let squeeze = wide_face(1, "……");
            let stub = fit_label("…and".to_string(), squeeze, |s| wide_face(1, s));
            assert_eq!(stub, "……", "the pixel fit really does leave TWO marks");
            assert_ne!(stub, "…", "and it is NOT the literal the old guard tested");
            let entries = vec![
                BandLabel {
                    tab: 0,
                    span_px: 200.0,
                    text: "…alpha".to_string(),
                },
                BandLabel {
                    tab: 1,
                    span_px: squeeze,
                    text: "…and".to_string(),
                },
                BandLabel {
                    tab: 2,
                    span_px: 200.0,
                    text: "…gamma".to_string(),
                },
            ];
            let fitted = fit_labels_distinctly(&entries, 0, &[], &wide_face);
            assert_eq!(
                fitted[1].1, "2",
                "a chip whose fit says nothing takes its POSITION — it does not \
                 paint the stub the guard was meant to catch: {fitted:?}"
            );
            // Over the whole strip: a label that says nothing appears only where
            // the window cannot seat the chip's number.
            for (n, (tab, text)) in fitted.iter().enumerate() {
                assert!(
                    !label_says_nothing(text)
                        || wide_face(*tab, &(tab + 1).to_string()) > entries[n].span_px,
                    "tab {tab} paints {text:?} in a window its number fits"
                );
            }
        }

        /// When a repaired chip's window seats only its number and another chip's
        /// title already reads as that number, the impostor gives it back and
        /// takes its own position.
        #[test]
        fn the_bare_number_last_resort_never_doubles_a_label() {
            let squeeze = wide_face(2, "……");
            let entries = vec![
                BandLabel {
                    tab: 0,
                    span_px: 200.0,
                    text: "…zulu".to_string(),
                },
                // A tab whose title IS the number the chip below needs.
                BandLabel {
                    tab: 1,
                    span_px: 200.0,
                    text: "3".to_string(),
                },
                // …and a window that seats the number and nothing else.
                BandLabel {
                    tab: 2,
                    span_px: squeeze,
                    text: "…and".to_string(),
                },
            ];
            let fitted = fit_labels_distinctly(&entries, 0, &[], &wide_face);
            for (n, (tab, text)) in fitted.iter().enumerate() {
                for (m, (_, other)) in fitted.iter().enumerate() {
                    assert!(
                        n == m || text != other,
                        "tab {tab} doubles another chip's label: {fitted:?}"
                    );
                }
            }
            assert_eq!(
                fitted[2].1, "3",
                "the position claims its own number: {fitted:?}"
            );
            assert!(
                fitted[1].1.starts_with('2'),
                "the title that reads as a number carries its own position \
                 instead: {fitted:?}"
            );
        }

        /// A label left to the cell painter still counts as a name: the fit may not
        /// hand another chip that string.
        #[test]
        fn a_fitted_label_never_collides_with_a_cell_painter_fallback() {
            let entries = vec![
                BandLabel {
                    tab: 1,
                    span_px: 200.0,
                    text: "…~/aterm".to_string(),
                },
                BandLabel {
                    tab: 2,
                    span_px: 200.0,
                    text: "…$HOME/trust".to_string(),
                },
            ];
            let fitted = fit_labels_distinctly(&entries, 0, &["…~/aterm"], &wide_face);
            assert_ne!(
                fitted[0].1, "…~/aterm",
                "the fallback segment already paints that string"
            );
            assert!(fitted[0].1.starts_with('2'), "{:?}", fitted[0].1);
            assert_eq!(fitted[1].1, "…$HOME/trust", "an uncontested label is untouched");
        }

        /// Both title windows (chip-card band with its interior pad, and a padless
        /// strip one cell wider) resolve distinctly with own-position ordinals.
        /// `tab_content_layout` reads the pad off the build, so both are computed
        /// here.
        #[test]
        fn both_title_windows_resolve_the_same_strip_distinctly() {
            assert_eq!(
                STRIP_DISTINCT_LABELS, STRIP_IS_CHROME_BAND,
                "the pass runs wherever the band does — Windows included"
            );
            let titles: Vec<String> = ["alpha", "beta", "gamma", "delta"]
                .iter()
                .map(|leaf| format!("user@m17-tower: ~/work/service-{leaf}"))
                .collect();
            let metadata = plain(titles.len());
            let segments = layout_segments_with_metadata(80, titles.len(), &metadata, 0, false);
            let cell: Vec<String> =
                distinct_chip_labels(&segments, &titles, Some(&metadata), 0, None)
                    .into_iter()
                    .map(Option::unwrap)
                    .collect();
            // (lane, its interior card pad) for both builds.
            let mut widths = Vec::new();
            for (lane, pad) in [("padless", 0u16), ("chip-card", 1u16)] {
                let entries: Vec<BandLabel> = segments
                    .iter()
                    .filter_map(|seg| match seg.kind {
                        TabHit::Select(i) => {
                            let pad = if seg.end_col.saturating_sub(seg.start_col)
                                >= PREFERRED_MIN_TAB_COLS
                            {
                                pad
                            } else {
                                0
                            };
                            let start = seg.start_col + 1 + pad;
                            let end = match seg.close_col {
                                Some(close) => close.saturating_sub(1),
                                None => seg.end_col.saturating_sub(1 + pad),
                            };
                            let avail = end.saturating_sub(start);
                            Some(BandLabel {
                                tab: i,
                                span_px: f32::from(avail) * CELL_W as f32 - 1.0,
                                text: cell[i].clone(),
                            })
                        }
                        _ => None,
                    })
                    .collect();
                widths.push(entries[1].span_px);
                let fitted = fit_labels_distinctly(&entries, 0, &[], &wide_face);
                for (n, (tab, text)) in fitted.iter().enumerate() {
                    assert!(!text.is_empty(), "{lane}: a chip with a window speaks");
                    for (m, (_, other)) in fitted.iter().enumerate() {
                        assert!(
                            n == m || text != other,
                            "{lane}: two chips paint {text:?} — {fitted:?}"
                        );
                    }
                    let digits = text
                        .chars()
                        .take_while(char::is_ascii_digit)
                        .collect::<String>();
                    assert!(
                        digits.is_empty() || digits == (tab + 1).to_string(),
                        "{lane}: tab {tab} paints another tab's number: {text:?}"
                    );
                }
            }
            assert!(
                widths[0] > widths[1],
                "fixture: the two windows really are different ({widths:?}) — a \
                 padless strip spends no card pad"
            );
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

        /// An out-of-range `active` is clamped once ([`BandInput::selected`]) so the
        /// band exempts the same chip `layout_segments` widened and
        /// [`distinct_chip_labels`] exempted; the raw reading is shown beside it.
        #[test]
        fn an_out_of_range_selection_is_clamped_once_for_the_whole_band() {
            if !with_ui_faces() {
                return;
            }
            // Two titles that fit in cells and collapse onto one string in pixels.
            let titles: Vec<String> = ["alpha", "beta"]
                .iter()
                .map(|leaf| format!("{}{leaf}", "W".repeat(20)))
                .collect();
            let metadata = plain(titles.len());
            // The same out-of-range index the layout was given: it clamps, and
            // the LAST chip gets the selected chip's wider window.
            let segments = layout_segments_with_metadata(80, titles.len(), &metadata, 99, false);
            let input = BandInput {
                active: 99,
                ..band(
                    &segments,
                    &titles,
                    &metadata,
                    StripPaint::default(),
                    geometry(80, 1),
                )
            };
            assert_eq!(
                input.selected(),
                1,
                "the layout's selection is the last chip"
            );
            // A label size well past the mono cell, so the fit bites on any
            // host face rather than only on a wide one.
            let big = 30.0;
            let raw = resolve_band_labels(&input, input.active, 80, CELL_W as f32, big, true);
            let clamped =
                resolve_band_labels(&input, input.selected(), 80, CELL_W as f32, big, true);
            // Fixture guard, per host: the fit really collapses the pair.
            let raw_last = raw[1].as_deref().unwrap_or_default();
            if !raw_last.starts_with('2') {
                crate::tray_raster::clear_ui_fonts_for_test();
                return;
            }
            assert!(
                clamped[1].as_deref().is_some_and(|l| l.starts_with('W')),
                "the SELECTED chip keeps its title through the pixel fit — the \
                 repair is not the selection's to take: {clamped:?}"
            );
            assert!(
                clamped[0].as_deref().is_some_and(|l| l.starts_with('1')),
                "and the chip that lost the collision carries its position: \
                 {clamped:?}"
            );
            // The whole raster of an out-of-range selection equals the clamped
            // one's, so every band reader asks about the same tab.
            let selected = BandInput {
                active: input.selected(),
                ..band(
                    &segments,
                    &titles,
                    &metadata,
                    StripPaint::default(),
                    geometry(80, 1),
                )
            };
            let (raw_px, raw_w, raw_h) = image_of(&raster_band(&input, &[]).expect("band"));
            let (px, w, h) = image_of(&raster_band(&selected, &[]).expect("band"));
            assert_eq!((raw_w, raw_h), (w, h), "same strip, same canvas");
            let differing = raw_px.iter().zip(&px).filter(|(a, b)| a != b).count();
            assert_eq!(
                differing, 0,
                "the band paints a different strip for the same selection \
                 ({differing} bytes differ)"
            );
            crate::tray_raster::clear_ui_fonts_for_test();
        }

        /// Four chips whose titles share a prompt, drawn in the host's UI face
        /// where the fit bites, paint four different pictures over their title
        /// spans.
        #[test]
        fn the_painted_band_gives_no_two_chips_the_same_picture() {
            if !with_ui_faces() {
                return;
            }
            let titles: Vec<String> = ["A", "B", "C", "D"]
                .iter()
                .map(|leaf| format!("WMWM: ~/WWWWWWWWWW{leaf}"))
                .collect();
            let metadata = plain(titles.len());
            let segments = layout_segments_with_metadata(80, titles.len(), &metadata, 0, false);
            let geometry = geometry(80, 1);
            let label_px = band_label_px((BAND_TOP + CELL_H) as f32, 1.0);
            let cell: Vec<String> =
                distinct_chip_labels(&segments, &titles, Some(&metadata), 0, None)
                    .into_iter()
                    .map(Option::unwrap)
                    .collect();
            // Fixture guard, per host: the UI face's caps must out-measure the mono
            // cell so the fit bites; otherwise skip rather than pass vacuously.
            let naive: Vec<String> = segments
                .iter()
                .filter_map(|seg| match seg.kind {
                    TabHit::Select(i) => {
                        let span = window(seg).1;
                        Some(fit_label(cell[i].clone(), span, |s| {
                            crate::tray_raster::ui_text_width_for(TextFace::Ui, s, label_px)
                        }))
                    }
                    _ => None,
                })
                .collect();
            let bites = naive
                .iter()
                .enumerate()
                .any(|(a, x)| naive.iter().enumerate().any(|(b, y)| a != b && x == y));
            if !bites {
                crate::tray_raster::clear_ui_fonts_for_test();
                return;
            }
            let input = band(
                &segments,
                &titles,
                &metadata,
                StripPaint::default(),
                geometry,
            );
            let rows = raster_band(&input, &[]).expect("band");
            let (rgba, w, h) = image_of(&rows);
            // One picture per INACTIVE chip (equal windows, so equal labels
            // would be equal pixels), cropped to the title span.
            let pictures: Vec<(usize, Vec<u8>)> = segments
                .iter()
                .filter_map(|seg| match seg.kind {
                    TabHit::Select(i) if i != 0 => {
                        let layout = band_content_layout(seg, Some(PLAIN_TAB), seg.end_col);
                        let x0 = usize::from(layout.title_start) * CELL_W;
                        let x1 = usize::from(layout.title_end) * CELL_W;
                        let mut px = Vec::new();
                        for y in 0..h {
                            for x in x0..x1.min(w) {
                                px.extend_from_slice(&rgba[(y * w + x) * 4..(y * w + x) * 4 + 4]);
                            }
                        }
                        Some((i, px))
                    }
                    _ => None,
                })
                .collect();
            assert_eq!(pictures.len(), 3, "three inactive chips");
            for (i, a) in &pictures {
                assert!(
                    a.chunks(4).any(|px| px != &a[..4]),
                    "chip {i} painted a flat span — no label reached the glass"
                );
                for (j, b) in &pictures {
                    assert!(
                        i == j || a != b,
                        "chips {i} and {j} paint the same picture — the pixel \
                         fit collapsed two labels the cell pass told apart \
                         ({cell:?} → {naive:?})"
                    );
                }
            }
            crate::tray_raster::clear_ui_fonts_for_test();
        }

        #[test]
        fn multi_row_strip_gets_a_ref_list_per_row_over_one_taller_image() {
            if !with_ui_faces() {
                return;
            }
            let metadata = plain(1);
            let segments = layout_segments_with_metadata(60, 1, &metadata, 0, false);
            let titles = vec!["only".to_string()];
            let input = band(
                &segments,
                &titles,
                &metadata,
                StripPaint::default(),
                geometry(60, 2),
            );
            let rows = raster_band(&input, &[]).expect("band");
            assert_eq!(rows.len(), 2);
            assert!(rows[1].iter().all(|(_, r)| r.cell_row == 1));
            let (_, w, h) = image_of(&rows);
            let lift = BAND_TOP;
            assert_eq!((w, h), (60 * CELL_W, 2 * CELL_H + lift));
            crate::tray_raster::clear_ui_fonts_for_test();
        }

        /// Card-row geometry, no fonts: symmetric in the content region, clear of
        /// the resolved seam, never rounded past a pill.
        #[test]
        fn design_card_row_is_centred_in_the_content_and_clears_the_seam() {
            for (band_top, cell_h, strip_rows, underline_y, scale) in [
                (16usize, 18usize, 1usize, 15usize, 1.0f32),
                (2, 21, 1, 17, 1.0),
                (24, 28, 1, 23, 1.5),
                (13, 19, 2, 16, 1.0),
            ] {
                let img_h = band_top + strip_rows * cell_h;
                let seam_top = band_top + (strip_rows - 1) * cell_h + underline_y;
                let geometry = BandGeometry {
                    cols: 80,
                    cell_w: 8,
                    cell_h,
                    strip_rows,
                    band_top_px: band_top,
                    scale,
                    seam_top_px: Some(seam_top),
                };
                let label_px = band_label_px(img_h as f32, scale);
                let d = BandDesign::resolve(&geometry, img_h, label_px);
                let content = seam_top as f32;
                assert!(
                    (d.card_top - (content - d.card_bot)).abs() <= 1.0,
                    "symmetric insets: top {} vs bottom {}",
                    d.card_top,
                    content - d.card_bot
                );
                assert!(d.card_bot <= content, "cards clear the seam rule");
                assert!(
                    d.radius <= (d.card_bot - d.card_top) * 0.5 + 0.01,
                    "radius within the card"
                );
                assert!(
                    d.card_bot - d.card_top >= label_px,
                    "the card holds its label: {} vs {label_px}",
                    d.card_bot - d.card_top
                );
                // The baseline's cap box sits inside the card.
                assert!(d.baseline - label_px * 0.7 >= d.card_top - 1.0);
                assert!(d.baseline <= d.card_bot + 1.0);
            }
        }

        /// Design lane: the canvas is opaque, so no cell background shows under a
        /// rounded card.
        #[test]
        fn design_canvas_is_opaque_edge_to_edge() {
            if !with_ui_faces() {
                return;
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
            let rows = raster_band(&input, &[]).expect("band");
            let (rgba, w, h) = image_of(&rows);
            assert!(
                (0..w * h).all(|i| rgba[i * 4 + 3] == 255),
                "an opaque bar has no see-through pixels"
            );
            crate::tray_raster::clear_ui_fonts_for_test();
        }

        /// Design lane: the `+` glyph's ink is centred on its segment's pixel
        /// centre (its hit region's centre), to the pixel.
        #[test]
        fn design_plus_glyph_is_dead_centred_on_its_hit_region() {
            if !with_ui_faces() {
                return;
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
            let rows = raster_band(&input, &[]).expect("band");
            let (rgba, w, h) = image_of(&rows);
            let plus = segments
                .iter()
                .find(|seg| matches!(seg.kind, TabHit::NewTab))
                .expect("an 80-col two-tab strip keeps its +");
            let (px0, px1) = (
                usize::from(plus.start_col) * CELL_W,
                usize::from(plus.end_col) * CELL_W,
            );
            let colors = strip_colors_with_active(Theme::default(), None);
            let (x0, y0, x1, y1) =
                ink_bbox_off_surfaces(&rgba, w, h, px0, px1, &[colors.band_bg, colors.chip_bg])
                    .expect("the + inks its segment");
            let seg_mid = (px0 + px1) as f32 / 2.0;
            let ink_mid = (x0 + x1) as f32 / 2.0;
            assert!(
                (ink_mid - seg_mid).abs() <= 1.0,
                "+ centred on its hit region: ink mid {ink_mid} vs segment mid {seg_mid}"
            );
            // …and on the shared optical row: its vertical middle is the same
            // cap-box midline every label centres on.
            let content_h = h as f32 - 2.0;
            let ink_mid_y = (y0 + y1) as f32 / 2.0;
            assert!(
                (ink_mid_y - content_h / 2.0).abs() <= 2.5,
                "+ rides the label row: {ink_mid_y} vs {}",
                content_h / 2.0
            );
            crate::tray_raster::clear_ui_fonts_for_test();
        }

        /// Design lane: below the resolved seam line the canvas is pure band tone,
        /// so the seam rule lands on clean ground.
        #[test]
        fn design_cards_clear_the_resolved_seam_row() {
            if !with_ui_faces() {
                return;
            }
            let metadata = plain(2);
            let segments = layout_segments_with_metadata(80, 2, &metadata, 0, false);
            let titles = vec!["alpha".to_string(), "beta".to_string()];
            let seam_top = BAND_TOP + CELL_H - 4;
            let mut geometry = geometry(80, 1);
            geometry.seam_top_px = Some(seam_top);
            let input = band(
                &segments,
                &titles,
                &metadata,
                StripPaint::default(),
                geometry,
            );
            let rows = raster_band(&input, &[]).expect("band");
            let (rgba, w, h) = image_of(&rows);
            let colors = strip_colors_with_active(Theme::default(), None);
            for y in seam_top..h {
                for x in 0..w {
                    let px = &rgba[(y * w + x) * 4..(y * w + x) * 4 + 3];
                    assert_eq!(
                        px,
                        &colors.band_bg[..],
                        "row {y} col {x} below the seam is bare band"
                    );
                }
            }
            crate::tray_raster::clear_ui_fonts_for_test();
        }

        /// `(band_top_px, cell_h, underline_y)` for the optical bands the Windows
        /// metrics produce at 96 dpi for four font sizes. The synthetic head keeps
        /// the band 32 px, so `band_top` shrinks as `cell_h` grows:
        ///
        /// | `font_px` | cell   | `pad_top + head` | seam row |
        /// |----------:|:-------|-----------------:|---------:|
        /// | 11        | 6×13   | 2 + 17 = 19      | 29       |
        /// | 13        | 8×15   | 2 + 15 = 17      | 29       |
        /// | 16        | 9×19   | 2 + 11 = 13      | 28       |
        /// | 20        | 12×23  | 2 + 7  = 9       | 27       |
        const WIN_BANDS: [(usize, usize, usize); 4] =
            [(19, 13, 10), (17, 15, 12), (13, 19, 15), (9, 23, 18)];

        /// A one-window band at `(band_top, cell_h, underline_y)`.
        fn win_geometry(
            cols: usize,
            (band_top, cell_h, underline_y): (usize, usize, usize),
        ) -> BandGeometry {
            BandGeometry {
                cols,
                cell_w: CELL_W,
                cell_h,
                strip_rows: 1,
                band_top_px: band_top,
                scale: 1.0,
                seam_top_px: Some(band_top + underline_y),
            }
        }

        /// At every Windows band, the label run sits inside its card, is not
        /// bottom-heavy, starts above the card's midline, and its descenders clear
        /// the seam the cell lane stamps over the raster.
        #[test]
        fn the_label_is_centred_in_its_card_with_descender_room_at_every_font_px() {
            if !with_ui_faces() {
                return;
            }
            let colors = strip_colors_with_active(Theme::default(), None);
            for win in WIN_BANDS {
                let (band_top, cell_h, underline_y) = win;
                let metadata = plain(2);
                let segments = layout_segments_with_metadata(80, 2, &metadata, 0, false);
                // ASCENDER and DESCENDER in one run, so the bbox is the label's
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
                // Not bottom-heavy: air above and below the run agree within a
                // descender's depth.
                let above = y0 as f32 - d.card_top;
                let below = d.card_bot - y1 as f32;
                assert!(
                    (above - below).abs() <= label_px * 0.5,
                    "{win:?}: air above {above} vs below {below} (ink {y0}..{y1}, card {}..{}, label {label_px})",
                    d.card_top,
                    d.card_bot
                );
                // The run's top is above the card's midline.
                let card_mid = (d.card_top + d.card_bot) * 0.5;
                assert!(
                    (y0 as f32) < card_mid,
                    "{win:?}: the run starts at {y0}, below the card's midline {card_mid}"
                );
                // DESCENDER ROOM: the last inked row clears the seam rule.
                assert!(
                    y1 <= band_top + underline_y,
                    "{win:?}: descenders reach {y1}, seam rule at {}",
                    band_top + underline_y
                );
            }
            crate::tray_raster::clear_ui_fonts_for_test();
        }

        /// Every tab is an inset rounded card on bare band, and a quiet tab is a
        /// real surface distinct from the band.
        #[test]
        fn quiet_and_selected_chips_are_inset_rounded_cards_on_bare_band() {
            if !with_ui_faces() {
                return;
            }
            let colors = strip_colors_with_active(Theme::default(), None);
            let win = WIN_BANDS[1];
            let metadata = plain(3);
            let segments = layout_segments_with_metadata(90, 3, &metadata, 0, false);
            let titles = vec!["one".to_string(), "two".to_string(), "three".to_string()];
            let geometry = win_geometry(90, win);
            let input = band(
                &segments,
                &titles,
                &metadata,
                StripPaint::default(),
                geometry,
            );
            let rows = raster_band(&input, &[]).expect("band");
            let (rgba, w, h) = image_of(&rows);
            let label_px = band_label_px(h as f32, 1.0);
            let d = BandDesign::resolve(&geometry, h, label_px);
            let card = |seg: &TabSegment| {
                (
                    (f32::from(seg.start_col) * CELL_W as f32 + d.gap_h).round() as usize,
                    (f32::from(seg.end_col) * CELL_W as f32 - d.gap_h).round() as usize,
                )
            };
            for (i, seg) in segments
                .iter()
                .filter(|s| matches!(s.kind, TabHit::Select(_)))
                .enumerate()
            {
                let (x0, x1) = card(seg);
                let mid_x = (x0 + x1) / 2;
                let fill = if i == 0 {
                    colors.active_bg
                } else {
                    colors.chip_bg
                };
                // The card's own surface, sampled ABOVE the label's own rows and
                // clear of the corner radius.
                let inside = pixel(&rgba, w, mid_x, (d.card_top as usize) + 2);
                assert_eq!(inside, fill, "tab {i}: the card carries its own tone");
                // A QUIET card is a real surface, not the band wearing a tick.
                if i > 0 {
                    assert_ne!(
                        fill, colors.band_bg,
                        "tab {i}: a quiet chip must not be byte-identical to the band"
                    );
                }
                // TOP INSET: the row just above the card is bare band, so the
                // card floats instead of being glued to the window's edge.
                assert_eq!(
                    pixel(&rgba, w, mid_x, d.card_top as usize - 1),
                    colors.band_bg,
                    "tab {i}: bare band above the card (top inset)"
                );
                assert_eq!(
                    pixel(&rgba, w, mid_x, d.card_bot as usize + 1),
                    colors.band_bg,
                    "tab {i}: bare band below the card (bottom inset)"
                );
                // ROUNDED: the card's literal corner pixel is not the fill —
                // the radius ate it. A square chip fails here.
                assert_ne!(
                    pixel(&rgba, w, x0, d.card_top as usize),
                    fill,
                    "tab {i}: the corner is rounded, not square"
                );
            }
            // Between neighbouring cards the band shows through, `2·gap_h` px wide
            // rather than a whole cell.
            let a = card(&segments[0]);
            let b = card(&segments[1]);
            assert!(b.0 > a.1, "the cards do not touch");
            for x in a.1..b.0 {
                assert_eq!(
                    pixel(&rgba, w, x, (d.card_top as usize) + 2),
                    colors.band_bg,
                    "the gutter at x={x} is bare band"
                );
            }
            assert!(
                b.0 - a.1 < CELL_W,
                "the gutter is SUB-CELL ({} px vs a {CELL_W} px cell)",
                b.0 - a.1
            );
            let _ = h;
            crate::tray_raster::clear_ui_fonts_for_test();
        }

        /// The `+` is not painted in the selected card's tone, and hovering it
        /// changes its pixels.
        #[test]
        fn the_plus_is_its_own_surface_and_lights_under_the_pointer() {
            if !with_ui_faces() {
                return;
            }
            let colors = strip_colors_with_active(Theme::default(), None);
            let win = WIN_BANDS[1];
            let metadata = plain(2);
            let segments = layout_segments_with_metadata(80, 2, &metadata, 0, false);
            let titles = vec!["one".to_string(), "two".to_string()];
            let geometry = win_geometry(80, win);
            let plus = segments
                .iter()
                .find(|s| matches!(s.kind, TabHit::NewTab))
                .copied()
                .expect("an 80-col two-tab strip keeps its +");
            // A point on the button's square but OFF the glyph's own strokes:
            // a quarter of the way in from the square's left edge.
            let label_px = band_label_px((geometry.band_top_px + geometry.cell_h) as f32, 1.0);
            let d =
                BandDesign::resolve(&geometry, geometry.band_top_px + geometry.cell_h, label_px);
            let seg0 = f32::from(plus.start_col) * CELL_W as f32;
            let seg1 = f32::from(plus.end_col) * CELL_W as f32;
            let centre = (seg0 + seg1) * 0.5;
            let side = (d.card_bot - d.card_top).min(seg1 - seg0 - 2.0 * d.gap_h);
            // Mid-height (no corner radius) and a third of the way toward the
            // square's edge: past the glyph's arms, short of the rounded rim.
            let probe = (
                (centre - side * 0.35).round() as usize,
                (d.card_top + (d.card_bot - d.card_top) * 0.5).round() as usize,
            );
            let sample = |hot: bool| {
                let paint = StripPaint {
                    hovered: None,
                    new_tab_hovered: hot,
                    version_hovered: false,
                    subtitle: None,
                    rename: None,
                };
                let input = band(&segments, &titles, &metadata, paint, geometry);
                let rows = raster_band(&input, &[]).expect("band");
                let (rgba, w, _) = image_of(&rows);
                pixel(&rgba, w, probe.0, probe.1)
            };
            let quiet = sample(false);
            assert_eq!(
                quiet, colors.chip_bg,
                "the resting `+` sits on the quiet chip tone"
            );
            assert_ne!(
                quiet, colors.active_bg,
                "…which is NOT the selected card's tone — the primary affordance \
                 must not read as one more selected tab"
            );
            let hot = sample(true);
            assert_ne!(quiet, hot, "hovering the `+` changes pixels");
            assert_eq!(hot, colors.hover_bg, "…to the band's own hover material");
            crate::tray_raster::clear_ui_fonts_for_test();
        }

        /// A hovered quiet tab is visibly washed and shows its ✕.
        #[test]
        fn hovering_a_quiet_chip_lifts_its_card_and_reveals_the_close_mark() {
            if !with_ui_faces() {
                return;
            }
            let colors = strip_colors_with_active(Theme::default(), None);
            let win = WIN_BANDS[1];
            let metadata = plain(3);
            let segments = layout_segments_with_metadata(90, 3, &metadata, 0, false);
            let titles = vec!["one".to_string(), "two".to_string(), "three".to_string()];
            let geometry = win_geometry(90, win);
            let seg = segments[1];
            let close = seg.close_col.expect("a 30-col chip reserves its ✕");
            let label_px = band_label_px((win.0 + win.1) as f32, 1.0);
            let d = BandDesign::resolve(&geometry, win.0 + win.1, label_px);
            let probe_y = d.card_top as usize + 2;
            // Clear of the corner radius, and above the label's own rows.
            let probe_x =
                (f32::from(seg.start_col) * CELL_W as f32 + d.gap_h).round() as usize + 10;
            let close_span = (usize::from(close) * CELL_W, usize::from(close + 1) * CELL_W);
            let sample = |hovered: Option<usize>| {
                let paint = StripPaint {
                    hovered,
                    new_tab_hovered: false,
                    version_hovered: false,
                    subtitle: None,
                    rename: None,
                };
                let input = band(&segments, &titles, &metadata, paint, geometry);
                let rows = raster_band(&input, &[]).expect("band");
                let (rgba, w, h) = image_of(&rows);
                let card = pixel(&rgba, w, probe_x, probe_y);
                let mark = ink_bbox_off_surfaces(
                    &rgba,
                    w,
                    h,
                    close_span.0,
                    close_span.1,
                    &[
                        colors.band_bg,
                        colors.chip_bg,
                        colors.hover_bg,
                        colors.active_bg,
                    ],
                );
                (card, mark.is_some())
            };
            let (quiet_card, quiet_mark) = sample(None);
            let (hot_card, hot_mark) = sample(Some(1));
            assert_eq!(
                quiet_card, colors.chip_bg,
                "resting: the quiet chip's own card"
            );
            assert_eq!(
                hot_card, colors.hover_bg,
                "hovered: the wash a full rung up"
            );
            assert_ne!(quiet_card, hot_card, "the hover is a REAL surface change");
            assert!(!quiet_mark, "a quiet chip shows no ✕ (the mis-click trap)");
            assert!(hot_mark, "…and the hovered one does");
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
    }

    /// The lone-chip cap is a maximum: a narrower window still gets its equal share.
    #[test]
    fn a_narrow_window_keeps_the_share_the_lone_chip_cap_never_widens_it() {
        let cols = 20u16;
        let segs = layout_segments(cols, 1, 0, false);
        assert_eq!(segs[0].end_col, cols - NEW_TAB_W);
        assert_eq!(segs[1].start_col, cols - NEW_TAB_W);
    }

    /// The `+`'s raised card never touches the active tab's: the painter leaves the
    /// `+` segment's leading pad on the band (and the `↻` alert's trailing pad), while
    /// the gutter column still answers `TabHit::NewTab`.
    #[test]
    fn the_new_tab_card_never_fuses_with_the_active_tab_card() {
        let colors = strip_colors(Theme::default());
        // One tab, active, with the `+` immediately after it.
        let segs = layout_segments(40, 1, 0, false);
        let plus = segs
            .iter()
            .find(|s| s.kind == TabHit::NewTab)
            .copied()
            .unwrap();
        let mut row = vec![blank_cell(Theme::default()); 40];
        paint_strip(&mut row, &segs, &["one".into()], None, 0, Theme::default());
        if STRIP_IS_CHROME_BAND {
            assert_eq!(
                row[plus.start_col as usize].bg, colors.band_bg,
                "the `+`'s leading pad stays BAND so the two cards cannot fuse"
            );
            let plus_bg = if STRIP_CHIP_CARDS {
                colors.chip_bg
            } else {
                colors.raise_bg
            };
            assert_eq!(
                row[plus.start_col as usize + 1].bg,
                plus_bg,
                "the button surface itself is off the band"
            );
            if STRIP_CHIP_CARDS {
                // Chip-card band: beyond the gutter, the tones differ too — the
                // `+` chip can never be mistaken for one more selected card.
                assert_ne!(
                    row[plus.start_col as usize + 1].bg,
                    row[plus.start_col as usize - 1].bg,
                    "the `+` chip is distinct from the active card beside it"
                );
            }
        }
        assert_eq!(
            hit_test(&segs, plus.start_col),
            Some(TabHit::NewTab),
            "the gutter is paint-only: the click target is the whole segment"
        );
        // And the `↻` alert keeps a band column between itself and tab 0.
        let segs = layout_segments(40, 1, 0, true);
        let mut row = vec![blank_cell(Theme::default()); 40];
        paint_strip(&mut row, &segs, &["one".into()], None, 0, Theme::default());
        if STRIP_IS_CHROME_BAND {
            assert_eq!(row[UPDATE_W as usize - 1].bg, colors.band_bg);
            assert_eq!(hit_test(&segs, UPDATE_W - 1), Some(TabHit::Update));
        }
    }

    /// The `+` answers the pointer in the cell lane (the band's half is
    /// `the_plus_is_its_own_surface_and_lights_under_the_pointer`).
    #[test]
    fn the_new_tab_button_takes_the_hover_wash_under_the_pointer() {
        let theme = Theme::default();
        let colors = strip_colors(theme);
        let metadata = [TabStripMetadata::from_presentation(
            &crate::tab_model::TabPresentation::terminal("one"),
        )];
        let segs = layout_segments_with_metadata(40, 1, &metadata, 0, false);
        let plus = segs
            .iter()
            .find(|s| s.kind == TabHit::NewTab)
            .copied()
            .expect("a one-tab strip still offers the +");
        let paint = |hot: bool| {
            let mut row = vec![blank_cell(theme); 40];
            paint_strip_with_metadata(
                &mut row,
                &segs,
                &["one".to_string()],
                &metadata,
                StripPaint {
                    hovered: None,
                    new_tab_hovered: hot,
                    version_hovered: false,
                    subtitle: None,
                    rename: None,
                },
                0,
                theme,
                None,
            );
            row
        };
        let quiet = paint(false);
        let hot = paint(true);
        let button = plus.start_col as usize + 1;
        assert_ne!(
            quiet[button].bg, hot[button].bg,
            "hovering the `+` must change its surface"
        );
        assert_eq!(
            hot[button].bg, colors.hover_bg,
            "…to the band's one hover material, the same one a chip takes"
        );
        assert_ne!(
            hot[button].bg, colors.active_bg,
            "…and never to the SELECTED card's tone"
        );
        // The glyph is still there, and still legible on the moved ground.
        assert_eq!(hot[button].ch, '+');
        assert_eq!(hot[button].fg, colors.button_hover_fg);
        // Paint only: the wash never moves a hit target.
        assert_eq!(hit_test(&segs, plus.start_col), Some(TabHit::NewTab));
    }

    /// A chip with status marks gains a `connector_col` at exactly the cell the
    /// painter puts the status canvas on, hit-testing to [`TabHit::Connector`]; the
    /// close column keeps its own cell, markless chips and the metadata-free layout
    /// mint no connector.
    #[test]
    fn connector_col_is_the_painted_status_cell_and_hits_connector() {
        let theme = Theme::default();
        let mut busy = crate::tab_model::TabPresentation::terminal("a");
        busy.indicators.busy = true;
        let metadata = [
            TabStripMetadata::from_presentation(&busy),
            TabStripMetadata::from_presentation(&crate::tab_model::TabPresentation::terminal("b")),
        ];
        let segments = layout_segments_with_metadata(60, 2, &metadata, 0, false);
        let seg = segments[0];
        let connector = seg.connector_col.expect("status marks mint a connector");
        assert_ne!(Some(connector), seg.close_col, "distinct from the close ✕");
        assert_eq!(hit_test(&segments, connector), Some(TabHit::Connector(0)));
        assert_eq!(
            hit_test(&segments, seg.close_col.expect("wide chip has a ✕")),
            Some(TabHit::Close(0))
        );
        assert_eq!(
            hit_test(&segments, seg.start_col + 1),
            Some(TabHit::Select(0))
        );
        assert_eq!(
            segments[1].connector_col, None,
            "a markless chip has no connector; its whole interior selects"
        );
        assert_eq!(
            hit_test(&segments, segments[1].start_col + 3),
            Some(TabHit::Select(1))
        );

        // Hit geometry == painted geometry: the status image lands on the SAME
        // cell the hit region claims.
        let titles = ["one".to_string(), "two".to_string()];
        let mut row = vec![blank_cell(theme); 60];
        let images = paint_strip_with_metadata(
            &mut row,
            &segments,
            &titles,
            &metadata,
            StripPaint {
                hovered: None,
                new_tab_hovered: false,
                version_hovered: false,
                subtitle: None,
                rename: None,
            },
            0,
            theme,
            None,
        );
        assert!(
            images.iter().any(|(col, _)| *col == usize::from(connector)),
            "the status canvas paints exactly on the connector cell"
        );

        // The metadata-free base layout never mints a connector.
        assert!(
            layout_segments(60, 2, 0, false)
                .iter()
                .all(|s| s.connector_col.is_none())
        );

        // A chip too narrow to paint its marks mints no connector; the context menu
        // is the connect path there.
        let narrow = layout_segments_with_metadata(9, 3, &[metadata[0]; 3], 0, false);
        assert!(
            narrow.iter().all(|s| s.connector_col.is_none()),
            "narrow chips drop the marks, so they must drop the connector"
        );
        assert_eq!(
            hit_test(&narrow, narrow[0].start_col + 1),
            Some(TabHit::Select(0))
        );
    }

    /// The solo band's connector is its fixed trailing status cell (`end_col - 2`),
    /// minted only when the band is wide enough to paint marks.
    #[test]
    fn solo_band_connector_is_the_trailing_status_cell() {
        let theme = Theme::default();
        let mut busy = crate::tab_model::TabPresentation::terminal("a");
        busy.indicators.busy = true;
        let metadata = [TabStripMetadata::from_presentation(&busy)];
        let segments = layout_segments_with_metadata(40, 1, &metadata, 0, false);
        let seg = segments[0];
        // A lone tab is a solo band only on macOS ([`SOLO_TITLE_BAND`]); elsewhere
        // the connector is the chip's status cell, at the same column.
        assert_eq!(seg.solo, SOLO_TITLE_BAND);
        let connector = seg.connector_col.expect("a lone tab with marks");
        // Solo band: the fixed trailing status cell. Ordinary chip: one cell
        // inside its ✕ (or the cell the ✕ would have owned when it has none).
        let expected = if seg.solo {
            seg.end_col - 2
        } else {
            seg.close_col.map_or(seg.end_col - 2, |close| close - 2)
        };
        assert_eq!(connector, expected);
        assert_eq!(hit_test(&segments, connector), Some(TabHit::Connector(0)));

        let titles = ["one".to_string()];
        let mut row = vec![blank_cell(theme); 40];
        let images = paint_strip_with_metadata(
            &mut row,
            &segments,
            &titles,
            &metadata,
            StripPaint {
                hovered: None,
                new_tab_hovered: false,
                version_hovered: false,
                subtitle: None,
                rename: None,
            },
            0,
            theme,
            None,
        );
        assert!(
            images.iter().any(|(col, _)| *col == usize::from(connector)),
            "the solo status canvas paints on the connector cell"
        );

        // A markless solo band mints none.
        let clean = [TabStripMetadata::from_presentation(
            &crate::tab_model::TabPresentation::terminal("a"),
        )];
        let segments = layout_segments_with_metadata(40, 1, &clean, 0, false);
        assert_eq!(segments[0].connector_col, None);
    }

    /// The inline rename field replaces the chip's title span with the find bar's
    /// well: text verbatim, a reverse-video block caret, and the selected chip's
    /// accent seam kept underneath.
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
        paint_strip_with_metadata(
            &mut row,
            &segments,
            &titles,
            &metadata,
            StripPaint {
                hovered: None,
                new_tab_hovered: false,
                version_hovered: false,
                subtitle: None,
                rename: Some(StripRenameField {
                    tab: 0,
                    text: "build",
                    cursor: 2,
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
        assert!(painted.contains("build"), "the field's text, not the title");
        assert!(
            !painted.contains("one"),
            "the label is replaced while editing, not drawn beside the field"
        );
        // The caret cell is the one at the edit position, reverse-videoed: the
        // well's background becomes its ink, so the caret costs no cell.
        let colors = crate::chrome_band::band_colors(theme);
        let caret = row
            .iter()
            .find(|cell| cell.ch == 'i')
            .expect("the character under the caret is still drawn");
        assert_eq!(caret.fg, colors.field_bg, "reverse video: bg becomes ink");
        assert_eq!(caret.bg, colors.caret);
        assert_eq!(
            caret.underline,
            UnderlineStyle::Single,
            "the edited chip is still the selected one"
        );
        // The tab NOT being edited keeps its ordinary label.
        let other: String = row[segments[1].start_col as usize..segments[1].end_col as usize]
            .iter()
            .map(|c| c.ch)
            .collect();
        assert!(other.contains("two"));
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

    /// An EMPTY field shows the label the ladder falls back to, dimmed — so
    /// "commit empty and it falls back to that" is visible rather than folklore.
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

    /// The solo band paints the window title: no raised card, underline or `✕`, the
    /// title centred and its description trailing dim.
    #[test]
    fn solo_band_paints_a_centred_title_and_description_with_no_chip() {
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
                // Hovered, and STILL no ✕: a title band has none to reveal.
                hovered: Some(0),
                new_tab_hovered: false,
                version_hovered: false,
                subtitle: Some("~/aterm"),
                rename: None,
            },
            0,
            theme,
            None,
        );
        let painted: String = row[..band.end_col as usize].iter().map(|c| c.ch).collect();
        assert!(
            !painted.contains('✕'),
            "a title band has no close affordance"
        );
        assert!(painted.contains("aterm"), "the title is drawn");
        assert!(painted.contains("~/aterm"), "the description trails it");
        // CENTRED: the ink is balanced within the band, not packed against an edge.
        let first = painted.find(|c: char| c != ' ').expect("some ink");
        let last = painted.rfind(|c: char| c != ' ').expect("some ink");
        let right_pad = painted.len() - 1 - last;
        assert!(
            first.abs_diff(right_pad) <= 1,
            "title group centred in the band (left {first}, right {right_pad})"
        );
        // Flat body chrome — the raised selected-card treatment belongs to a
        // switcher, and a lone tab is not one.
        let colors = strip_colors(theme);
        let title_cell = row[first];
        assert_eq!(title_cell.bg, colors.band_bg, "no raised card");
        assert_eq!(
            title_cell.underline,
            UnderlineStyle::None,
            "no selection rule"
        );
        assert_eq!(title_cell.fg, colors.fg, "full-strength title ink");
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

    /// The solo band spends its width on the TITLE: the description compresses
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

    /// Two or more tabs split the whole band into equal shares with no maximum, and
    /// none is `solo`.
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
            let per = band / u16::try_from(count).unwrap();
            for (index, seg) in tabs.iter().enumerate() {
                assert!(!seg.solo, "only a LONE tab is a title band");
                assert_eq!(seg.start_col, per * u16::try_from(index).unwrap());
                assert_eq!(seg.end_col - seg.start_col, per, "equal shares");
            }
            // Nothing but the integer-division remainder is left over.
            assert!(band - tabs.last().unwrap().end_col < u16::try_from(count).unwrap());
        }
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
        // Tabs start AFTER the icon.
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
                drop_target: false,
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
        // A non-closable chip's freed trailing cell holds its status canvas, which
        // is the connector.
        assert_eq!(
            canonical[1].connector_col,
            Some(base[1].close_col.unwrap()),
            "the status canvas takes the freed trailing cell"
        );
        assert_eq!(
            hit_test(&canonical, base[1].close_col.unwrap()),
            Some(TabHit::Connector(1)),
            "the painted status cell hit-tests as the connector"
        );
    }

    #[test]
    fn icon_dirty_title_and_close_slots_are_exact_and_disjoint() {
        let theme = Theme::default();
        let metadata = [
            TabStripMetadata {
                icon: Some(TabIconKind::Settings),
                dirty: true,
                busy: true,
                attention: ChipLevel::Wait,
                conn: None,
                closable: true,
                drop_target: false,
            },
            TabStripMetadata::clean(TabIconKind::Editor),
        ];
        let segments = layout_segments_with_metadata(80, metadata.len(), &metadata, 0, false);
        let mut row = vec![blank_cell(theme); 80];
        let images = paint_strip_with_metadata(
            &mut row,
            &segments,
            &["Settings".to_string(), "notes.md".to_string()],
            &metadata,
            // Tab 0 is under the pointer: the ✕ is a hover-only reveal, so its
            // glyph assertion below is about the HOVERED tab.
            StripPaint {
                hovered: Some(0),
                new_tab_hovered: false,
                version_hovered: false,
                subtitle: None,
                rename: None,
            },
            0,
            theme,
            None,
        );
        let first = segments[0];
        let layout = tab_content_layout(&first, metadata[0]);
        // A roomy chip card keeps one interior pad cell after its gutter; the flat
        // strip starts content right after the leading pad.
        let pad = u16::from(STRIP_CHIP_CARDS);
        assert_eq!(layout.icon_start, Some(first.start_col + 1 + pad));
        assert_eq!(layout.title_start, first.start_col + 4 + pad);
        assert_eq!(layout.status_col, Some(first.close_col.unwrap() - 2));
        assert_eq!(row[layout.title_start as usize].ch, 'S');
        assert_eq!(row[first.close_col.unwrap() as usize].ch, '✕');
        let image_cols: Vec<_> = images.iter().map(|(col, _)| *col).collect();
        assert!(image_cols.contains(&usize::from(first.start_col + 1 + pad)));
        assert!(image_cols.contains(&usize::from(first.start_col + 2 + pad)));
        assert!(image_cols.contains(&usize::from(layout.status_col.unwrap())));
        assert!(!image_cols.contains(&usize::from(first.close_col.unwrap())));
        assert_eq!(
            hit_test(&segments, first.start_col + 1),
            Some(TabHit::Select(0)),
            "paint-only icon cells keep the full tab target"
        );
    }

    #[test]
    fn every_non_terminal_app_icon_has_distinct_deterministic_primitive_pixels() {
        let kinds = [
            TabIconKind::Settings,
            TabIconKind::Markdown,
            TabIconKind::Editor,
            TabIconKind::Recovery,
        ];
        let rasters: Vec<Vec<u8>> = kinds
            .iter()
            .map(|kind| rasterize_icon(tab_icon_primitives(*kind), [213, 219, 255]))
            .collect();
        for (kind, raster) in kinds.iter().zip(&rasters) {
            assert_eq!(raster.len(), 32 * 32 * 4, "{kind:?} raster extent");
            assert_eq!(
                raster,
                &rasterize_icon(tab_icon_primitives(*kind), [213, 219, 255]),
                "{kind:?} is deterministic"
            );
            assert!(
                raster.as_chunks::<4>().0.iter().any(|pixel| pixel[3] != 0),
                "{kind:?} has visible primitive coverage"
            );
        }
        for i in 0..rasters.len() {
            for j in i + 1..rasters.len() {
                assert_ne!(rasters[i], rasters[j], "icons {i}/{j} must not alias");
            }
        }
        assert_eq!(
            kinds.map(TabIconKind::semantic_name),
            ["settings", "markdown", "editor", "recovery"]
        );
        assert_eq!(
            crate::tab_model::TabPresentation::terminal("shell").icon,
            None,
            "a terminal cannot represent an app icon"
        );
    }

    /// [`IconKey`] names every input the raster depends on: every reachable key
    /// (4 icon kinds, all 8 status bit combinations, two colours) through the real
    /// `append_*` entry points matches a fresh rasterization byte for byte.
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
                    drop_target: false,
                    closable: true,
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

    /// The SOLO band's subtitle comes from the composed session chrome, prefers
    /// authored prose over generated status, and never echoes the title back.
    #[test]
    fn solo_subtitle_prefers_authored_prose_and_never_echoes_the_title() {
        let full = "aterm\ndescription: the release cutter\nactivity: Building\ncwd: ~/aterm";
        assert_eq!(
            solo_subtitle("aterm", Some(full)).as_deref(),
            Some("the release cutter"),
            "an authored description outranks every generated fact"
        );
        assert_eq!(
            solo_subtitle("aterm", Some("aterm\ncwd: ~/aterm\nactivity: Building")).as_deref(),
            Some("~/aterm"),
            "where the session is outranks what it is doing"
        );
        assert_eq!(
            solo_subtitle("aterm", Some("aterm\nactivity: Building")).as_deref(),
            Some("Building")
        );
        assert_eq!(
            solo_subtitle("aterm", Some("aterm\nstate: exited")),
            None,
            "a lifecycle word is not a description; the bare title reads better"
        );

        // A title that already carries the fact gets no echo (the composed label
        // folds the activity in: "aterm · Ready").
        assert_eq!(
            solo_subtitle("aterm · Ready", Some("aterm · Ready\nactivity: Ready")),
            None
        );
        // …and the NEXT-best fact still wins when only the first one echoes.
        assert_eq!(
            solo_subtitle(
                "aterm · Ready",
                Some("aterm · Ready\nactivity: Ready\ncwd: ~/aterm")
            )
            .as_deref(),
            Some("~/aterm")
        );

        // Nothing to say stays silent rather than padding the title.
        assert_eq!(solo_subtitle("aterm", None), None);
        assert_eq!(solo_subtitle("aterm", Some("aterm")), None);
        assert_eq!(solo_subtitle("aterm", Some("aterm\ncwd: ")), None);
        assert_eq!(
            solo_subtitle("aterm", Some("aterm\ncwd: -")),
            None,
            "the unset sentinel is not a description"
        );
        // A timeline row is not an identity line and must never be mistaken for one.
        assert_eq!(
            solo_subtitle("aterm", Some("aterm\n\nstate changed · just now")),
            None
        );
    }

    #[test]
    fn native_point_layout_keeps_close_and_tab_targets_while_reserving_app_icon_only() {
        let plain = native_tab_content_layout(220.0, 14.0, false, false);
        let app = native_tab_content_layout(220.0, 14.0, true, true);
        assert_eq!(plain.close, [2.0, 6.0, 16.0, 16.0]);
        assert_eq!(app.close, plain.close, "close hit target never moves");
        assert!(plain.close_available);
        assert!(app.close_available);
        assert_eq!(app.icon, Some([20.0, 7.0, 14.0, 14.0]));
        assert_eq!(
            app.status,
            Some([198.0, 6.0, 16.0, 16.0]),
            "the status canvas mirrors the icon on the trailing edge"
        );
        assert_eq!(app.label, [38.0, 5.5, 144.0, 17.0]);
        assert_eq!(plain.label, [20.0, 5.5, 180.0, 17.0]);
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

        // The hover-only ✕'s slot is reserved unconditionally: identical geometry
        // with and without an icon.
        assert_eq!(
            native_tab_content_layout(220.0, 14.0, false, false),
            plain,
            "close-slot geometry is independent of whether the ✕ is painted"
        );

        let compact = native_tab_content_layout(48.0, 14.0, true, true);
        assert_eq!(compact.close, app.close);
        assert!(
            !compact.close_available,
            "compact title reclaims close space"
        );
        assert_eq!(compact.icon, None, "title wins before decorative identity");
        assert_eq!(
            compact.status, None,
            "states remain semantic when space is gone"
        );
        assert_eq!(compact.label, [4.0, 5.5, 40.0, 17.0]);
        assert!(
            compact.label[2] >= 0.0,
            "narrow label clips, never underflows"
        );

        let phone = native_tab_content_layout(63.0, 14.0, true, false);
        assert!(!phone.close_available);
        assert_eq!(phone.icon, None, "full Settings title wins at phone width");
        assert_eq!(phone.label, [4.0, 5.5, 55.0, 17.0]);

        let app_with_room = native_tab_content_layout(109.0, 14.0, true, false);
        assert!(!app_with_room.close_available);
        assert_eq!(app_with_room.icon, Some([4.0, 7.0, 14.0, 14.0]));
        assert_eq!(app_with_room.label, [22.0, 5.5, 65.0, 17.0]);

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

    #[test]
    fn all_independent_states_share_one_compact_shape_coded_canvas() {
        let metadata = TabStripMetadata {
            icon: Some(TabIconKind::Settings),
            dirty: true,
            busy: true,
            attention: ChipLevel::Wait,
            conn: None,
            closable: true,
            drop_target: false,
        };
        assert!(metadata.has_status());
        assert_eq!(metadata.status_count(), 3);
        assert_eq!(tab_status_center(0, 3), 3.5);
        assert_eq!(tab_status_center(1, 3), 8.0);
        assert_eq!(tab_status_center(2, 3), 12.5);
        let primitives = status_primitives(metadata);
        assert_eq!(
            primitives
                .iter()
                .filter(|primitive| matches!(primitive, TabIconPrimitive::Dot { .. }))
                .count(),
            1,
            "dirty is a solid dot"
        );
        assert_eq!(
            primitives
                .iter()
                .filter(|primitive| matches!(primitive, TabIconPrimitive::RoundedRect { .. }))
                .count(),
            1,
            "busy is a hollow ring"
        );
        assert_eq!(
            primitives
                .iter()
                .filter(|primitive| matches!(primitive, TabIconPrimitive::Line { .. }))
                .count(),
            4,
            "attention is a diamond"
        );
        let raster = rasterize_icon(&primitives, [255, 180, 64]);
        let (pixels, remainder) = raster.as_chunks::<4>();
        assert!(remainder.is_empty(), "tab icon raster has complete pixels");
        assert!(pixels.iter().any(|pixel| pixel[3] != 0));
    }

    /// The horizontal span a primitive can ink, including stroke width.
    fn primitive_x_extent(primitive: &TabIconPrimitive) -> (f32, f32) {
        match *primitive {
            TabIconPrimitive::Line { from, to, width } => (
                from[0].min(to[0]) - width * 0.5,
                from[0].max(to[0]) + width * 0.5,
            ),
            TabIconPrimitive::RoundedRect { rect, width, .. } => {
                (rect[0] - width * 0.5, rect[0] + rect[2] + width * 0.5)
            }
            TabIconPrimitive::Dot { center, radius } => (center[0] - radius, center[0] + radius),
            TabIconPrimitive::Triangle { points } => (
                points.iter().map(|p| p[0]).fold(f32::INFINITY, f32::min),
                points
                    .iter()
                    .map(|p| p[0])
                    .fold(f32::NEG_INFINITY, f32::max),
            ),
        }
    }

    /// Four marks pack the 16-unit canvas on distinct centres, and the shared shrink
    /// keeps every shape inside its own 4-unit pitch.
    #[test]
    fn four_marks_pack_distinct_centers_without_overlap() {
        let metadata = TabStripMetadata {
            icon: None,
            dirty: true,
            busy: true,
            attention: ChipLevel::Wait,
            conn: Some(TabConnRole::Both),
            closable: true,
            drop_target: false,
        };
        assert!(metadata.has_status_kind(TabStatusKind::Connection));
        assert_eq!(metadata.status_count(), 4);
        let centers: Vec<f32> = (0..4).map(|i| tab_status_center(i, 4)).collect();
        assert_eq!(centers, [2.0, 6.0, 10.0, 14.0]);
        for pair in centers.windows(2) {
            assert!(pair[0] < pair[1], "centres stay distinct and ordered");
        }
        // Three-or-fewer keeps the historical full-size geometry bit-exact.
        assert_eq!(tab_status_mark_scale(3), 1.0);
        assert_eq!(tab_status_mark_scale(4), 0.75);
        // Kind order is fixed (TAB_STATUS_KINDS), so the flat primitive list
        // partitions per mark: dot, ring, 4 diamond lines, 2 hourglass fills.
        let primitives = status_primitives(metadata);
        assert_eq!(primitives.len(), 1 + 1 + 4 + 2);
        let pitch_of = [0, 1, 2, 2, 2, 2, 3, 3];
        for (primitive, mark) in primitives.iter().zip(pitch_of) {
            let (lo, hi) = primitive_x_extent(primitive);
            let center = centers[mark];
            // Shape bodies stay on their own pitch; only a diamond stroke's round
            // cap may touch the boundary.
            assert!(
                lo >= center - 2.5 && hi <= center + 2.5,
                "{primitive:?} escapes its pitch around {center}"
            );
            assert!(lo >= 0.0 && hi <= 16.0, "{primitive:?} escapes the canvas");
        }
        let raster = rasterize_icon(&primitives, [213, 219, 255]);
        assert!(raster.as_chunks::<4>().0.iter().any(|pixel| pixel[3] != 0));
    }

    fn span_of(primitives: &[TabIconPrimitive]) -> (f32, f32) {
        primitives
            .iter()
            .map(primitive_x_extent)
            .fold((f32::INFINITY, f32::NEG_INFINITY), |(lo, hi), (a, b)| {
                (lo.min(a), hi.max(b))
            })
    }

    fn two_marks(
        dirty: bool,
        busy: bool,
        attention: ChipLevel,
        conn: Option<TabConnRole>,
    ) -> TabStripMetadata {
        TabStripMetadata {
            icon: None,
            dirty,
            busy,
            attention,
            conn,
            closable: true,
            drop_target: false,
        }
    }

    /// The pair a working Claude tab wears when it needs a look: the busy ring
    /// and the attention diamond. The native strip mitres the diamond's corners
    /// (AppKit's default join), which reaches `w/2 * (sqrt(2) - 1)` past the
    /// round-join extent the IR reports, so the IR gap must beat the floor by
    /// that. One design unit is one point, two device pixels on a 2x display.
    #[test]
    fn a_busy_ring_and_an_attention_diamond_keep_clear_of_each_other() {
        let metadata = two_marks(false, true, ChipLevel::Wait, None);
        assert_eq!(metadata.status_count(), 2);
        let primitives = status_primitives(metadata);
        // Kind order is fixed: the ring (one RoundedRect) precedes the diamond
        // (four Lines).
        assert!(matches!(
            primitives[0],
            TabIconPrimitive::RoundedRect { .. }
        ));
        assert_eq!(primitives.len(), 1 + 4);
        let ring = span_of(&primitives[..1]);
        let diamond = span_of(&primitives[1..]);
        let mitre_extra = 1.15 * 0.5 * (std::f32::consts::SQRT_2 - 1.0);
        let gap = (diamond.0 - mitre_extra) - ring.1;
        assert!(
            gap >= 1.0,
            "ring and diamond gap {gap} units (ring {ring:?}, diamond {diamond:?})"
        );
        assert!(
            ring.0 >= 0.0 && diamond.1 + mitre_extra <= 16.0,
            "both inside the 16-unit canvas"
        );
    }

    /// Every other pair of two marks keeps ground between them too.
    #[test]
    fn every_two_mark_pair_keeps_clear() {
        let outbound = Some(TabConnRole::Outbound);
        for (dirty, busy, attention, conn) in [
            (true, true, ChipLevel::Off, None),
            (true, false, ChipLevel::Wait, None),
            (true, false, ChipLevel::Off, outbound),
            (false, true, ChipLevel::Off, outbound),
            (false, false, ChipLevel::Wait, outbound),
            (false, false, ChipLevel::Wait, Some(TabConnRole::Both)),
        ] {
            let metadata = two_marks(dirty, busy, attention, conn);
            assert_eq!(metadata.status_count(), 2);
            let primitives = status_primitives(metadata);
            // Partition the flat list per mark by kind order: dot 1, ring 1,
            // diamond 4 lines, connection 1 / 3 / 2 primitives.
            let mut spans = Vec::new();
            let mut at = 0usize;
            let mut take = |n: usize| {
                spans.push(span_of(&primitives[at..at + n]));
                at += n;
            };
            if dirty {
                take(1);
            }
            if busy {
                take(1);
            }
            if attention != ChipLevel::Off {
                take(4);
            }
            if let Some(role) = conn {
                take(match role {
                    TabConnRole::Outbound => 1,
                    TabConnRole::Inbound => 3,
                    TabConnRole::Both => 2,
                });
            }
            assert_eq!(at, primitives.len());
            let gap = spans[1].0 - spans[0].1;
            assert!(
                gap >= 0.25,
                "{metadata:?}: gap {gap} between {:?} and {:?}",
                spans[0],
                spans[1]
            );
        }
    }

    /// Every connection role draws its own §4 shape from the ONE shared IR the
    /// in-grid rasterizer consumes (the native strip mirrors the same centres,
    /// scale, and geometry in `toolbar.rs::paint_identity`): outbound a filled
    /// up-triangle, inbound a hollow line-built down-triangle, both a filled
    /// hourglass — pairwise distinct on pixels, like the app icons.
    #[test]
    fn connection_role_shapes_are_distinct_and_deterministic() {
        let conn_only = |role| TabStripMetadata {
            icon: None,
            dirty: false,
            busy: false,
            attention: ChipLevel::Off,
            conn: Some(role),
            closable: true,
            drop_target: false,
        };
        let outbound = status_primitives(conn_only(TabConnRole::Outbound));
        assert_eq!(outbound.len(), 1, "outbound is one filled triangle");
        assert!(matches!(outbound[0], TabIconPrimitive::Triangle { .. }));
        let inbound = status_primitives(conn_only(TabConnRole::Inbound));
        assert_eq!(inbound.len(), 3, "inbound is a hollow (line) triangle");
        assert!(
            inbound
                .iter()
                .all(|primitive| matches!(primitive, TabIconPrimitive::Line { .. }))
        );
        let both = status_primitives(conn_only(TabConnRole::Both));
        assert_eq!(both.len(), 2, "both is an hourglass of two fills");
        assert!(
            both.iter()
                .all(|primitive| matches!(primitive, TabIconPrimitive::Triangle { .. }))
        );
        // A lone mark centres like every other lone mark.
        for primitives in [&outbound, &inbound, &both] {
            let (lo, hi) = primitives.iter().map(primitive_x_extent).fold(
                (f32::INFINITY, f32::NEG_INFINITY),
                |(alo, ahi), (lo, hi)| (alo.min(lo), ahi.max(hi)),
            );
            assert!((lo + hi - 16.0).abs() < 0.01, "role mark centres on 8.0");
        }
        let rasters: Vec<Vec<u8>> = [&outbound, &inbound, &both]
            .iter()
            .map(|primitives| rasterize_icon(primitives, [213, 219, 255]))
            .collect();
        assert!(
            rasters
                .iter()
                .all(|r| r.as_chunks::<4>().0.iter().any(|p| p[3] != 0))
        );
        for (i, a) in rasters.iter().enumerate() {
            for b in rasters.iter().skip(i + 1) {
                assert_ne!(a, b, "role shapes must be tellable apart");
            }
        }
    }

    /// A segment below `MIN_SEG_WITH_CLOSE` drops the close `x` but still selects.
    /// The rule is per segment: 9 columns and 3 tabs seat the two-seat minimum
    /// ([`STRIP_MIN_SEATS`]), one of them too narrow for a close column.
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

    /// Title truncation: a long title is cut to `max` cells with a trailing `…`; a
    /// short title is returned unchanged; `max == 0` is empty.
    #[test]
    fn truncate_title_ellipsis() {
        assert_eq!(truncate_title("bash", 10), "bash");
        assert_eq!(truncate_title("a-very-long-title", 5), "a-ve…");
        assert_eq!(truncate_title("anything", 0), "");
        assert_eq!(truncate_title("anything", 1), "…");
        // Exactly-fits is not truncated.
        assert_eq!(truncate_title("abcde", 5), "abcde");
    }

    /// Truncation budgets display cells: three CJK chars are six cells, and a wide
    /// char that cannot fit before the `…` is dropped whole.
    #[test]
    fn truncate_title_counts_display_cells_not_chars() {
        // 6 cells fit exactly: no cut, though it is only 3 chars.
        assert_eq!(truncate_title("日本語", 6), "日本語");
        // 5 cells: 日(2) + 本(2) fill the 4-cell keep budget, `…` takes the 5th.
        assert_eq!(truncate_title("日本語", 5), "日本…");
        // 4 cells: 本 would straddle the 3-cell keep budget → dropped whole,
        // so the ellipsis lands a cell early ("日…" is 3 of the 4).
        assert_eq!(truncate_title("日本語", 4), "日…");
        assert_eq!(truncate_title("日本語", 2), "…");
        // Mixed narrow/wide: "a🚀b" is 1+2+1 = 4 cells.
        assert_eq!(truncate_title("a🚀bc", 5), "a🚀bc");
        assert_eq!(truncate_title("a🚀bcd", 4), "a🚀…");
    }

    /// The tail cut mirrors the head cut: same display-cell budget and whole-glyph
    /// rule, with the `…` marking the dropped head.
    #[test]
    fn truncate_title_tail_keeps_the_end_of_the_title() {
        assert_eq!(truncate_title_tail("abcdef", 6), "abcdef", "a fit is kept");
        assert_eq!(truncate_title_tail("abcdef", 4), "…def");
        assert_eq!(truncate_title_tail("abcdef", 1), "…");
        assert_eq!(truncate_title_tail("abcdef", 0), "");
        // The shared prompt prefix goes, the cwd tail survives, and the cut lands on
        // a word boundary.
        assert_eq!(
            truncate_title_tail("user@m17-tower: ~/aterm", 10),
            "…~/aterm"
        );
        // No mid-word cut (`…eady in aterm`): the boundary snap keeps whole words.
        assert_eq!(truncate_title_tail("Ready in aterm", 13), "…in aterm");
        assert_eq!(truncate_title_tail("Typing a command", 14), "…a command");
        // A boundary costing more than half the surviving text is refused.
        assert_eq!(
            truncate_title_tail("verylongtokenwithoutspaces/x", 12),
            "…outspaces/x",
            "the /x tail is too small a survivor to justify the boundary"
        );
        // 日本語 is 6 cells. Budget 5 keeps 本語 + `…`; budget 4 drops 本 whole and
        // the ellipsis sits a cell early.
        assert_eq!(truncate_title_tail("日本語", 5), "…本語");
        assert_eq!(truncate_title_tail("日本語", 4), "…語");
        // Budget 2 leaves a 1-cell keep no width-2 glyph can use.
        assert_eq!(truncate_title_tail("日本語", 2), "…");
    }

    /// A cut path keeps its end, cut only where a name begins ([`path_tail`]).
    #[test]
    fn a_cut_path_keeps_its_directory_and_never_a_fragment_of_a_name() {
        // Loners: the longest tail that starts a name and fits.
        for (subject, room, expected) in [
            // Painted `…ndows\Temp` and `C:\Windows…`.
            ("C:\\Windows\\Temp", 12, Some("…Temp")),
            ("C:\\Windows\\Temp", 13, Some("…Windows\\Temp")),
            // Painted `…32\drivers`.
            ("C:\\Windows\\System32\\drivers", 12, Some("…drivers")),
            (
                "C:\\Windows\\System32\\drivers",
                17,
                Some("…System32\\drivers"),
            ),
            // Painted `…les\Git`.
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
            // The selected chip at 480×360 painted `~\aterm…` beside `~\aterm`.
            (
                "~\\aterm\\crates\\aterm-cli\\src",
                14,
                Some("…aterm-cli\\src"),
            ),
            ("~\\aterm\\crates\\aterm-cli\\src", 13, Some("…cli\\src")),
            ("~\\aterm\\crates\\aterm-cli\\src", 4, Some("…src")),
            ("/home/person/work/api", 9, Some("…work/api")),
            // The leaf seats and `…` with it does not: the directory's name,
            // whole (painted `…ystem32` at 716×539).
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
        // Kin that end differently change nothing, and an ANCESTOR is not cut
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

        // THE WINDOW'S KIN, seated or not — and a twin is not kin: text
        // cannot tell twins apart, their position does.
        let subjects = ["~\\aterm", gui, cli, sys, gui, "cargo test -p crates"];
        assert_eq!(path_chip_label(1, &subjects, 7), "…gui");
        assert_eq!(path_chip_label(2, &subjects, 7), "…cli");
        assert_eq!(path_chip_label(4, &subjects, 7), "…gui");
        assert_eq!(path_chip_label(3, &subjects, 7), "System…");
        // CONTROL: with no `aterm-cli\src` in the window, `…src` names gui
        // alone.
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

    /// Two sibling crates' `src` directories, two shells in `~\aterm` and one in
    /// `System32`, over every width: a sibling's chip names the differing component
    /// (`gui`/`cli`) wherever four cells fit, every path cut is honest
    /// ([`honest_path_cut`]), and `System32` reads the same alone or clustered
    /// with `C:\Windows\Temp` or `C:\Windows\System32\drivers`.
    #[test]
    fn deep_sibling_directories_keep_the_component_that_differs_at_every_width() {
        let gui = "~\\aterm\\crates\\aterm-gui\\src";
        let cli = "~\\aterm\\crates\\aterm-cli\\src";
        let sys = "C:\\Windows\\System32";
        let strip = |titles: [&str; 5]| -> Vec<String> { titles.map(String::from).to_vec() };
        let family = strip([gui, cli, "~\\aterm", "~\\aterm", sys]);
        // `System32` alone, and `System32` sharing its head with a neighbour
        // the cluster pass admits it with (`C:\Windows\`, eleven cells).
        let alone = strip(["~\\aterm", "~\\aterm", "~\\aterm", "~\\aterm", sys]);
        let with_temp = strip(["~\\aterm", "C:\\Windows\\Temp", "~\\aterm", "~\\aterm", sys]);
        let with_drivers = strip([
            "~\\aterm",
            "C:\\Windows\\System32\\drivers",
            "~\\aterm",
            "~\\aterm",
            sys,
        ]);
        let mut named = 0usize;
        let mut clustered = 0usize;
        for cols in 30..=240u16 {
            for active in [0usize, 1, 4] {
                let segments = layout_segments(cols, family.len(), active, false);
                let with_family = seated(&segments, &family, active);
                for (tab, label) in &with_family {
                    let avail = chip_avail(&segments, *tab);
                    let (subject, name) = match tab {
                        0 => (gui, "gui"),
                        1 => (cli, "cli"),
                        4 => (sys, "System32"),
                        _ => continue,
                    };
                    let at = format!("{cols} cols, active {active}, tab {tab} ({avail} cells)");
                    assert!(
                        honest_path_cut(*tab, subject, label),
                        "{at}: {label:?} is not an honest cut of {subject:?}"
                    );
                    // Wherever the distinguishing name fits (`…gui` four cells, the
                    // leaf `System32` eight), the chip paints it.
                    if avail >= if *tab == 4 { name.len() } else { 4 } {
                        named += 1;
                        assert!(
                            label.contains(name),
                            "{at}: {label:?} lost the name that tells it apart"
                        );
                    }
                }
                // STABLE: System32's chip reads the same beside any
                // neighbours (same widths — only the other titles differ).
                let sys_of = |titles: &[String]| {
                    seated(&segments, titles, active)
                        .into_iter()
                        .find(|(tab, _)| *tab == 4)
                        .map(|(_, label)| label)
                };
                let reference = sys_of(&alone);
                for (neighbours, titles) in [
                    ("the sibling pair", &family),
                    ("C:\\Windows\\Temp", &with_temp),
                    ("C:\\Windows\\System32\\drivers", &with_drivers),
                ] {
                    assert_eq!(
                        sys_of(titles),
                        reference,
                        "{cols} cols, active {active}: System32's label turned on {neighbours}"
                    );
                }
                // Non-vacuity: count frames where System32 and Temp were both cut
                // with a head dominating the window (the cluster branch).
                let both_cut = [1usize, 4].iter().all(|&tab| {
                    segments.iter().any(|seg| seg.kind == TabHit::Select(tab))
                        && strip_display_cells(&with_temp[tab]) > chip_avail(&segments, tab)
                });
                if both_cut
                    && "C:\\Windows\\".len() * 2
                        >= chip_avail(&segments, 1).min(chip_avail(&segments, 4))
                {
                    clustered += 1;
                }
            }
        }
        assert!(
            named > 100,
            "vacuous: only {named} sibling chips were checked"
        );
        assert!(
            clustered > 20,
            "vacuous: System32 clustered with Temp in only {clustered} frames"
        );
    }

    /// A windowed strip (fifteen tabs, five chips a page, a 7-cell title window)
    /// with the `aterm-gui\src` and `aterm-cli\src` tabs on different pages. At
    /// every width and selection: gui and cli chips name `gui`/`cli` wherever four
    /// cells fit (or carry their position); every path cut is honest; a path chip's
    /// label is one per (tab, width) across pages and selections; and `System32`
    /// reads the same when the Temp tab moves to `C:\Users`.
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
        let (mut apart, mut floor) = (0usize, 0usize);
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
                    // By value at the floor's windows (seven cells chip-card, eight
                    // padless) and the few below them.
                    if (4..=8).contains(&avail) && matches!(tab, 4..=6) && !ordinal {
                        floor += 1;
                        let expected = match (tab, avail) {
                            (4, 8) => "…gui\\src".to_string(),
                            (4, _) => "…gui".to_string(),
                            (5, 8) => "…cli\\src".to_string(),
                            (5, _) => "…cli".to_string(),
                            (_, 8) => "System32".to_string(),
                            // `System…` in seven cells.
                            (_, _) => truncate_title("System32", avail),
                        };
                        assert_eq!(*label, expected, "{at}: the floor's window");
                    }
                    // A twin's ordinal is its position among twins cut beside
                    // it — the one label here allowed to turn on its page.
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
        assert!(
            floor > 20,
            "vacuous: the floor's windows were seen {floor} times"
        );
    }

    /// One label pin for both band geometries: the chip-card interior pad
    /// ([`tab_content_layout`]) costs one title cell, so a pin at the budget edge
    /// differs by one glyph between the chip-card band (Linux) and padless bands
    /// (macOS/Windows). Pin both.
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
    /// composes with no cap (`ChromeSurface::TabStrip`), so the pass sees whole
    /// titles and each chip keeps the component that differs.
    ///
    /// One case per region a fixed cut discards: cwds differing in the leaf (a head
    /// cut collapses them), two worktree ids inside a middle cut's `[48, N-48)`
    /// window (bare cwd rung and a prompt-set title), and trees differing past
    /// both. The fixture guard pins which cuts collapse each pair, and each
    /// collapsing cut is replayed as a negative control.
    #[test]
    fn deep_siblings_past_the_chrome_cap_stay_tellable_apart() {
        use crate::app_config::{Config, TitleFormat};
        use crate::title_summary::{ChromeSurface, Coordinator, whole_label};
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

        let coordinator = Coordinator::new(None);
        let config = Config::default();
        let head_cut: fn(&str) -> String =
            |t| format!("{}…", t.graphemes().take(96).collect::<String>());
        let middle_cut: fn(&str) -> String = |t| whole_label(t).into_owned();
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
                "~/src/github.com/alabsystems/aterm/.claude/worktrees/lane-bec3123b-89c-10/crates/aterm-gui/src/title_summary/fixtures/model".to_string(),
                "~/src/github.com/alabsystems/aterm/.claude/worktrees/lane-bec3123b-89c-25/crates/aterm-gui/src/title_summary/fixtures/model".to_string(),
                "10",
                "25",
                &[80],
                [false, true],
            ),
            (
                "user@m17-tower: ~/aterm/.claude/worktrees/lane-bec3123b-89c-10/crates/aterm-gui/src/title_summary/description.rs".to_string(),
                "user@m17-tower: ~/aterm/.claude/worktrees/lane-bec3123b-89c-11/crates/aterm-gui/src/title_summary/description.rs".to_string(),
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
                .map(|t| {
                    coordinator.compose(
                        None,
                        t,
                        None,
                        TitleFormat::Title,
                        &config,
                        ChromeSurface::TabStrip,
                    )
                })
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

    /// Four shells whose prompts set `user@host: <cwd>` do not truncate into four
    /// identical `user@host: …` labels: colliding labels are re-cut from the tail,
    /// while distinct heads keep the head cut.
    #[test]
    fn a_label_is_never_only_shared_furniture() {
        // A label made only of the suffix every tab shares (`…· Ready`) names
        // nothing; each must show its own place.
        let titles = [
            "user@m17-tower: ~/aterm · Ready".to_string(),
            "user@m17-tower: $HOME/trust · Ready".to_string(),
            "user@m17-tower: ~/ay · Ready".to_string(),
            "user@m17-tower: /tmp · Typing a command".to_string(),
        ];
        let segments = layout_segments(80, titles.len(), 3, false);
        let labels = distinct_chip_labels(&segments, &titles, None, 3, None);
        let resolved: Vec<String> = labels
            .into_iter()
            .enumerate()
            .map(|(i, l)| l.unwrap_or_else(|| titles[i].clone()))
            .collect();
        for (i, label) in resolved.iter().enumerate().take(3) {
            let painted = label.trim_start_matches('…').trim_start();
            assert_ne!(
                painted, "· Ready",
                "tab {i} paints only the shared furniture: {resolved:?}"
            );
            assert!(
                !painted.is_empty() && painted != "Ready",
                "tab {i} must name itself, got {label:?} in {resolved:?}"
            );
        }
        for (i, a) in resolved.iter().enumerate() {
            for (j, b) in resolved.iter().enumerate() {
                if i != j {
                    assert_ne!(a, b, "tabs {i} and {j} must differ: {resolved:?}");
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

        // Composed labels share their ending (`title · <activity>`); the group's
        // common suffix is shed first, so the kept tail ends at the cwd.
        let composed = [
            "user@m17-tower: ~/aterm · Typing a command".to_string(),
            "user@m17-tower: $HOME/trust · Typing a command".to_string(),
            "user@m17-tower: ~ · Typing a command".to_string(),
            "user@m17-tower: ~/aterm/crates · Typing a command".to_string(),
        ];
        let segments = layout_segments(80, composed.len(), 0, false);
        let labels = distinct_chip_labels(&segments, &composed, None, 0, None);
        let resolved: Vec<String> = labels.into_iter().map(Option::unwrap).collect();
        assert!(
            resolved[0].ends_with("aterm"),
            "the shared activity suffix is shed, the cwd tail survives: {:?}",
            resolved[0]
        );
        assert!(resolved[1].ends_with("trust"), "{:?}", resolved[1]);
        assert!(resolved[2].ends_with('~'), "{:?}", resolved[2]);
        assert!(resolved[3].ends_with("crates"), "{:?}", resolved[3]);

        // One dialect per strip: on a wider band the head cuts stop colliding, but
        // the shared-head cluster still flips the whole family together.
        let segments = layout_segments(130, composed.len(), 0, false);
        let labels = distinct_chip_labels(&segments, &composed, None, 0, None);
        let resolved: Vec<String> = labels.into_iter().map(Option::unwrap).collect();
        assert_eq!(
            resolved,
            ["…~/aterm", "…$HOME/trust", "…~", "…~/aterm/crates"],
            "every member of the shared-prompt family speaks the tail dialect"
        );

        // Identical titles: the suffix shed must not empty them, and the non-active
        // twin is labelled by position (`2`) with whatever tail fits, while the
        // active twin keeps as much title as fits.
        let twins = ["same shell".to_string(), "same shell".to_string()];
        let segments = layout_segments(30, twins.len(), 0, false);
        let labels = distinct_chip_labels(&segments, &twins, None, 0, None);
        for label in labels.iter().flatten() {
            assert!(!label.is_empty());
            assert_ne!(label.as_str(), "…", "a bare ellipsis names nothing");
        }
        assert_eq!(
            labels[0].as_deref(),
            Some("…shell"),
            "the ACTIVE twin keeps the title — it is the tab being read"
        );
        assert_eq!(
            labels[1].as_deref(),
            Some(on_band("2 · …ell", "2 · …hell")),
            "the other twin is addressable by ordinal, tail attached where room allows"
        );

        // Distinct heads stay head-cut: nothing collided, nothing flips.
        let distinct = [
            "alpha-service logs".to_string(),
            "beta-service logs".to_string(),
        ];
        let segments = layout_segments(24, distinct.len(), 0, false);
        let labels = distinct_chip_labels(&segments, &distinct, None, 0, None);
        for (label, raw) in labels.iter().zip(&distinct) {
            let label = label.as_deref().unwrap();
            assert!(
                label.chars().next() == raw.chars().next(),
                "an already-distinguishing head is kept: {label:?}"
            );
        }

        // And the PAINTED row carries the resolved labels (the pass feeds the
        // painter, not just itself) — chip-card band only.
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

    /// Eight shells in one directory, and tabs sharing no head at all: every chip
    /// sheds its composed state clause ([`state_clause_bytes`]) before the cut, so
    /// no label is `…in aterm` or `…a command`. The selected shell is named the
    /// same whether idle or mid-command. Asserts only what a label must not say,
    /// so the chip-card pad cannot move it.
    #[test]
    fn a_loner_sheds_its_state_clause_with_no_cluster_to_carry_it() {
        let titles: Vec<String> = [
            "alpha", "bravo", "charlie", "delta", "echo", "foxtrot", "golf", "hotel",
        ]
        .iter()
        .enumerate()
        .map(|(i, subject)| {
            if i % 2 == 0 {
                format!("{subject} · Ready in aterm")
            } else {
                format!("{subject} · Typing a command")
            }
        })
        .collect();
        // No two of these share a head, so every tab is its own cluster.
        let segments = layout_segments(80, titles.len(), 0, false);
        let resolved = seated_labels(&segments, &titles, 0);
        assert!(!resolved.is_empty(), "the strip seats something");
        for label in &resolved {
            assert!(
                !label.contains("Ready")
                    && !label.contains("Typing")
                    && !label.contains("command")
                    && !label.contains("aterm"),
                "a chip with no cluster still names its tab, not the moment it \
                 is having: {resolved:?}"
            );
        }
    }

    #[test]
    fn eight_shells_in_one_cwd_name_their_subject_not_their_state() {
        let idle: Vec<String> = (0..8)
            .map(|_| "claude · Ready in aterm".to_string())
            .collect();
        // The second capture: the selected tab is mid-command, so its state
        // clause — and nothing else — differs from the other seven.
        let mut typing = idle.clone();
        typing[7] = "claude · Typing a command".to_string();
        for (capture, titles) in [("idle", idle), ("typing", typing)] {
            // 160 cols: eight equal 20-cell chips, a 14-cell title window each
            // — room for a name, never for the whole composed title.
            let segments = layout_segments(160, titles.len(), 7, false);
            let resolved: Vec<String> = distinct_chip_labels(&segments, &titles, None, 7, None)
                .into_iter()
                .map(Option::unwrap)
                .collect();
            assert_eq!(
                resolved[7], "claude",
                "{capture}: the selected chip names its tab, where it painted \
                 the activity phrase: {resolved:?}"
            );
            for (i, label) in resolved.iter().enumerate().take(7) {
                assert_eq!(
                    label,
                    &format!("{} · claude", i + 1),
                    "{capture}: a quiet chip carries its position AND what it \
                     is, where it painted a bare number: {resolved:?}"
                );
            }

            // At 80 cols eight chips would keep four title cells each; the strip
            // seats the selection's page instead ([`STRIP_SEATS_FLOOR`]): four
            // 19-column chips, a `‹` mark, and every chip carries its position and
            // whole subject.
            let segments = layout_segments(80, titles.len(), 7, false);
            let seats = seated(&segments, &titles, 7);
            assert_eq!(
                seats.iter().map(|(tab, _)| *tab).collect::<Vec<_>>(),
                [4, 5, 6, 7],
                "{capture}: the window is the selection's PAGE"
            );
            assert_eq!(
                strip_overflow(&segments, titles.len()),
                StripOverflow {
                    left: Some(0),
                    right: None
                },
                "{capture}: and says so at the edge it clips"
            );
            for (tab, label) in &seats {
                if *tab == 7 {
                    assert_eq!(label, "claude", "{capture}: {seats:?}");
                    continue;
                }
                assert_eq!(
                    label,
                    &format!("{} · claude", tab + 1),
                    "{capture}: a quiet chip carries its position AND what it \
                     is, where it painted a bare number: {seats:?}"
                );
                assert_ne!(
                    label,
                    &(tab + 1).to_string(),
                    "{capture}: the bare ordinal is the defect: {seats:?}"
                );
            }

            // At no width does a chip spend its window on the state alone.
            for cols in [80u16, 98, 120, 160] {
                let segments = layout_segments(cols, titles.len(), 7, false);
                let resolved = seated_labels(&segments, &titles, 7);
                for (i, label) in resolved.iter().enumerate() {
                    assert!(
                        !label.contains("Ready")
                            && !label.contains("Typing")
                            && !label.contains("command"),
                        "{capture} at {cols} cols: chip {i} spends its window on \
                         the state: {resolved:?}"
                    );
                }
            }
        }
    }

    /// Subject before state holds at every width, not only where cluster admission
    /// (shared head covering half the window) happens to catch a chip. A chip may
    /// show its state only after naming its subject; the assertion is positional.
    #[test]
    fn a_cut_chip_names_its_subject_before_its_state_at_every_width() {
        let idle: Vec<String> = (0..8)
            .map(|_| "claude · Ready in aterm".to_string())
            .collect();
        let mut typing = idle.clone();
        typing[7] = "claude · Typing a command".to_string();
        let mut chips = 0usize;
        for (capture, titles) in [("idle", idle), ("typing", typing)] {
            for cols in 40..=240u16 {
                let segments = layout_segments(cols, titles.len(), 7, false);
                for (tab, label) in seated(&segments, &titles, 7) {
                    chips += 1;
                    assert!(
                        !label.is_empty() && label != "…",
                        "{capture} at {cols} cols: chip {tab} says nothing ({label:?})"
                    );
                    assert!(
                        !label.chars().all(|c| c.is_ascii_digit()),
                        "{capture} at {cols} cols: chip {tab} is the bare ordinal {label:?}"
                    );
                    let Some(state) = ["Ready", "Typing", "command", "in aterm"]
                        .iter()
                        .filter_map(|word| label.find(word))
                        .min()
                    else {
                        continue;
                    };
                    let subject = label.find("claude").unwrap_or_else(|| {
                        panic!(
                            "{capture} at {cols} cols: chip {tab} spends its window on the \
                             state and never names its subject: {label:?}"
                        )
                    });
                    assert!(
                        subject < state,
                        "{capture} at {cols} cols: chip {tab} leads with the moment it is \
                         having: {label:?}"
                    );
                }
            }
        }
        assert!(chips > 1000, "vacuous: only {chips} chips were labelled");
    }

    /// The state shed also reaches the loner arm: a family that clusters plus an
    /// unrelated shell mid-build that shares neither head nor ending. Under one
    /// dialect it tail-cuts with the family, and its own state clause is shed first
    /// ([`cut_core`]), so the tail lands on `build`.
    #[test]
    fn the_one_dialect_flip_cuts_the_subject_not_the_state() {
        let titles = [
            "Settings.toml · Ready".to_string(),
            "Settings.toml · Ready".to_string(),
            "cargo build · Building the project".to_string(),
            "README.md · Ready".to_string(),
            "Setup.sh · Ready".to_string(),
        ];
        // 48 columns seats three 14-column chips: two clustered twins and a loner
        // on one page, the geometry where the flip is observable.
        let segments = layout_segments(48, titles.len(), 0, false);
        let seats = seated(&segments, &titles, 0);
        assert_eq!(
            seats.iter().map(|(tab, _)| *tab).collect::<Vec<_>>(),
            [0, 1, 2],
            "the fixture's geometry: family, family, loner on one page"
        );
        assert_eq!(
            seats[2].1, "…build",
            "the loner is named by what it is RUNNING, where it painted the \
             phrase for what it is doing: {seats:?}"
        );
        for (tab, label) in &seats {
            for moment in ["Ready", "Building", "project"] {
                assert!(
                    !label.contains(moment),
                    "chip {tab} spends its window on the state: {seats:?}"
                );
            }
        }
        assert!(
            seats[0].1.contains("toml") && seats[1].1.contains("toml"),
            "the family still names the file it shares: {seats:?}"
        );
        assert!(
            seats[1].1.starts_with("2 · "),
            "and the non-active twin is still addressable: {seats:?}"
        );
    }

    /// Two shells in one directory differing only in state are twins by subject: the
    /// non-active one takes its position, never `…s passed` / `…e project`.
    #[test]
    fn tabs_that_differ_only_in_state_are_told_apart_by_position() {
        let titles = [
            "~/work/api · Tests passed".to_string(),
            "~/work/api · Building the project".to_string(),
        ];
        let segments = layout_segments(40, titles.len(), 0, false);
        let resolved: Vec<String> = distinct_chip_labels(&segments, &titles, None, 0, None)
            .into_iter()
            .map(Option::unwrap)
            .collect();
        assert_ne!(
            resolved[0], resolved[1],
            "still tellable apart: {resolved:?}"
        );
        for (i, label) in resolved.iter().enumerate() {
            assert!(
                label.contains("api"),
                "tab {i} names the directory both tabs are in: {resolved:?}"
            );
            assert!(
                !label.contains("passed") && !label.contains("Building"),
                "tab {i} is named after a moment: {resolved:?}"
            );
        }
        assert!(
            resolved[1].starts_with("2 · "),
            "the one text cannot name takes its position: {resolved:?}"
        );
    }

    /// A state clause is shed only when a subject survives, and only the last one
    /// (the half `compose_parts` appends; `title_summary`'s
    /// `a_composed_tab_label_splits_back_where_the_strip_looks_for_it` pins the
    /// composition side).
    #[test]
    fn a_state_clause_is_shed_only_when_a_subject_survives_it() {
        let subject = |title: &str| -> String {
            title[..title.len() - state_clause_bytes(title)].to_string()
        };
        assert_eq!(state_clause_bytes("claude"), 0, "no separator, no clause");
        assert_eq!(
            state_clause_bytes(" · Ready in aterm"),
            0,
            "a shed that would leave no subject is not a shed"
        );
        assert_eq!(state_clause_bytes("claude · "), 0, "an empty clause");
        assert_eq!(subject("claude · Ready in aterm"), "claude");
        assert_eq!(
            subject("user@m17-tower: ~/aterm · Typing a command"),
            "user@m17-tower: ~/aterm"
        );
        assert_eq!(
            subject("mux · pane 2 · Ready"),
            "mux · pane 2",
            "only the LAST clause goes: a title that carries the separator \
             itself keeps everything the composition did not append"
        );
    }

    /// Ten shells in one cwd under pressure: each non-active twin is labelled by its
    /// 1-based position with a tail of the subject, and the active twin keeps as
    /// much subject as fits (`…~/aterm`). At an ordinary width the ordinal is never
    /// bare, because the strip seats fewer chips instead ([`STRIP_SEATS_FLOOR`]).
    ///
    /// The window is a page ([`strip_window`]): selecting a seated tab leaves the
    /// strip identical apart from the exemption, and only leaving the page turns it.
    ///
    /// Both band geometries ([`on_band`]): the chip-card pad costs one title cell,
    /// so the ordinal's tail is one glyph longer on padless bands.
    #[test]
    fn byte_identical_tabs_under_pressure_are_addressable_by_ordinal() {
        let titles: Vec<String> = (0..10)
            .map(|_| "user@m17-tower: ~/aterm · Typing a command".to_string())
            .collect();
        // 80 cols, 10 tabs → the band seats six chips at the floor, which pages
        // ten tabs into two fives; the five it cannot reach are marked.
        let segments = layout_segments(80, titles.len(), 0, false);
        let resolved = seated(&segments, &titles, 0);
        assert_eq!(
            resolved,
            [
                (0, "…~/aterm".to_string()),
                (1, on_band("2 · …aterm", "2 · …/aterm").to_string()),
                (2, on_band("3 · …aterm", "3 · …/aterm").to_string()),
                (3, on_band("4 · …aterm", "4 · …/aterm").to_string()),
                (4, on_band("5 · …aterm", "5 · …/aterm").to_string()),
            ],
            "the active twin keeps as much SUBJECT as its width fits — the cwd \
             its prompt names, not the `…command` its state clause used to \
             leave there — and every other seated twin carries its number AND a \
             piece of that same cwd"
        );
        assert_eq!(
            strip_overflow(&segments, titles.len()).right,
            Some(75),
            "the five tabs the window does not reach are marked, not silently gone"
        );
        for (i, (_, a)) in resolved.iter().enumerate() {
            for (j, (_, b)) in resolved.iter().enumerate() {
                if i != j {
                    assert_ne!(a, b, "chips {i} and {j} must be tellable apart");
                }
            }
        }

        // Selecting a seated chip moves nothing: tabs 0..4 lay out the same strip.
        for active in 0..5 {
            assert_eq!(
                layout_segments(80, titles.len(), active, false),
                segments,
                "selecting seated tab {active} re-anchored the strip"
            );
        }
        // ...and only the exemption moves: the selected twin keeps the subject,
        // every other seated twin takes its own number.
        let resolved = seated(&segments, &titles, 4);
        assert_eq!(
            resolved,
            [
                (0, on_band("1 · …aterm", "1 · …/aterm").to_string()),
                (1, on_band("2 · …aterm", "2 · …/aterm").to_string()),
                (2, on_band("3 · …aterm", "3 · …/aterm").to_string()),
                (3, on_band("4 · …aterm", "4 · …/aterm").to_string()),
                (4, "…~/aterm".to_string()),
            ],
            "the numbers are the TABS' — `switch_tab_3` reaches the chip \
             painted `3 · …aterm`"
        );

        // Selecting tab 5, the first off this page, seats the second page.
        let segments = layout_segments(80, titles.len(), 5, false);
        assert_eq!(
            seated(&segments, &titles, 5)
                .iter()
                .map(|(tab, _)| *tab)
                .collect::<Vec<_>>(),
            [5, 6, 7, 8, 9],
            "the page after the one that was showing"
        );
        assert_eq!(
            strip_overflow(&segments, titles.len()),
            StripOverflow {
                left: Some(0),
                right: None
            },
            "the last page marks the tabs behind it and nothing ahead"
        );

        // A wider strip spends the page on wider chips; each ordinal's tail is cut
        // from the same half of the title the selected chip keeps.
        let segments = layout_segments(120, titles.len(), 0, false);
        let resolved = seated_labels(&segments, &titles, 0);
        assert_eq!(resolved[1], "2 · …~/aterm");
        assert_eq!(
            resolved.last().unwrap(),
            "5 · …~/aterm",
            "the page's last seat, still numbered by its TAB and not by its \
             seat: {resolved:?}"
        );
    }

    /// Pins [`ordinal_chip_label`]'s claims: `switch_tab_1`..`switch_tab_9` parse on
    /// every platform, no built-in default binds them, and the hardcoded Super
    /// chord suite is gated on `app_input::HARDCODED_SUPER_CHORDS` (off on Linux).
    /// Changing any of these means re-reading that doc.
    #[test]
    fn only_the_first_nine_ordinals_name_an_action() {
        use crate::keybinding::{Action, Keybindings};
        for n in 1..=9u8 {
            assert_eq!(
                Action::parse(&format!("switch_tab_{n}")),
                Some(Action::SwitchTab(n)),
                "the first nine positions are addressable by action"
            );
        }
        for n in [10u32, 11, 20, 99, 100] {
            assert_eq!(
                Action::parse(&format!("switch_tab_{n}")),
                None,
                "no action names tab {n}: its ordinal is a POSITION, and the \
                 label's doc says exactly that"
            );
        }
        for (chord, action) in Keybindings::PLATFORM_DEFAULT_PAIRS {
            assert!(
                !matches!(Action::parse(action), Some(Action::SwitchTab(_))),
                "a default now binds {chord:?} to {action:?} — the ordinal's doc \
                 says the jump-to-tab keystroke is the user's to write"
            );
        }
        assert_eq!(
            crate::app_input::HARDCODED_SUPER_CHORDS,
            !cfg!(target_os = "linux"),
            "the hardcoded 1..=9 chord is compiled OUT on Linux — the ordinal's \
             doc names exactly the platforms where the number is also an address"
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
                .map(|_| "user@m17-tower: ~/aterm · Typing a command".to_string())
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
    /// when one member in a different state collapses the cluster's shed, the tail
    /// is re-cut instead of spending the window on `· Ready`.
    ///
    /// Not on macOS: it paints no band ([`STRIP_IS_CHROME_BAND`]), and this pins
    /// the chip-card avail (14). The cross-band `on_band` tests cover macOS.
    #[cfg(not(target_os = "macos"))]
    #[test]
    fn an_ordinal_tail_is_never_the_shared_furniture() {
        let titles = [
            "~/work/aterm · Ready".to_string(),
            "~/work/aterm · Ready".to_string(),
            "~/work/trust · Ready".to_string(),
            "~/work/other · Typing a command".to_string(),
        ];
        let segments = layout_segments(80, titles.len(), 0, false);
        let resolved: Vec<String> = distinct_chip_labels(&segments, &titles, None, 0, None)
            .into_iter()
            .map(Option::unwrap)
            .collect();
        let twin = &resolved[1];
        assert!(
            twin.starts_with("2 · "),
            "the non-active twin is labelled by its position: {twin:?}"
        );
        assert!(
            !twin.contains("Ready"),
            "the tail is this tab's own text, not the state every chip on the \
             strip shares: {twin:?}"
        );
        assert!(
            twin.contains("aterm"),
            "and what it spends the tail on is the cwd that names it: {twin:?}"
        );
        // Fixture guard: the cluster's shed collapsed, so the word-boundary cut at
        // the tail's budget lands in the furniture.
        let seg = segments
            .iter()
            .find(|seg| matches!(seg.kind, TabHit::Select(1)))
            .expect("the twin has a chip");
        let layout = tab_content_layout(seg, PLAIN_TAB);
        let avail = usize::from(layout.title_end.saturating_sub(layout.title_start));
        let naive = truncate_title_tail(&titles[1], avail.saturating_sub(4));
        assert!(
            naive.contains("Ready") && !naive.contains("aterm"),
            "fixture: the naive tail is pure furniture: {naive:?}"
        );
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
    /// Pinned by value per window, then end to end for a composed title and the
    /// idle title `~\aterm`.
    #[test]
    fn twins_keep_whole_components_or_the_head_at_every_width() {
        let title = "~\\aterm · Ready in aterm";
        let subject = "~\\aterm";
        let siblings = [title, title];
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
                ordinal_chip_label(1, title, subject, "", avail, &siblings, twin_tail),
                ordinal,
                "window {avail}: the non-active twin"
            );
            assert_eq!(
                twin_tail(subject, "", avail),
                active,
                "window {avail}: the active twin"
            );
        }
        for avail in 0..=40usize {
            let label = ordinal_chip_label(1, title, subject, "", avail, &siblings, twin_tail);
            assert!(strip_display_cells(&label) <= avail, "{avail}: {label:?}");
            let painted = label.strip_prefix("2 · ").unwrap_or(&label);
            assert!(
                painted.is_empty() || painted == "2" || is_twin_context(subject, painted),
                "window {avail}: {label:?} is not context for {subject:?}"
            );
            let active = twin_tail(subject, "", avail);
            assert!(strip_display_cells(&active) <= avail, "{avail}: {active:?}");
            assert!(
                avail < 2 || is_twin_context(subject, &active),
                "window {avail}: {active:?} is not context for {subject:?}"
            );
        }
        // The audit's windows, by value.
        assert_eq!(
            ordinal_chip_label(1, title, subject, "", 9, &siblings, twin_tail),
            "2 · ~\\at…",
            "painted `2 · …term` on glass"
        );
        assert_eq!(
            ordinal_chip_label(0, title, subject, "", 7, &siblings, twin_tail),
            "1 · ~\\…",
            "painted `1 · …rm` at 400x300"
        );
        // ...where the family's own cut is what split the name.
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
        // A NAME is not a path: a family of names keeps the family's cut.
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
        for composed in [title, subject] {
            let titles: Vec<String> = (0..3).map(|_| composed.to_string()).collect();
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
                        if label == composed {
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
        }
        assert!(cut > 100, "vacuous: only {cut} chips were cut");
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
                format!("{dir} · Ready"),
                format!("{dir} · Ready"),
                "cargo build --release · Building the project".to_string(),
                "README.md · Ready".to_string(),
                "Setup.sh · Ready".to_string(),
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
        let titles = strip("~/wave/api");
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
            "user@m17-tower: ~/src/aterm-gui · Ready".to_string(),
            "user@m17-tower: ~/src/aterm-gui · Ready".to_string(),
            "user@m17-tower: ~/src/aterm-cli · Ready".to_string(),
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
            .map(|_| "user@m17-tower: ~/aterm · Typing a command".to_string())
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

    /// Titles for the one-dialect test: once any cluster on a pressure strip flips
    /// to tail cuts, the cut loners flip too (no `…oml` beside `REA…`); a roomy
    /// strip keeps head cuts for distinct heads.
    ///
    /// Twins and loners are interleaved: the strip seats a page ([`strip_window`]),
    /// and a sorted list would give all-twin or all-stranger pages.
    ///
    /// Pins cover both band geometries ([`on_band`]): the 13-column seat keeps eight
    /// title cells on a chip-card band and nine on padless ones.
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

        // With ` · <activity>` composed onto every chip, unrelated loners share an
        // ending; the flip sheds that furniture (shared with two or more chips)
        // first, but not a one-chip coincidence like the trailing `d`.
        let composed: Vec<String> = one_dialect_titles()
            .iter()
            .map(|t| format!("{t} · Ready"))
            .collect();
        let segments = layout_segments(70, composed.len(), 9, false);
        let resolved = seated_labels(&segments, &composed, 9);
        assert_eq!(
            resolved,
            [
                "…build",
                on_band("7 · …oml", "7 · …toml"),
                "Setup.sh",
                on_band("9 · …oml", "9 · …toml"),
                "….bak"
            ],
            "the composed activity is shed strip-wide, the loners stay tellable"
        );

        // Two visible cells per chip: `README.md` and `cargo build` both tail-cut
        // to `…d`, so colliding candidates keep their head cuts (`R…`/`c…`).
        //
        // Built by hand: [`layout_segments`] now seats a window instead of this
        // geometry ([`STRIP_SEATS_FLOOR`]), but the label pass is a pure function of
        // the segments it is handed and must hold for any of them.
        let segments: Vec<TabSegment> = std::iter::once(TabSegment {
            start_col: 0,
            end_col: 18,
            close_col: Some(16),
            connector_col: None,
            kind: TabHit::Select(0),
            solo: false,
        })
        .chain((1..10u16).map(|i| {
            let start = 18 + (i - 1) * 6;
            TabSegment {
                start_col: start,
                end_col: start + 6,
                close_col: Some(start + 4),
                connector_col: None,
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
    /// accent underline) from inactive ones (dimmed fg that recedes) and paints the
    /// title chars and close `✕` into the segment.
    #[test]
    fn paint_active_inactive_and_title() {
        let theme = Theme::default();
        let cols = 40usize;
        let mut row = vec![strip_cell(' ', &strip_colors(theme), StripRole::Inactive); cols];
        let segs = layout_segments(cols as u16, 2, 0, false);
        let titles = vec!["zsh".to_string(), "vim".to_string()];
        // Tab 0 is under the pointer, so its hover-only ✕ is painted.
        paint_strip(&mut row, &segs, &titles, Some(0), 0, theme);
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
        // Tab 0 is active: raised bg, bold full-strength text, and the accent
        // underline except on a chip-card band, where the filled card alone marks
        // the selection.
        assert_ne!(
            row[card0].bg, bg_rgb,
            "active tab bg is raised above the body"
        );
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
        // The title 'z','s','h' appears at the content start (after the pad —
        // and after the roomy card's interior pad on a chip-card band).
        let ts = (t0.start_col + 1 + u16::from(STRIP_CHIP_CARDS)) as usize;
        assert_eq!(row[ts].ch, 'z');
        assert_eq!(row[ts + 1].ch, 's');
        assert_eq!(row[ts + 2].ch, 'h');
        // The close ✕ is present for a wide tab.
        let cx = t0.close_col.unwrap() as usize;
        assert_eq!(row[cx].ch, '✕');
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

    /// `cols == 0` (degenerate) yields no segments and never panics.
    #[test]
    fn zero_cols_no_segments() {
        assert!(layout_segments(0, 3, 0, false).is_empty());
    }

    /// A wide char in a tab title paints as a real lead + continuation pair, never
    /// a `·` placeholder or an overlapping narrow cell: emoji, CJK and a width-2
    /// BMP symbol.
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
        }
    }

    /// A wide title on a narrow segment drops a straddling wide char whole rather
    /// than bleeding over the close `✕` or the neighbouring chip.
    #[test]
    fn a_wide_char_never_straddles_the_title_boundary() {
        let theme = Theme::default();
        // Narrow equal shares: 24 cols / 2 tabs → small title spans.
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

    /// The rename field paints wide chars as themselves: the caret block covers
    /// both cells of a wide char, and the continuation carries the `wide` flag.
    #[test]
    fn the_rename_field_paints_wide_chars_with_a_full_width_caret() {
        let theme = Theme::default();
        let metadata = [
            TabStripMetadata::from_presentation(&crate::tab_model::TabPresentation::terminal("a")),
            TabStripMetadata::from_presentation(&crate::tab_model::TabPresentation::terminal("b")),
        ];
        let segments = layout_segments_with_metadata(60, 2, &metadata, 0, false);
        let mut row = vec![blank_cell(theme); 60];
        paint_strip_with_metadata(
            &mut row,
            &segments,
            &["one".to_string(), "two".to_string()],
            &metadata,
            StripPaint {
                hovered: None,
                new_tab_hovered: false,
                version_hovered: false,
                subtitle: None,
                rename: Some(StripRenameField {
                    tab: 0,
                    // Caret ON the wide char: "日" starts at byte 2.
                    text: "ab日cd",
                    cursor: 2,
                }),
            },
            0,
            theme,
            None,
        );
        let colors = crate::chrome_band::band_colors(theme);
        let lead_col = row
            .iter()
            .position(|cell| cell.ch == '日')
            .expect("the wide char is painted as itself");
        let lead = row[lead_col];
        let tail = row[lead_col + 1];
        assert_eq!(lead.bg, colors.caret, "caret block on the lead…");
        assert_eq!(lead.fg, colors.field_bg, "…in reverse video");
        assert!(tail.wide, "the continuation is flagged for the renderer");
        assert_eq!(tail.bg, colors.caret, "and the block covers both cells");
        // The narrow neighbours are ordinary field cells.
        assert_eq!(row[lead_col - 1].ch, 'b');
        assert_eq!(row[lead_col + 2].ch, 'c');
        assert_eq!(row[lead_col + 2].bg, colors.field_bg);
    }

    /// The solo band centres a CJK title by display width and paints
    /// lead+continuation pairs.
    #[test]
    fn the_solo_band_centres_wide_titles_by_display_cells() {
        let theme = Theme::default();
        let metadata = [TabStripMetadata::from_presentation(
            &crate::tab_model::TabPresentation::terminal("日本語"),
        )];
        let segments = forced_solo_segments(40, &metadata);
        let band = segments[0];
        let mut row = vec![blank_cell(theme); 40];
        paint_strip_with_metadata(
            &mut row,
            &segments,
            &["日本語".to_string()],
            &metadata,
            StripPaint::default(),
            0,
            theme,
            None,
        );
        let first = row[..band.end_col as usize]
            .iter()
            .position(|cell| cell.ch != ' ')
            .expect("the title is drawn");
        assert_eq!(row[first].ch, '日');
        assert!(row[first + 1].wide, "lead + continuation, not narrow CJK");
        assert_eq!(row[first + 2].ch, '本');
        assert_eq!(row[first + 4].ch, '語');
        // Centred by its SIX display cells: ink from `first` to `first+6`.
        let span = usize::from(band.end_col - band.start_col);
        let left = first - usize::from(band.start_col);
        let right = span - (left + 6);
        assert!(
            left.abs_diff(right) <= 1,
            "centred by display width (left {left}, right {right})"
        );
    }

    /// Once the equal share falls under the legibility floor, no seated chip is
    /// compressed below it: sixteen tabs on ~130 columns seat a page of equal chips
    /// rather than compressing every title to ~3 chars. No active reservation
    /// applies within a page ([`strip_window`]).
    #[test]
    fn under_pressure_the_active_tab_keeps_a_readable_floor() {
        let cols = 130u16;
        let active = 7usize;
        let segs = layout_segments(cols, 16, active, false);
        let tabs: Vec<_> = segs
            .iter()
            .filter(|s| matches!(s.kind, TabHit::Select(_)))
            .collect();
        assert!(!tabs.is_empty());
        let active_w = tabs
            .iter()
            .find(|s| s.kind == TabHit::Select(active))
            .map(|s| s.end_col - s.start_col)
            .expect("the active tab is placed");
        assert!(
            active_w >= PREFERRED_MIN_TAB_COLS,
            "the selected chip clears the legibility floor: {active_w} cols"
        );
        // Every other seated chip has the same width: the strip seats a page and
        // marks the rest ([`STRIP_SEATS_FLOOR`], [`strip_window`]).
        for seg in &tabs {
            let w = seg.end_col - seg.start_col;
            assert_eq!(
                w, active_w,
                "a seated page divides its band equally: {:?} has {w} cols \
                 against the selection's {active_w}",
                seg.kind
            );
            assert!(
                w >= PREFERRED_MIN_TAB_COLS,
                "...and never below the legibility floor: {:?} has {w} cols",
                seg.kind
            );
        }
        assert!(
            tabs.len() < 16,
            "the band cannot seat sixteen legible chips, so it seats a window"
        );
        // The page depends only on width and tab count: every selection it seats
        // lays out the identical strip.
        for other in tabs.iter().filter_map(|seg| match seg.kind {
            TabHit::Select(i) => Some(i),
            _ => None,
        }) {
            assert_eq!(
                layout_segments(cols, 16, other, false),
                segs,
                "selecting seated tab {other} re-laid the strip"
            );
        }
        // Geometry stays sane: disjoint, ordered, inside the band, close column
        // present on the active tab (it is the one being read and closed).
        for pair in tabs.windows(2) {
            assert!(pair[0].end_col <= pair[1].start_col);
        }
        assert!(tabs.last().unwrap().end_col <= cols);
        assert!(
            tabs.iter()
                .find(|s| s.kind == TabHit::Select(active))
                .unwrap()
                .close_col
                .is_some(),
            "the floored active tab keeps its close affordance"
        );
        for seg in &tabs {
            assert_eq!(
                hit_test(&segs, seg.start_col),
                Some(seg.kind),
                "every placed tab remains individually clickable"
            );
        }
    }

    /// Three realistic strips: eight shells in one directory (identical titles), eight
    /// different jobs, and two pairs of twins among strangers. Tests below run all
    /// three.
    fn realistic_strips() -> [(&'static str, Vec<String>); 3] {
        let same: Vec<String> = (0..8)
            .map(|_| "claude · Ready in aterm".to_string())
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
        .map(|subject| format!("{subject} · Ready in aterm"))
        .collect();
        let mixed: Vec<String> = [
            "claude",
            "claude",
            "vim TODO",
            "~/aterm",
            "~/aterm",
            "cargo test",
            "git log",
            "claude",
        ]
        .iter()
        .map(|subject| format!("{subject} · Ready in aterm"))
        .collect();
        [("same", same), ("distinct", distinct), ("mixed", mixed)]
    }

    /// A painted label with its FURNITURE taken off — a leading ordinal and the
    /// elision marks — leaving only the characters the chip spent on text.
    fn painted_text(label: &str) -> &str {
        label
            .trim_start_matches(|c: char| c.is_ascii_digit())
            .trim_start_matches(' ')
            .trim_start_matches('\u{00b7}')
            .trim_start_matches(' ')
            .trim_matches('…')
    }

    /// Whether this chip says something of its own tab: a cut of the title's
    /// subject (the tail dialect and the ordinal's tail) or a head cut of the whole
    /// composed title, rather than the repeated state clause.
    fn says_its_own(label: &str, title: &str) -> bool {
        let painted = painted_text(label);
        let subject = title
            .rsplit_once(" \u{00b7} ")
            .map_or(title, |(subject, _)| subject);
        !painted.is_empty() && (subject.contains(painted) || title.starts_with(painted))
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
                        // The window is one CONTIGUOUS run of tabs.
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
    /// elision mark, never the state clause.
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
                        "{where_}: the selection spends its chip on the state \
                         every chip repeats: {label:?}"
                    );
                    // The subject is the title's first word; wherever the chip has
                    // room for the word and its elision mark, it shows that word.
                    let subject = titles[active].split(' ').next().unwrap();
                    let mark = usize::from(label != titles[active].as_str());
                    if strip_display_cells(label) >= strip_display_cells(subject) + mark {
                        assert!(
                            label.contains(subject),
                            "{where_}: room for {subject:?} and it painted {label:?}"
                        );
                    }
                }
            }
        }
    }

    /// Eight tabs on 80 columns in all three vocabularies: every seated chip carries
    /// text of its own title (not just a number) and no two say the same thing. The
    /// window seats five 14-column chips and marks the rest.
    #[test]
    fn eight_tabs_on_eighty_columns_seat_labels_that_say_something() {
        for (set, titles) in realistic_strips() {
            let segs = layout_segments(80, titles.len(), 7, false);
            let chips = seated(&segs, &titles, 7);
            assert!(
                chips.len() < titles.len(),
                "{set}: 80 columns cannot label eight tabs, so it must not pretend to"
            );
            assert!(
                chips.len() >= 4,
                "{set}: ...and it must still be a strip: {chips:?}"
            );
            assert_eq!(
                strip_overflow(&segs, titles.len()).left,
                Some(0),
                "{set}: the tabs before the window are marked, not silently gone"
            );
            for (tab, label) in &chips {
                assert_ne!(
                    label,
                    &(tab + 1).to_string(),
                    "{set}: chip {tab} is a bare ordinal — the defect: {chips:?}"
                );
                // Some of this chip's own title survives: the ordinal may lead but
                // never stands alone, and never before the state clause.
                assert!(
                    says_its_own(label, &titles[*tab]),
                    "{set}: chip {tab} keeps nothing of {:?}: {label:?}",
                    titles[*tab]
                );
            }
            for (i, (_, a)) in chips.iter().enumerate() {
                for (j, (_, b)) in chips.iter().enumerate() {
                    if i != j {
                        assert_ne!(a, b, "{set}: two seated chips paint {a:?}");
                    }
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
    /// strip without panicking.
    #[test]
    fn extreme_tab_counts_stay_inside_the_band() {
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

    /// Separator rule: dividers separate two quiet chips only; never before the
    /// first chip or beside the active or hovered chip. The first two clauses match
    /// `TabGeometry::separates` (`toolbar.rs`); the hover clause mirrors the native
    /// strip's per-view hover pill.
    #[test]
    fn separators_divide_quiet_chips_and_never_crowd_active_or_hover() {
        let drawn = |active: usize, hovered: Option<usize>| {
            (0..5)
                .map(|index| strip_separates(index, active, hovered))
                .collect::<Vec<_>>()
        };
        // No hover: exactly the native rule's table.
        assert_eq!(drawn(0, None), [false, false, true, true, true]);
        assert_eq!(drawn(2, None), [false, true, false, false, true]);
        assert_eq!(drawn(4, None), [false, true, true, true, false]);
        // Hover suppresses its own pair of edges too.
        assert_eq!(drawn(0, Some(3)), [false, false, true, false, false]);
        assert_eq!(drawn(4, Some(0)), [false, false, true, true, false]);
        // Hovering the active chip changes nothing the selection had not.
        assert_eq!(drawn(2, Some(2)), drawn(2, None));
    }

    /// Separators as painted: on a chip-card band quiet tabs are cards separated by
    /// bare-band gutters and no `│` exists; in the cell-glyph dialect quiet chips
    /// show hairlines on their leading pad cells. The pad still selects the tab, and
    /// hovering an inactive chip washes it.
    #[cfg(not(target_os = "macos"))]
    #[test]
    fn the_band_draws_separators_and_a_hover_wash_between_quiet_chips() {
        let theme = Theme::default();
        let colors = strip_colors(theme);
        let titles = [
            "one".to_string(),
            "two".to_string(),
            "three".to_string(),
            "four".to_string(),
        ];
        let segs = layout_segments(80, 4, 0, false);
        let tabs: Vec<_> = segs
            .iter()
            .filter(|s| matches!(s.kind, TabHit::Select(_)))
            .copied()
            .collect();

        let mut row = vec![blank_cell(theme); 80];
        paint_strip(&mut row, &segs, &titles, None, 0, theme);
        if STRIP_CHIP_CARDS {
            // Chip-card band: no `│` anywhere; each segment's leading cell is a
            // gutter of bare band and quiet chips carry their own card.
            assert!(
                row.iter().all(|cell| cell.ch != '│'),
                "the chip-card band separates with gutters, not pipe glyphs"
            );
            for tab in &tabs {
                assert_eq!(
                    row[tab.start_col as usize].bg, colors.band_bg,
                    "every card leads with a bare-band gutter column"
                );
            }
            assert_eq!(
                row[tabs[1].start_col as usize + 1].bg,
                colors.chip_bg,
                "a quiet tab is a chip card, a half-step off the band"
            );
            assert_ne!(colors.chip_bg, colors.band_bg, "visibly so");
        } else {
            // Cell-glyph dialect: tabs 2 and 3 are quiet neighbours of quiet tabs
            // (hairlines); tab 1 touches the active chip (none).
            assert_eq!(row[tabs[1].start_col as usize].ch, ' ');
            assert_eq!(row[tabs[2].start_col as usize].ch, '│');
            assert_eq!(row[tabs[3].start_col as usize].ch, '│');
            assert_eq!(
                row[tabs[2].start_col as usize].fg,
                colors.seam.expect("a chrome band always has a seam"),
                "the divider is the seam's ink — one structural material"
            );
        }
        // Paint-only: the gutter/divider column is still the tab's own click
        // target.
        assert_eq!(
            hit_test(&segs, tabs[2].start_col),
            Some(TabHit::Select(2)),
            "the leading pad cell moves no hit geometry"
        );

        // Hover tab 2: it takes the wash (and in the cell-glyph dialect, the
        // rules beside it clear).
        let mut row = vec![blank_cell(theme); 80];
        paint_strip(&mut row, &segs, &titles, Some(2), 0, theme);
        let washed = row[tabs[2].start_col as usize + 1];
        assert_eq!(washed.bg, colors.hover_bg, "the hovered chip is washed");
        assert_ne!(colors.hover_bg, colors.band_bg, "visibly — off the band");
        assert_ne!(
            colors.hover_bg, colors.active_bg,
            "but never the selection's raise"
        );
        if STRIP_CHIP_CARDS {
            assert_ne!(
                colors.hover_bg, colors.chip_bg,
                "and a step above the quiet chip, so hover visibly answers"
            );
        }
        assert!(!washed.bold, "hover keeps the inactive vocabulary");
        assert_eq!(washed.underline, UnderlineStyle::None);
        if !STRIP_CHIP_CARDS {
            assert_eq!(
                row[tabs[2].start_col as usize].ch, ' ',
                "its own rule yields"
            );
            assert_eq!(
                row[tabs[3].start_col as usize].ch, ' ',
                "and so does its trailing neighbour's"
            );
        }
        // The hover-revealed ✕ sits ON the wash — it finally has a backing.
        let cx = tabs[2].close_col.expect("wide tab reserves a close column");
        assert_eq!(row[cx as usize].ch, '✕');
        assert_eq!(row[cx as usize].bg, colors.hover_bg);
        // The ACTIVE chip is never washed, hovered or not.
        let mut row = vec![blank_cell(theme); 80];
        paint_strip(&mut row, &segs, &titles, Some(0), 0, theme);
        let card0 = tabs[0].start_col as usize + usize::from(STRIP_CHIP_CARDS);
        assert_eq!(
            row[card0].bg, colors.active_bg,
            "hovering the selected tab leaves it selected"
        );
    }

    /// Perceptual luma of a strip tone — the same cheap weights [`bg_is_light`]
    /// classifies with, so the surface tests and the classifier cannot disagree.
    #[cfg(not(target_os = "macos"))]
    fn luma(c: [u8; 3]) -> f32 {
        0.299 * f32::from(c[0]) + 0.587 * f32::from(c[1]) + 0.114 * f32::from(c[2])
    }

    /// The strip is three separable surfaces on every bundled scheme: the band is
    /// visibly off the grid, the card visibly off the band, and the card's step
    /// above the band is no smaller than the band's step off the grid.
    #[cfg(not(target_os = "macos"))]
    #[test]
    fn the_band_is_distinct_from_the_grid_and_the_card_is_distinct_from_the_band() {
        for name in aterm_types::scheme::builtin_names() {
            let s = aterm_types::scheme::builtin(name).expect("builtin exists");
            let tp = s.to_theme_parts();
            let theme = Theme {
                fg: tp.fg,
                bg: tp.bg,
                cursor: tp.cursor,
                selection: tp.selection,
            };
            let c = strip_colors(theme);
            let grid = [
                ((tp.bg >> 16) & 0xff) as u8,
                ((tp.bg >> 8) & 0xff) as u8,
                (tp.bg & 0xff) as u8,
            ];
            assert_ne!(
                c.band_bg, grid,
                "{name}: the strip band is the terminal background — no chrome at all"
            );
            assert_ne!(
                c.active_bg, c.band_bg,
                "{name}: the active card sank into the band"
            );
            let band_step = (luma(c.band_bg) - luma(grid)).abs();
            let card_step = (luma(c.active_bg) - luma(c.band_bg)).abs();
            assert!(
                card_step >= band_step,
                "{name}: the selected card ({card_step:.1}) reads fainter than the \
                 band's own edge ({band_step:.1}) — the rejected merged-card look"
            );
        }
    }

    /// Every cell of the last strip row carries a rule closing it against the grid,
    /// and cells already carrying the selection accent keep it.
    #[cfg(not(target_os = "macos"))]
    #[test]
    fn the_last_strip_row_is_sealed_against_the_grid_without_eating_the_accent() {
        let theme = Theme::default();
        let cols = 40usize;
        let segments = layout_segments(cols as u16, 2, 0, false);
        let mut row = vec![blank_cell(theme); cols];
        let titles = ["a".to_string(), "b".to_string()];
        paint_strip(&mut row, &segments, &titles, None, 0, theme);
        let accent = strip_colors(theme).accent;
        let active_underlines = row
            .iter()
            .filter(|cell| cell.underline_color == Some(accent))
            .count();
        if STRIP_CHIP_CARDS {
            // The chip-card selection is a filled card, so the seal is one unbroken
            // hairline.
            assert_eq!(
                active_underlines, 0,
                "no accent rule: the filled card carries the selection"
            );
        } else {
            assert!(active_underlines > 0, "the active chip has its accent rule");
        }
        seal_strip_bottom(&mut row, theme);
        assert!(
            row.iter()
                .all(|cell| cell.underline == UnderlineStyle::Single),
            "every cell of the closing row carries an edge"
        );
        assert_eq!(
            row.iter()
                .filter(|cell| cell.underline_color == Some(accent))
                .count(),
            active_underlines,
            "and the seal neither ate nor spread the selection accent"
        );
        if STRIP_CHIP_CARDS {
            let seam = strip_colors(theme).seam.expect("a band has a seam");
            assert!(
                row.iter().all(|cell| cell.underline_color == Some(seam)),
                "one seam tone, unbroken across cards and band alike"
            );
        }
    }

    /// Off macOS a lone tab paints a real chip (raised card, selection rule, close
    /// column), because the OS caption already carries the window title.
    #[cfg(not(target_os = "macos"))]
    #[test]
    fn a_lone_tab_paints_a_chip_where_the_os_caption_carries_the_title() {
        let theme = Theme::default();
        let metadata = [TabStripMetadata::from_presentation(
            &crate::tab_model::TabPresentation::terminal("aterm"),
        )];
        let segments = layout_segments_with_metadata(40, 1, &metadata, 0, false);
        assert!(!segments[0].solo);
        let mut row = vec![blank_cell(theme); 40];
        paint_strip_with_metadata(
            &mut row,
            &segments,
            &["aterm".to_string()],
            &metadata,
            StripPaint::default(),
            0,
            theme,
            None,
        );
        let colors = strip_colors(theme);
        let title = row
            .iter()
            .find(|cell| cell.ch == 'a')
            .expect("the title is drawn");
        assert_eq!(title.bg, colors.active_bg, "on a raised card");
        if STRIP_CHIP_CARDS {
            assert_eq!(
                title.underline,
                UnderlineStyle::None,
                "the filled card IS the selection — no per-cell rule"
            );
            let cx = segments[0].close_col.expect("a lone chip keeps its ✕");
            assert_eq!(
                row[cx as usize].ch, '✕',
                "and the selected card's close mark is resident"
            );
        } else {
            assert_eq!(
                title.underline,
                UnderlineStyle::Single,
                "with the selection rule under it"
            );
            assert_eq!(title.underline_color, Some(colors.accent));
        }
    }

    /// On a chrome band the `+` takes the theme's raise (like the `↻`), so
    /// `active_tab_color` recolours only the selected tab.
    #[cfg(not(target_os = "macos"))]
    #[test]
    fn the_new_tab_button_is_raised_and_ignores_the_active_tab_override() {
        let theme = Theme::default();
        let colors = strip_colors(theme);
        let plus = strip_cell('+', &colors, StripRole::NewTab);
        if STRIP_CHIP_CARDS {
            // On the chip-card band the `+` is a quiet chip with bright ink, not the
            // selected tab's tone.
            assert_eq!(plus.bg, colors.chip_bg, "the `+` is a chip button");
            assert_eq!(plus.fg, colors.chip_button_fg, "with full-strength ink");
            assert_ne!(
                plus.bg, colors.active_bg,
                "never the selection's own surface"
            );
        } else {
            assert_eq!(plus.bg, colors.raise_bg, "the `+` is a raised button");
        }
        assert_ne!(plus.bg, colors.band_bg, "not a flat label on the band");
        assert_eq!(
            plus.underline,
            UnderlineStyle::None,
            "but not a SELECTED one — that rule belongs to the active tab"
        );
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
        // Zero-width chars keep a cell (the caret's char-index ↔ column
        // invariant), exactly as the pre-width-aware painter treated them.
        assert_eq!(strip_char_cells('\u{FE0F}'), 1);
        assert_eq!(
            strip_display_cells("日本語"),
            6,
            "display width, not char count"
        );
    }

    /// Every built-in theme keeps the strip's text above the 3.0:1 UI-text floor:
    /// active-tab text on the raised card, the `+`, and inactive labels on the band,
    /// on dark and light themes alike. The default theme's inactive labels are
    /// pinned higher.
    #[test]
    fn strip_contrast_meets_wcag_aa() {
        use aterm_types::Rgb;
        let rgb = |c: [u8; 3]| Rgb::new(c[0], c[1], c[2]);
        // Assertions (i)-(iii) below compare against the raw theme fg, which is the
        // strip's ink only while no OS palette forces chrome; fail clearly if one
        // leaked onto this worker. The High-Contrast twin is
        // `strip_contrast_holds_under_forced_high_contrast_palettes`.
        assert_eq!(
            crate::chrome_band::forced_chrome(),
            None,
            "the theme-derived strip is only defined with no OS-forced palette"
        );
        for name in aterm_types::scheme::builtin_names() {
            let s = aterm_types::scheme::builtin(name).expect("builtin exists");
            let tp = s.to_theme_parts();
            let theme = Theme {
                fg: tp.fg,
                bg: tp.bg,
                cursor: tp.cursor,
                selection: tp.selection,
            };
            let c = strip_colors(theme);
            // Each pair is the ink and the surface `strip_cell` actually puts
            // together for that role.
            let active = rgb(c.active_fg).contrast(rgb(c.active_bg));
            let (new_tab_fg, new_tab_bg) = if STRIP_CHIP_CARDS {
                (c.chip_button_fg, c.chip_bg)
            } else if STRIP_IS_CHROME_BAND {
                (c.raise_fg, c.raise_bg)
            } else {
                (c.fg, c.band_bg)
            };
            let new_tab = rgb(new_tab_fg).contrast(rgb(new_tab_bg));
            // Inactive labels on the band, held to the 3:1 floor (muted schemes
            // cannot reach AA 4.5: Solarized Light's full-strength text is ~4.13:1
            // and its dim ~3.20:1). The default theme is pinned higher below.
            let inactive = rgb(c.inactive_fg).contrast(rgb(c.band_bg));
            assert!(
                active >= 3.0,
                "{name}: active-tab text contrast {active:.2} < 3.0:1"
            );
            assert!(
                new_tab >= 3.0,
                "{name}: '+' affordance contrast {new_tab:.2} < 3.0:1"
            );
            assert!(
                inactive >= 3.0,
                "{name}: inactive-tab label contrast {inactive:.2} < 3.0:1"
            );
            // The hover wash (chrome band only) is a step toward the ink, so it is
            // the pair likeliest to slip under the floor on a muted scheme.
            if STRIP_IS_CHROME_BAND {
                let hover = rgb(c.hover_fg).contrast(rgb(c.hover_bg));
                assert!(
                    hover >= 3.0,
                    "{name}: hovered-tab label contrast {hover:.2} < 3.0:1"
                );
            }
            // The quiet chip's card is one more surface its dim label stands on
            // (chip-card band only), and it too is a step TOWARD the ink.
            if STRIP_CHIP_CARDS {
                let chip = rgb(c.chip_fg).contrast(rgb(c.chip_bg));
                assert!(
                    chip >= 3.0,
                    "{name}: quiet-chip label contrast {chip:.2} < 3.0:1"
                );
            }
            // The `+` under the pointer: full-strength ink on the hover wash.
            if STRIP_IS_CHROME_BAND {
                let button = rgb(c.button_hover_fg).contrast(rgb(c.hover_bg));
                assert!(
                    button >= 3.0,
                    "{name}: hovered '+' contrast {button:.2} < 3.0:1"
                );
                // …and the hover is a REAL surface change, or the feedback the
                // wash exists to give is invisible.
                assert_ne!(
                    c.hover_bg, c.chip_bg,
                    "{name}: the hover wash is byte-identical to the quiet chip"
                );
                assert_ne!(
                    c.hover_bg, c.active_bg,
                    "{name}: the hover wash impersonates the SELECTED card"
                );
            }
            // The active card must be a DISTINCT surface from the body, or the
            // focused tab vanishes into the strip (true on dark and light alike).
            assert_ne!(
                c.active_bg, c.band_bg,
                "{name}: active-tab card is indistinguishable from the body"
            );
            // Every ink above already went through `ensure_contrast(_, surface, 3.0)`,
            // so `>= 3.0` alone cannot catch a bad re-tune. The two properties below
            // are ones the floor cannot fake.
            let raw = [
                ((theme.fg >> 16) & 0xff) as u8,
                ((theme.fg >> 8) & 0xff) as u8,
                (theme.fg & 0xff) as u8,
            ];
            // (i) The band's own full-strength ink is never floored on a bundled
            // scheme; needing it would mean the band itself is too heavy.
            assert_eq!(
                c.fg, raw,
                "{name}: the band's full-strength ink needed the contrast floor — the \
                 BAND is too heavy; re-tune `band_colors`, don't lean on the floor"
            );
            // (ii) The inactive dim never out-contrasts full strength on the same
            // surface. The floor is one-way and could drag a dim past its own ink;
            // only `strip_colors`' ceiling prevents it, and only this assertion
            // notices if the ceiling goes. Closest bundled case: Solarized Light on
            // a band, dim 3.50:1 against 3.69:1 full strength.
            let full = rgb(c.fg).contrast(rgb(c.band_bg));
            assert!(
                inactive <= full + 1e-9,
                "{name}: inactive labels ({inactive:.2}:1 on the band) are MORE \
                 prominent than full-strength ink ({full:.2}:1) — the dim was floored \
                 past what it dims"
            );
            // (iii) On the flat strip the floors are no-ops on every bundled scheme,
            // which keeps the macOS arm's byte identity checkable.
            if !STRIP_IS_CHROME_BAND {
                assert_eq!(
                    c.active_fg, raw,
                    "{name}: a flat strip's active label needed the contrast floor"
                );
            }
        }
        // The default theme's inactive labels stay well clear of the floor. The pin
        // is surface-dependent: ~9:1 on the flat strip, ~6.6:1 on a band (lifted
        // toward fg by construction). It catches a slide back toward 3.0.
        let pin: f64 = if STRIP_IS_CHROME_BAND { 6.0 } else { 7.0 };
        let def = strip_colors(Theme::default());
        let def_inactive = rgb(def.inactive_fg).contrast(rgb(def.band_bg));
        assert!(
            def_inactive >= pin,
            "default-theme inactive label contrast {def_inactive:.2} < {pin:.1}:1 (regressed the readability fix)"
        );
    }

    /// The forced-chrome twin of [`strip_contrast_meets_wcag_aa`]: with Linux
    /// `window_theme = light|dark` against the terminal theme's darkness, the strip
    /// uses [`crate::native_appearance::forced_chrome_theme`] (authored surfaces with
    /// every bundled scheme's accent), and the same ink/surface floors hold for
    /// every donor scheme. The raw-ink identities stay in the sibling: fg/bg here
    /// are authored constants.
    #[test]
    fn strip_contrast_holds_under_forced_chrome_themes() {
        use aterm_types::Rgb;
        let rgb = |c: [u8; 3]| Rgb::new(c[0], c[1], c[2]);
        assert_eq!(
            crate::chrome_band::forced_chrome(),
            None,
            "the theme-derived strip is only defined with no OS-forced palette"
        );
        for name in aterm_types::scheme::builtin_names() {
            let s = aterm_types::scheme::builtin(name).expect("builtin exists");
            let tp = s.to_theme_parts();
            let terminal = Theme {
                fg: tp.fg,
                bg: tp.bg,
                cursor: tp.cursor,
                selection: tp.selection,
            };
            for dark in [false, true] {
                let forced = crate::native_appearance::forced_chrome_theme(terminal, dark);
                assert_eq!(
                    theme_is_dark(forced.bg),
                    dark,
                    "{name}: the forced side classifies as the side it forces"
                );
                let c = strip_colors(forced);
                let active = rgb(c.active_fg).contrast(rgb(c.active_bg));
                let inactive = rgb(c.inactive_fg).contrast(rgb(c.band_bg));
                assert!(
                    active >= 3.0,
                    "{name} forced dark={dark}: active-tab text contrast {active:.2} < 3.0:1"
                );
                assert!(
                    inactive >= 3.0,
                    "{name} forced dark={dark}: inactive-tab label contrast {inactive:.2} < 3.0:1"
                );
                if STRIP_IS_CHROME_BAND {
                    let hover = rgb(c.hover_fg).contrast(rgb(c.hover_bg));
                    assert!(
                        hover >= 3.0,
                        "{name} forced dark={dark}: hovered label contrast {hover:.2} < 3.0:1"
                    );
                }
                if STRIP_CHIP_CARDS {
                    let chip = rgb(c.chip_fg).contrast(rgb(c.chip_bg));
                    let plus = rgb(c.chip_button_fg).contrast(rgb(c.chip_bg));
                    assert!(
                        chip >= 3.0,
                        "{name} forced dark={dark}: quiet-chip label contrast {chip:.2} < 3.0:1"
                    );
                    assert!(
                        plus >= 3.0,
                        "{name} forced dark={dark}: '+' contrast {plus:.2} < 3.0:1"
                    );
                }
                assert_ne!(
                    c.active_bg, c.band_bg,
                    "{name} forced dark={dark}: active card indistinguishable from the band"
                );
                // The dim-ceiling property carries over: unfocused labels never
                // print MORE prominently than full-strength ink.
                let full = rgb(c.fg).contrast(rgb(c.band_bg));
                assert!(
                    inactive <= full + 1e-9,
                    "{name} forced dark={dark}: the dim was floored past what it dims"
                );
            }
        }
    }

    /// The High-Contrast twin of [`strip_contrast_meets_wcag_aa`]: under every stock
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
                    // The CTA's underline is a RULE on its own chip: HIGHLIGHT drawn
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

    /// `strip_colors` is appearance-aware: on a dark theme the active card steps
    /// toward the light fg (brighter than the body), on a light theme toward the
    /// dark fg (darker). Either way it is a distinct surface.
    #[test]
    fn strip_colors_raise_direction_follows_appearance() {
        let luma = |c: [u8; 3]| {
            0.299 * f32::from(c[0]) + 0.587 * f32::from(c[1]) + 0.114 * f32::from(c[2])
        };
        let parts = |name: &str| {
            let s = aterm_types::scheme::builtin(name).expect("builtin exists");
            let tp = s.to_theme_parts();
            strip_colors(Theme {
                fg: tp.fg,
                bg: tp.bg,
                cursor: tp.cursor,
                selection: tp.selection,
            })
        };
        // Dark builtin: the active card is brighter than the body (a raised step).
        let dark = parts("Dracula");
        assert!(
            luma(dark.active_bg) > luma(dark.band_bg),
            "dark theme active card should be brighter than the body"
        );
        // Light builtin: the active card is darker than the body (a subtle card).
        let light = parts("Solarized Light");
        assert!(
            luma(light.active_bg) < luma(light.band_bg),
            "light theme active card should be darker than the body"
        );
        // The default (dark) theme: active = blend(bg, fg, 0.21), inactive labels =
        // blend(fg, bg, 0.15).
        let def = strip_colors(Theme::default());
        assert!(luma(def.active_bg) > luma(def.band_bg));
    }

    // ================= The strip under a stationary pointer =================

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
                    // CLOSURE: every tab this page seats computes this page.
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

    // ================= The floor is a title width =================

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
            floor,
            if STRIP_CHIP_CARDS { 7 } else { 8 },
            "'Settings' is 8 cells, and a chip-card band spends one of them on \
             the card's interior pad"
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
        // The one that matters: a busy chip needs the status canvas AND its
        // separator on top of the plain chip's pads.
        assert_eq!(
            chip_seat_cols(busy_tab()),
            PREFERRED_MIN_TAB_COLS + 3,
            "the status canvas, its separator, and the cell they push the title \
             off — the three cells the old gate handed a busy chip for free"
        );
    }

    /// Eight busy `claude` shells at 99–114 columns window at the width their titles
    /// stop fitting ([`strip_seat_cols`]) instead of painting bare ordinals on
    /// 12-column chips with four title cells. Pinned at both selections.
    #[test]
    fn eight_busy_shells_never_paint_a_bare_ordinal() {
        let titles: Vec<String> = (0..8).map(|_| "claude".to_string()).collect();
        let metadata = vec![busy_tab(); 8];
        for cols in 99..=114u16 {
            for active in [0usize, 7] {
                let segments =
                    layout_segments_with_metadata(cols, titles.len(), &metadata, active, false);
                let labels =
                    distinct_chip_labels(&segments, &titles, Some(&metadata), active, None);
                let painted: Vec<String> = segments
                    .iter()
                    .filter_map(|seg| match seg.kind {
                        TabHit::Select(i) => labels[i].clone(),
                        _ => None,
                    })
                    .collect();
                assert!(
                    !painted.is_empty(),
                    "{cols} cols: the strip seated nothing at all"
                );
                for label in &painted {
                    assert_eq!(
                        label, "claude",
                        "{cols} cols, active {active}: a seated chip says {label:?} \
                         instead of naming its shell: {painted:?}"
                    );
                }
            }
        }

        // Only the busy strip's gate moved: at 100 columns eight idle tabs still
        // seat eight equal chips, as the metadata-free entry point computes.
        let idle = vec![PLAIN_TAB; 8];
        let quiet = layout_segments_with_metadata(100, 8, &idle, 0, false);
        assert_eq!(
            quiet,
            layout_segments(100, 8, 0, false),
            "an idle strip's geometry is the pure-geometry one, to the column"
        );
        assert_eq!(
            quiet
                .iter()
                .filter(|seg| matches!(seg.kind, TabHit::Select(_)))
                .count(),
            8,
            "eight idle chips still fit at 100 columns"
        );
        assert!(
            layout_segments_with_metadata(100, 8, &metadata, 0, false)
                .iter()
                .filter(|seg| matches!(seg.kind, TabHit::Select(_)))
                .count()
                < 8,
            "...and the busy strip windows instead of painting eight chips it \
             cannot title"
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

    /// The version label's widths the layout tests below reason in — SYMBOLIC
    /// stand-ins for a measured slot (full ≈ 22 columns, short ≈ 13 at 96 dpi),
    /// because the rule under test is how the strip yields to the label, not one
    /// face's advances. The measured widths have their own test
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

    /// THE LABEL SITS FLUSH RIGHT AND HITS AS ITSELF. A lone tab at 132 columns
    /// keeps its whole chip and its `+` exactly where they were, the label takes
    /// the last 22 columns, and the band between them stays bare band — caption
    /// on Windows (drag, double-click, the system menu).
    #[cfg(not(target_os = "macos"))]
    #[test]
    fn version_slot_is_trailing_flush_right_and_hits_version() {
        let segs = layout_segments_with_version(132, 1, &[PLAIN_TAB], 0, false, SLOT);
        let full = TabHit::Version {
            rung: crate::menu::VersionRung::Full,
            attention: false,
        };
        assert_eq!(
            segs.iter()
                .map(|s| (s.kind, s.start_col, s.end_col))
                .collect::<Vec<_>>(),
            vec![
                (TabHit::Select(0), 0, 28),
                (TabHit::NewTab, 28, 31),
                (full, 110, 132),
            ]
        );
        let label = segs.last().copied().expect("the label segment");
        assert_eq!(
            (label.close_col, label.connector_col, label.solo),
            (None, None, false),
            "the label is one button: no close, no connector, not a title band"
        );
        for col in 110..132 {
            assert_eq!(hit_test(&segs, col), Some(full), "column {col}");
        }
        assert_eq!(
            hit_test(&segs, 60),
            None,
            "the band left of the label stays bare"
        );
        assert_eq!(hit_test(&segs, 132), None, "nothing past the strip");
    }

    /// EVERYTHING LEFT OF THE LABEL IS TODAY'S STRIP, TO THE COLUMN: with a
    /// label laid out, the other segments are exactly today's layout of the
    /// `cols - w` columns left of it; without one, exactly today's layout of all
    /// `cols`. And no label ([`VersionSlotCols::NONE`]) is today's strip — for
    /// plain chips, the metadata-free entry's too.
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

    /// THE LABEL YIELDS BEFORE ANY CHIP DOES. Wherever the label is laid out
    /// beside two or more tabs, every chip still has at least its seat
    /// ([`strip_seat_cols`]), every tab is seated, and no edge is marked: the
    /// label never pushes the strip into its pressure or windowed layout.
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

    /// A LONE CHIP NEVER SHRINKS FOR THE LABEL: at every width it is exactly
    /// the chip it would be with no label, and a label beside it means the chip
    /// has its full [`LONE_CHIP_COLS`].
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
        // The thresholds the design names: full from 53 columns, short from 44.
        let rung_at = |cols: u16| {
            version_of(&layout_segments_with_version(
                cols,
                1,
                &[PLAIN_TAB],
                0,
                false,
                SLOT,
            ))
            .map(|v| v.0)
        };
        assert_eq!(rung_at(53), Some(crate::menu::VersionRung::Full));
        assert_eq!(rung_at(52), Some(crate::menu::VersionRung::Short));
        assert_eq!(rung_at(44), Some(crate::menu::VersionRung::Short));
        assert_eq!(rung_at(43), None);
    }

    /// THE `+` AND THE `↻` NEVER LAND IN THE LABEL: no other segment overlaps its
    /// columns. And no `‹`/`›` mark can: those mark a strip that could not seat
    /// every tab, and the label is laid out only beside a strip that does
    /// ([`version_rung_that_fits`]), so wherever there is a label there is no
    /// mark at all — asserted outright, rather than as "no mark in the label's
    /// columns", which could never fail.
    #[test]
    fn the_plus_and_the_marks_never_share_a_column_with_the_slot() {
        let mut laid = 0usize;
        version_sweep(|tabs, name, metadata, show_update, active, cols| {
            let segs =
                layout_segments_with_version(cols, tabs, metadata, active, show_update, SLOT);
            let Some((_, start, end)) = version_of(&segs) else {
                return;
            };
            laid += 1;
            for seg in without_version(&segs) {
                assert!(
                    seg.end_col <= start,
                    "{name} at {cols}: {:?} {}..{} reaches the label at {start}..{end}",
                    seg.kind,
                    seg.start_col,
                    seg.end_col
                );
            }
            assert_eq!(
                strip_overflow(&segs, tabs),
                StripOverflow::NONE,
                "{name} at {cols} cols, {tabs} tabs: a label beside a marked strip"
            );
        });
        assert!(laid > 2_000, "vacuous: only {laid} strips laid a label out");
    }

    /// THE LABEL'S MATERIAL IN EACH STATE ([`version_label_material`]) — the
    /// pairs the band raster paints and the High-Contrast rows measure: no card
    /// and the inactive ink at rest, the band's hover card and ink under the
    /// pointer, the update CTA's card and ink under attention, which outranks
    /// the pointer.
    #[test]
    fn the_version_label_wears_the_strips_own_materials() {
        for theme in [
            Theme::default(),
            Theme {
                fg: 0x0024_292F,
                bg: 0x00FF_FFFF,
                cursor: 0x0024_292F,
                selection: 0x00DD_F4FF,
            },
        ] {
            let c = strip_colors(theme);
            assert_eq!(
                version_label_material(&c, false, false),
                (None, c.inactive_fg),
                "rest: bare band, the inactive ink"
            );
            assert_eq!(
                version_label_material(&c, false, true),
                (Some(c.hover_bg), c.hover_fg),
                "hovered: the band's hover card"
            );
            for hovered in [false, true] {
                assert_eq!(
                    version_label_material(&c, true, hovered),
                    (Some(c.update_bg), c.update_fg),
                    "attention (hovered {hovered}): the update CTA's card"
                );
            }
        }
    }

    /// THE LABEL STEPS DOWN, THEN HIDES: full while it fits, the short rung when
    /// only that fits, and nothing once even that would cost a tab its seat —
    /// at 132 columns 8/9/10 tabs, at 77 columns 4/5/6.
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
        // The design's own columns for the short rung: 9 tabs of 12 at 132,
        // 5 tabs of 12 at 77 with the `+` at 60..63.
        let segs = layout_segments_with_version(132, 9, &[PLAIN_TAB; 9], 0, false, SLOT);
        assert_eq!(version_of(&segs), Some((Short, 119, 132)));
        let segs = layout_segments_with_version(77, 5, &[PLAIN_TAB; 5], 0, false, SLOT);
        assert_eq!(version_of(&segs), Some((Short, 64, 77)));
        let plus = segs
            .iter()
            .find(|s| s.kind == TabHit::NewTab)
            .expect("the +");
        assert_eq!((plus.start_col, plus.end_col), (60, 63));
        assert!(
            segs.iter()
                .filter(|s| matches!(s.kind, TabHit::Select(_)))
                .all(|s| s.end_col - s.start_col == 12)
        );
    }

    /// The label and the staged-update `↻` share one strip: the `↻` keeps the
    /// leading cells, the label the trailing ones, and each hits as itself.
    #[test]
    fn the_slot_coexists_with_the_update_alert() {
        let segs = layout_segments_with_version(132, 2, &[PLAIN_TAB; 2], 0, true, SLOT);
        assert_eq!(
            segs.iter()
                .map(|s| (s.kind, s.start_col, s.end_col))
                .collect::<Vec<_>>(),
            vec![
                (TabHit::Update, 0, 3),
                (TabHit::Select(0), 3, 55),
                (TabHit::Select(1), 55, 107),
                (TabHit::NewTab, 107, 110),
                (
                    TabHit::Version {
                        rung: crate::menu::VersionRung::Full,
                        attention: false,
                    },
                    110,
                    132
                ),
            ]
        );
        assert_eq!(hit_test(&segs, 1), Some(TabHit::Update));
        assert!(matches!(hit_test(&segs, 120), Some(TabHit::Version { .. })));
    }

    /// THE CELL LANE'S LABEL — the transitional frame before the UI face lands,
    /// and the whole label where the pixel band does not reach. Right-aligned
    /// with one trailing pad cell; bare band at rest, the band's hover material
    /// under the pointer, the update CTA's surface under attention; the short
    /// rung when the cells cannot hold the full one, and no text below that.
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
            connector_col: None,
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
}
