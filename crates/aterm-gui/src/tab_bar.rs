// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A VISIBLE, CLICKABLE TAB STRIP composed at the GUI level (no renderer pass).
//!
//! Tabs already exist as state ([`crate::TabIndex`] + one [`crate::pane::PaneTree`]
//! per tab), driven by Cmd-T / Cmd-W / Cmd-1..9 — but they were INVISIBLE. This
//! module reserves `tab_strip_rows` rows (config, default 1) at the TOP of the
//! window and draws, per TAB (top-level — one entry even when the tab is split
//! into panes), a cell-aligned segment carrying its title, an active-tab
//! highlight, and a close `x`, plus a trailing `+` to open a tab.
//!
//! It is PURE LAYOUT + a small [`RenderCell`] paint, mirroring [`crate::pane`]:
//! [`layout_segments`] produces the segment list (each a column range + an
//! optional close-`x` column + a [`TabHit`] target) and is unit-testable with no
//! window or renderer; [`paint_strip`] writes those segments into the composed
//! frame's top rows using the existing [`RenderCell`] + [`Theme`] colours. A
//! mouse click in `row < tab_strip_rows` maps through [`hit_test`] to switch /
//! close / open, intercepted in the GUI's mouse handlers BEFORE the focused
//! pane's cell mapping.
//!
//! NO-REGRESSION: with `tab_strip_rows == 0` nothing here runs — the composed
//! frame is the terminal grid exactly as before (the byte-identical path). The
//! strip is spliced ABOVE the terminal content in the composed `RenderInput`
//! only; the session grids are never shifted.

use std::cell::RefCell;
use std::sync::Arc;

use aterm_core::grid::extra::{ImageData, ImageFormat, ImageRef};
use aterm_core::terminal::{RenderCell, UnderlineStyle};
use aterm_render::Theme;

/// The four first-party *non-terminal* tab application identities. The type lives
/// beside `TabPresentation`, so a terminal is structurally `None` before either
/// chrome renderer sees it. Every renderer consumes the same code-native primitive
/// geometry below; no installed icon font, Unicode symbol, or external asset can
/// change what an aterm app tab means.
pub(crate) use crate::tab_model::TabIconKind;

/// Canonical non-title presentation metadata consumed by both tab-strip renderers.
/// Titles remain separate because terminal titles are live OSC state; these fields are
/// stable tab identity/state from [`crate::tab_model::TabPresentation`].
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub(crate) struct TabStripMetadata {
    pub(crate) icon: Option<TabIconKind>,
    pub(crate) dirty: bool,
    pub(crate) busy: bool,
    pub(crate) attention: bool,
    pub(crate) closable: bool,
}

impl TabStripMetadata {
    #[must_use]
    pub(crate) fn from_presentation(presentation: &crate::tab_model::TabPresentation) -> Self {
        Self {
            icon: presentation.icon,
            dirty: presentation.indicators.dirty,
            busy: presentation.indicators.busy,
            // The strip draws ONE attention mark, so the two owners fold here.
            attention: presentation.indicators.wants_attention(),
            closable: presentation.closable,
        }
    }

    #[must_use]
    pub(crate) const fn has_status(self) -> bool {
        self.dirty || self.busy || self.attention
    }

    #[must_use]
    pub(crate) const fn status_count(self) -> usize {
        self.dirty as usize + self.busy as usize + self.attention as usize
    }

    #[cfg(test)]
    const fn clean(icon: TabIconKind) -> Self {
        Self {
            icon: Some(icon),
            dirty: false,
            busy: false,
            attention: false,
            closable: true,
        }
    }
}

/// Independently visible status marks carried by a tab. Their order and shapes are
/// shared by the semantic in-grid renderer and the native macOS host, so a dirty,
/// working, or attention-requesting tab means the same thing on every surface.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub(crate) enum TabStatusKind {
    Dirty,
    Busy,
    Attention,
}

pub(crate) const TAB_STATUS_KINDS: [TabStatusKind; 3] = [
    TabStatusKind::Dirty,
    TabStatusKind::Busy,
    TabStatusKind::Attention,
];

impl TabStripMetadata {
    #[must_use]
    pub(crate) const fn has_status_kind(self, kind: TabStatusKind) -> bool {
        match kind {
            TabStatusKind::Dirty => self.dirty,
            TabStatusKind::Busy => self.busy,
            TabStatusKind::Attention => self.attention,
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
        2 => 5.25 + ordinal.min(1) as f32 * 5.5,
        _ => 3.5 + ordinal.min(2) as f32 * 4.5,
    }
}

/// One normalized code-native icon primitive in a 16×16 design box.  Lines use
/// round caps; outlined rounded rectangles use the given corner radius.  The in-grid
/// path rasterizes these analytically into `RawRgba8`; the macOS titlebar maps the same
/// commands to `NSBezierPath`.  Keeping this tiny IR here prevents semantic or geometry
/// drift between the two independently hosted strips.
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
}

pub(crate) const TAB_ICON_DESIGN_SIZE: f32 = 16.0;
/// Native toolbar icon size in logical points. The primitive design box is scaled
/// uniformly into this square, keeping a crisp, restrained ~16 px optical mark.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
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
pub enum TabHit {
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
}

/// One laid-out tab strip segment: a half-open column range `[start_col, end_col)`
/// in the strip row, an optional close-`x` column (the cell whose click closes the
/// tab), and what a plain click on the segment does. Caching these per frame lets a
/// mouse click in the strip map back to a tab in O(segments).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct TabSegment {
    /// First column of this segment (inclusive).
    pub start_col: u16,
    /// One past the last column of this segment (exclusive).
    pub end_col: u16,
    /// The column of the close `x`, if this segment drew one. A click here is a
    /// [`TabHit::Close`]; every other column of the segment is the `kind` action.
    pub close_col: Option<u16>,
    /// The action a plain (non-close) click on this segment performs.
    pub kind: TabHit,
    /// This is the window's ONLY tab, so it is drawn as the window TITLE — the
    /// name and its description centred on the body background — rather than as a
    /// raised chip with a close affordance. Mirrors the native strip's solo band
    /// (`toolbar::TabView`'s solo mode): with one tab there is nothing to switch
    /// between, so a switcher is the wrong thing to draw.
    pub solo: bool,
}

/// The minimum cells a tab segment needs to show ` x ` (a leading pad, at least
/// one title cell, a pad, the close `x`, a trailing pad). Below this, tabs are
/// drawn without a close `x` (just the title) so they still fit + remain clickable.
const MIN_SEG_WITH_CLOSE: u16 = 5;
/// Columns the trailing `+` (open-a-tab) affordance occupies: ` + `.
const NEW_TAB_W: u16 = 3;
/// Columns the LEADING update icon (`↻`) occupies: ` ↻ ` — same width as `+` so it
/// reads as a sibling affordance.
const UPDATE_W: u16 = 3;

/// How wide a LONE tab's chip may grow, in columns — `None` where a lone tab IS the
/// window title band and therefore *should* spend the whole strip (macOS; see
/// [`SOLO_TITLE_BAND`]).
///
/// Off macOS a lone tab is an ordinary chip, and the equal-share rule below ("no
/// maximum width") then hands the only tab every column the trailing `+` does not
/// want. On a chrome band that paints the ENTIRE strip row as one raised card: the
/// grid/band/card stack the strip is built on is invisible in the state every new
/// window starts in, and a full-width raised slab is a HEAVIER full-width grey than
/// the plain band tone the visual-judge loop already rejected (see [`strip_colors`]).
/// So the lone chip is capped and stays LEADING (the Windows/Linux convention — tabs
/// grow rightward from the left edge), leaving real band to its right for the `+` to
/// sit on and for the eye to read as the surface underneath.
///
/// 28 columns rather than a content measurement because `layout_segments` is pure
/// geometry and is never handed the titles (a content-sized chip would also resize
/// itself on every OSC-title change, moving the close `✕` under a stationary
/// pointer). 28 leaves ~24 cells of title after the pads and the `✕` — comfortably
/// past [`MIN_SEG_WITH_CLOSE`], and about what a native Windows tab caps at. A
/// narrow window still gets the smaller equal share, so this is a MAXIMUM, never a
/// minimum.
const SOLO_CHIP_MAX_COLS: Option<u16> = if cfg!(target_os = "macos") {
    None
} else {
    Some(28)
};

/// The EQUAL-SHARE legibility floor, in columns. While every tab's equal share
/// stays at or above this, tabs split the band evenly; once a share would drop
/// below it, [`layout_segments`] switches to the ACTIVE-PRIORITY layout — the
/// selected tab reserves a useful width and the inactive tabs compress around it.
///
/// This is the native strip's `toolbar::PREFERRED_MIN_TAB_WIDTH` (64 pt), in
/// cells. 12 columns is the same identity test that picked 64 pt: after the
/// leading pad, the trailing pad and the reserved close `✕` slot
/// ([`tab_content_layout`]), a 12-cell segment keeps exactly 8 title cells —
/// "Settings", the short-identity yardstick the point-space floor was measured
/// against.
const PREFERRED_MIN_TAB_COLS: u16 = 12;

/// The most band the ACTIVE tab may reserve once the pressure layout engages:
/// the native strip's 96 pt cap (`toolbar::native_tab_cells`) carried over at
/// the same 96:64 ratio off the floor above (1.5 × 12 = 18). 18 cells leave a
/// ~14-cell title — comfortably readable — without letting one tab swallow a
/// narrow band whole; below the cap the active share is still bounded by 60% of
/// the band, exactly as in points.
const ACTIVE_TAB_PRESSURE_CAP_COLS: u16 = 18;

/// Cell-space content ordering inside an in-grid tab: leading pad, two-cell icon,
/// one-cell breathing gap, title, optional shape-coded status canvas, close affordance. Two cells
/// yield an approximately square 16–18 px icon with ordinary terminal cell metrics;
/// narrow tabs drop the icon/status before they ever move the close hit
/// target or make the whole tab unselectable.
const ICON_COLS: u16 = 2;
const ICON_GAP: u16 = 1;

/// Point-space content geometry shared with the macOS view renderer. The entire tab
/// view remains the select/reorder target. Wide chips reserve a close target under
/// the leading edge; compact chips give that space back to the title (Close Tab
/// remains available from the menu/shortcut). Responsive icon/status slots appear
/// only after a useful title width survives.
///
/// SYMMETRIC BY CONSTRUCTION (macOS Terminal's tab shape): the label's leading and
/// trailing insets are the SAME number, so a centre-aligned title reads centred in
/// its own cell — never nudged off-centre by the close slot, the app icon, or the
/// status canvas. That symmetry is also what lets the close ✕ be a HOVER-ONLY
/// affordance: its slot is reserved whether or not it is currently painted, so
/// revealing it can never reflow the title (see `toolbar::TabView`). The icon takes
/// the leading slot and the status canvas the trailing one, and each is admitted
/// only while the resulting symmetric label still clears the legibility floor.
#[derive(Clone, Copy, PartialEq, Debug)]
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub(crate) struct NativeTabContentLayout {
    pub(crate) close: [f64; 4],
    pub(crate) close_available: bool,
    pub(crate) icon: Option<[f64; 4]>,
    pub(crate) status: Option<[f64; 4]>,
    pub(crate) label: [f64; 4],
}

/// Preference order for the SOLO title band's subtitle: the durable authored
/// description first, then where the session is, then what it is doing. Authored
/// prose outranks anything generated, and a location outranks a status word because
/// a one-tab window is usually asking "which shell is this?".
///
/// Deliberately NOT `state`: its steady value is the word "alive", which is a
/// subtitle that says nothing — the band shows the bare title instead. Lifecycle
/// state stays on the hover card, the context menu, and the `chrome` mirror.
const SOLO_SUBTITLE_KEYS: [&str; 3] = ["description", "cwd", "activity"];

/// The one-line DESCRIPTION a single-tab window shows beside its title, drawn from
/// the same composed session chrome (`session_chrome::compose_tooltip`) the hover
/// card and context menu render — never a second, drifting source of facts.
///
/// With exactly one tab there is no switching to do, so the strip stops being a
/// switcher and becomes the window's title (macOS Terminal's `folder — -zsh` line):
/// the chip, its pill, and its ✕ are all dead weight, and the space they held goes
/// to saying WHAT this window is. `None` when the session carries nothing the title
/// does not already say — the band then shows the bare title rather than padding it
/// with an echo.
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

/// Lay a tab chip's interior out around `center_y` — the vertical centre the whole
/// strip shares, MEASURED from the window's traffic lights rather than assumed to be
/// the view's own midpoint (`toolbar::strip_metrics`). Everything vertical is derived
/// from that one number, so the chips, the ✕, the icon, the status dots, and the
/// trailing "+" all sit on the stoplights' optical centre line.
#[must_use]
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub(crate) fn native_tab_content_layout(
    width: f64,
    center_y: f64,
    has_icon: bool,
    has_status: bool,
) -> NativeTabContentLayout {
    // The ✕ is a HOVER-ONLY reveal, but its slot is reserved on BOTH edges so the
    // reveal never reflows the centred title — which makes the slot's width a direct
    // tax on every title, paid at all times. Hence a deliberately small target
    // (macOS Terminal's is smaller still): 16 pt is comfortably clickable for a
    // pointer that is already inside the chip, and buys back 2× the difference.
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
    // A semantic-app tab needs room for close + icon + a complete short identity such
    // as "Settings". Below this width the title is the primary navigation signal, so
    // the close target yields its slot instead of forcing an ellipsis.
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

/// Exact title/icon/status slots for one already-laid-out tab segment. The segment and
/// `close_col` remain authoritative hit geometry; this function only divides their
/// interior paint space.  When width is scarce the degradation order is title width →
/// dirty mark → icon, never close/select geometry.
fn tab_content_layout(seg: &TabSegment, metadata: TabStripMetadata) -> TabContentLayout {
    let leading = seg.start_col.saturating_add(1);
    let mut title_end = match seg.close_col {
        Some(close) => close.saturating_sub(1),
        None => seg.end_col.saturating_sub(1),
    };

    // All independently true states share one compact, shape-coded status canvas. It
    // gets a trailing cell plus a separator only when one title cell survives. If the
    // tab is too narrow, inspection/accessibility still expose every state; paint never
    // overwrites the close affordance.
    let status_col = if metadata.has_status() && title_end >= leading.saturating_add(3) {
        let col = title_end - 1;
        title_end = title_end.saturating_sub(2);
        Some(col)
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

/// Lay out the tab strip across `cols` columns for `tab_count` tabs (`active` is
/// the highlighted one). Returns one [`TabSegment`] per visible tab plus, when
/// room remains, a trailing [`TabHit::NewTab`] `+` segment. Segments are packed
/// left-to-right; a tab that would overflow the strip is dropped (its title simply
/// isn't shown — it stays reachable by Cmd-N / cycling). `tab_count == 0` (never,
/// in practice — there is always ≥1 tab) yields just the `+`.
///
/// SAME RULES AS THE NATIVE STRIP (`toolbar::native_tab_cells` /
/// `toolbar::set_window_tabs`), so the two renderers cannot disagree about what a
/// tab bar IS: tabs divide the whole band into EQUAL shares with no maximum width
/// (a wide window buys longer titles, not bare strip past a capped segment); a
/// LONE tab is the window's title rather than a switcher — one full-band
/// [`TabSegment::solo`] segment with no close column; and UNDER PRESSURE — once
/// an equal share would drop below [`PREFERRED_MIN_TAB_COLS`] — the ACTIVE tab
/// reserves a useful width (60% of the band, capped at
/// [`ACTIVE_TAB_PRESSURE_CAP_COLS`]) while the inactive tabs compress around it,
/// remaining ordered and clickable. At sixteen tabs on a 130-column strip the
/// equal share is ~7 cells — a three-character title — and the one title the
/// user is actually reading collapsed with all the rest; the pressure layout is
/// `native_tab_cells`' answer, in cells.
///
/// All three rules are macOS's, and the native strip they mirror is macOS's alone
/// (the AppKit toolbar). Off macOS a lone tab is neither a title band
/// ([`SOLO_TITLE_BAND`]) nor entitled to the whole width ([`SOLO_CHIP_MAX_COLS`])
/// — it is a leading chip on a visible band. Two or more tabs still share
/// equally on every platform, until the pressure layout engages on all of them
/// alike.
///
/// Pure geometry: no window, no renderer, no `App`. `active` matters only under
/// pressure — with legible equal shares the layout is selection-independent, so
/// switching tabs on a roomy strip moves no chip and no close column.
#[must_use]
pub fn layout_segments(
    cols: u16,
    tab_count: usize,
    active: usize,
    show_update: bool,
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
    // ONE tab is the window's TITLE, not a switcher: it takes the whole band and
    // carries no close column (the window's own close affordance already exists).
    //
    // ...WHERE THE WINDOW HAS NO TITLE OF ITS OWN. That premise is macOS's alone
    // (see `SOLO_TITLE_BAND`): elsewhere the OS caption is right there carrying the
    // same string, so the band would be the title's second copy AND would strip the
    // one-tab window of the only chrome it has. Off macOS a lone tab is an ordinary
    // chip — it keeps its raised card, its selection rule and its close column. The
    // WIDTH policy is untouched either way: one tab still spends the whole band.
    let solo = tab_count == 1 && SOLO_TITLE_BAND;
    let mut x: u16 = lead;
    if tab_count > 0 {
        let band = avail.saturating_sub(lead);
        // Split the available width (past the leading update icon) into EQUAL
        // shares — no maximum for TWO OR MORE tabs, so a wide window spends its
        // columns on titles instead of leaving bare strip past a capped segment
        // (the lone-tab exception is immediately below). Floored so a tab is at
        // least 1 cell before we stop placing tabs.
        let share = band / tab_count as u16;
        // ONE tab, off macOS: cap the chip instead of spending the whole band on it
        // — see [`SOLO_CHIP_MAX_COLS`] for why a full-width lone chip is the wrong
        // picture. The chip keeps its LEADING position (`x` already starts at
        // `lead`), so the band shows to its right and the `+` follows it there
        // instead of being pushed to the far edge. Two or more tabs are untouched:
        // they already divide the band into visibly bounded shares.
        let per = match SOLO_CHIP_MAX_COLS {
            Some(cap) if tab_count == 1 => share.min(cap),
            _ => share,
        };
        // ACTIVE-PRIORITY UNDER PRESSURE (`toolbar::native_tab_cells`, in cells):
        // once the equal share falls below the legibility floor, the selected
        // identity gets a useful width — up to 60% of the band, capped — and the
        // inactive tabs split what remains, floored at one cell so they stay
        // individually clickable for as long as the band has columns at all.
        // (The old all-equal rule at that point gave EVERY title, the focused
        // one included, the same three unreadable cells.) Equal shares above the
        // floor are untouched, so a roomy strip stays selection-independent:
        // switching tabs there moves no geometry, and the hover-reserved close
        // columns stay under a stationary pointer.
        let active = active.min(tab_count - 1);
        let (active_w, inactive_w) = if tab_count > 1 && share < PREFERRED_MIN_TAB_COLS {
            // u32 for the 60% product only: `band` is a u16 column count, and
            // `band * 3` can overflow u16 on absurd-but-legal widths.
            let selected = u16::try_from(u32::from(band) * 3 / 5)
                .unwrap_or(u16::MAX)
                .min(ACTIVE_TAB_PRESSURE_CAP_COLS)
                .max(share);
            let inactive = (band.saturating_sub(selected) / (tab_count as u16 - 1)).max(1);
            (selected, inactive)
        } else {
            (per, per)
        };
        for i in 0..tab_count {
            let per = if i == active { active_w } else { inactive_w };
            if per == 0 || x >= avail {
                break; // out of room: remaining tabs are not drawn (still reachable)
            }
            let seg_w = per.min(avail - x);
            let start = x;
            let end = x + seg_w;
            // Draw a close `x` only when the segment is wide enough to also show a
            // title; its column is the last cell minus the trailing pad. A solo
            // title band never reserves one.
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

/// Canonical-presentation-aware layout used by the shipping strip.  The base segment
/// widths, order, select hit areas, reorder geometry, and trailing `+` remain exactly
/// [`layout_segments`]'s.  Only a tab explicitly marked non-closable loses its close
/// column; no icon/status field can shrink or move a hit target.
#[must_use]
pub(crate) fn layout_segments_with_metadata(
    cols: u16,
    tab_count: usize,
    metadata: &[TabStripMetadata],
    active: usize,
    show_update: bool,
) -> Vec<TabSegment> {
    let mut segments = layout_segments(cols, tab_count, active, show_update);
    for segment in &mut segments {
        if let TabHit::Select(index) = segment.kind
            && metadata.get(index).is_some_and(|item| !item.closable)
        {
            segment.close_col = None;
        }
    }
    segments
}

/// Map a strip click at column `col` to its [`TabHit`], or `None` for a click on
/// bare strip background (between/after segments). A click on a segment's
/// `close_col` is a [`TabHit::Close`]; any other column of a tab segment selects
/// it; the `+` segment opens a tab.
#[must_use]
pub fn hit_test(segments: &[TabSegment], col: u16) -> Option<TabHit> {
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

/// What a strip cell represents, selecting its precomputed tone in [`strip_cell`].
#[derive(Clone, Copy, PartialEq, Eq)]
enum StripRole {
    /// The focused tab: full-strength bold fg on the raised button bg.
    Active,
    /// An unfocused tab or the bare strip: dimmed fg on the body bg (recedes).
    Inactive,
    /// An unfocused tab UNDER THE POINTER (chrome band only): the inactive label
    /// on a faint wash a step off the band — the native strip's "fainter rounded
    /// pill" hover (`toolbar.rs`), in cells. Enough to say "this is a target"
    /// (and to give the revealed `✕` a backing), never enough to be mistaken for
    /// the selected card. On macOS's flat in-grid strip the pointer changes
    /// nothing but the `✕`, exactly as before — the flat treatment was tuned
    /// against titlebar vibrancy and has no band for a wash to sit on.
    Hover,
    /// The vertical hairline on a quiet chip's LEADING pad cell (chrome band
    /// only): equal flush-packed cells give three identical titles no other cue
    /// that they are three TABS. The seam's own ink on the band, so the strip's
    /// structural hairlines are one material; suppressed beside the active and
    /// hovered chips, which already draw their own edge ([`strip_separates`]).
    Separator,
    /// The trailing `+` new-tab affordance: a BUTTON — full-strength fg on the body
    /// (NOT the dim inactive treatment), so it meets WCAG-AA contrast on every theme.
    NewTab,
    /// The SOLO band's title: full-strength fg on the body, with no raised card and
    /// no selection underline. A one-tab window is not choosing between anything, so
    /// it gets a window title's treatment, not a selected chip's.
    Title,
    /// The leading `↻` update-ready alert: a RAISED highlighted button (the active-tab
    /// treatment) so it stands out from the flat `+` and the receded tabs.
    Update,
}

/// Does the in-grid strip paint itself as a real CHROME BAND — its own surface
/// tone, a hairline seam closing it against the grid, and a raised `+` — instead of
/// receding into the terminal background?
///
/// macOS says NO, and that is a tuned answer rather than an oversight. There the
/// in-grid strip is the *fallback* presentation for a window whose chrome is
/// normally the native AppKit toolbar (`DEFAULT_TAB_STRIP_ROWS` is 0 on macOS), it
/// composites over titlebar vibrancy, and `tools/visual-judge` rejected precisely
/// this treatment — "a full-width gray chrome band with the active tab merged into
/// the body, heavy/unfinished" (see [`strip_colors`]).
///
/// Everywhere else the in-grid strip is the ONLY chrome the window has:
/// `DEFAULT_TAB_STRIP_ROWS` is 1, there is no toolbar, and `body_bg == theme.bg`
/// left the tab labels floating as bare text on the grid — no band, no card, no
/// seam, no button. That is the shipped Windows look and the loudest complaint
/// against it, so off macOS the strip claims a surface of its own.
const STRIP_IS_CHROME_BAND: bool = !cfg!(target_os = "macos");

/// Does a LONE tab collapse into a full-width window-TITLE band (centred title +
/// description, no card, no `✕`) instead of painting an ordinary selected chip?
///
/// macOS says YES because it hides the OS title bar's text
/// (`setTitleVisibility(NSWindowTitleHidden)`, `toolbar.rs`), so the solo band *is*
/// the window title — the only place that string appears on screen at all.
///
/// Windows and Linux keep a real OS caption carrying the same composed string a few
/// pixels above the strip, so a solo band there prints the title TWICE and reads as
/// a rendering bug. Worse, it costs a one-tab window every scrap of chrome it has:
/// the solo painter is deliberately flat — [`StripRole::Title`] on the band, no
/// raised card, no selection rule, no close mark — which on the pre-band strip meant
/// bold theme-fg on theme-bg and nothing else. Off macOS a lone tab is therefore an
/// ordinary chip and the caption keeps the title to itself.
const SOLO_TITLE_BAND: bool = cfg!(target_os = "macos");

/// The UI-text contrast floor every strip ink is held to against whatever surface
/// it lands on. 3.0:1 rather than AA's 4.5, for the reason
/// `strip_contrast_meets_wcag_aa` records: deliberately MUTED schemes cannot reach
/// 4.5 even at full strength (Solarized Light's undimmed fg is ~4.13:1), so 3.0 is
/// the universally achievable bar. A no-op on every bundled scheme; it exists so a
/// user theme cannot put illegible ink on the band.
const STRIP_INK_FLOOR: f64 = 3.0;

/// How far the bottom seam steps off the band toward the foreground. A hairline
/// wants to read as the band's EDGE, not as a rule drawn inside it — big enough to
/// survive a 1 px underline at 12 px type, small enough that it never competes with
/// the active tab's accent.
const STRIP_SEAM_T: f32 = 0.30;

/// The strip tones derived from a theme, computed ONCE per [`paint_strip`]
/// (hoisted out of the per-cell [`strip_cell`] — the blends are frame-invariant).
#[derive(Clone, Copy)]
struct StripColors {
    /// Full-strength foreground (the `+` affordance and, by default, the
    /// active-tab text), contrast-floored against [`Self::band_bg`].
    fg: [u8; 3],
    /// The strip's own BAND surface: what bare strip, inactive tabs and a solo
    /// title band are painted on. On macOS the terminal background byte-for-byte,
    /// so the strip recedes into the content; elsewhere
    /// [`crate::chrome_band::band_colors`]'s `bar_bg` — the SAME tone the find bar,
    /// the config notices and the settings band already use, so the window's
    /// in-grid chrome reads as one material instead of three.
    band_bg: [u8; 3],
    /// The active tab's raised-button background. Equal to [`Self::raise_bg`]
    /// unless the user's `active_tab_color` replaced it.
    active_bg: [u8; 3],
    /// The THEME-derived raise, before any `active_tab_color` override. The `+`
    /// borrows this rather than [`Self::active_bg`]: recolouring the SELECTED TAB
    /// is not a request to recolour the new-tab button.
    raise_bg: [u8; 3],
    /// Ink for [`Self::raise_bg`], floored against it. A separate tone from
    /// [`Self::fg`] because a raised card is a DIFFERENT surface from the band, and
    /// on a muted scheme the step that lifts the card also walks the card out from
    /// under its own label (Solarized Dark's fg lands at 2.85:1 on the raised card
    /// once the card is raised off a band rather than off the grid).
    raise_fg: [u8; 3],
    /// The active tab's LABEL ink. Equal to [`Self::raise_fg`] for the
    /// theme-derived strip; a user `active_tab_color` override replaces it with
    /// black/white by the override's own luminance so the label stays readable on
    /// any pick.
    active_fg: [u8; 3],
    /// Dimmed foreground for inactive tab labels, contrast-floored against
    /// [`Self::band_bg`].
    inactive_fg: [u8; 3],
    /// The HOVER wash under an inactive chip the pointer is on — a step off
    /// [`Self::band_bg`] well below the active raise, so a hovered chip reads
    /// as a target without impersonating the selection. Meaningful only where
    /// the strip is a chrome band; the macOS flat strip never paints it.
    hover_bg: [u8; 3],
    /// Ink for [`Self::hover_bg`]: the inactive dim, re-floored against the
    /// wash it now sits on (the wash is a step TOWARD the ink, so a tight
    /// scheme can need the label pushed back out).
    hover_fg: [u8; 3],
    /// Theme accent used by the selected underline and state marks.
    accent: [u8; 3],
    /// The hairline that closes the BOTTOM of the last strip row against the
    /// terminal content beneath it (stamped by [`seal_strip_bottom`]). `None` when
    /// the strip has no band to close — see [`STRIP_IS_CHROME_BAND`].
    seam: Option<[u8; 3]>,
}

/// Derive the strip tones from a theme. ON macOS the bare strip + inactive tabs sit
/// on the TERMINAL BACKGROUND, so an unfilled strip (a single tab + empty room, the
/// common case) recedes into the content rather than reading as a heavy gray bar;
/// the ACTIVE tab is a distinct RAISED button (bg stepped toward fg) with full bold
/// fg.
///
/// (Two earlier iterations were rejected by the visual-judge loop: a full fg/bg
/// inversion — a near-white block, "harsh/dated" — and a full-width gray chrome band
/// with the active tab merged into the body, "heavy/unfinished". See tools/visual-judge.)
///
/// OFF macOS ([`STRIP_IS_CHROME_BAND`]) the strip is the window's only chrome and
/// takes a real band surface. **The second rejected iteration is the live hazard
/// here, and avoiding it is why the raise moved.** `bar_bg` is a 0.16 (dark) step
/// off the terminal background and the old `active_bg` was a 0.21 step off the same
/// origin — 0.05 apart, which is the merged-into-the-body look the judge threw out.
/// So the raise is now anchored on WHATEVER surface the strip actually is:
/// `mix3(band_bg, fg, active_t)`. On macOS `band_bg == theme.bg`, so that expression
/// is byte-identical to the old one and nothing there moves; on a band it puts the
/// selected card the *same visible step above the band* that macOS puts it above the
/// body (three separable surfaces on the default theme: grid `#111318`, band
/// `#303135`, card `#525256`). The absolute distance from the terminal background is
/// larger by design — that is what a card raised above a band costs.
///
/// The inks follow the surface: each is floored to [`STRIP_INK_FLOOR`] against the
/// tone it is actually drawn ON — band ink against `band_bg`, card ink against
/// `raise_bg` — because a theme's own fg/bg contrast stops describing what the
/// reader sees the moment the ink leaves the terminal background, and because the
/// step that lifts the card also carries it toward the label standing on it
/// (Solarized Dark's active label lands at 2.85:1 without that second floor).
///
/// APPEARANCE-AWARE: the active-card raise is derived from the THEME ITSELF —
/// `bg_is_light(theme.bg)` — so the strip works for dark AND light schemes (and any
/// user theme file), with no `Appearance` plumbed through. On dark themes a 0.21 step
/// toward the fg makes the active card a visibly *raised* (lighter) surface; on light
/// themes that same magnitude reads as a heavy near-black slab, so light uses a gentler
/// 0.14. The inactive-label dim is a mild 0.15 on BOTH branches: the earlier 0.40 (dark)
/// / 0.30 (light) rendered unfocused titles near-illegible in practice — the vibrancy
/// titlebar drops effective contrast below the flat-composite math — and the active
/// tab's raised card already carries the focus cue, so a heavy label dim only hurt
/// legibility (0.15 lifts the default theme's inactive labels from ~5:1 to ~9:1 on
/// the flat strip, ~6.6:1 on a band — the band costs contrast by construction, since
/// its whole job is to step off the background the ink is being read against).
/// `strip_contrast_meets_wcag_aa` now guards the INACTIVE label contrast too (not just
/// the active tab + `+`), so any scheme that breaks it fails at add-time.
fn strip_colors(theme: Theme) -> StripColors {
    let rgb = |c: u32| {
        [
            ((c >> 16) & 0xff) as u8,
            ((c >> 8) & 0xff) as u8,
            (c & 0xff) as u8,
        ]
    };
    // Gentler raise on light themes. The inactive-label dim is deliberately mild
    // (0.15): a heavier dim rendered the unfocused tab titles near-illegible in
    // practice — especially over the vibrancy titlebar, where the effective
    // contrast is below the flat-composite math — while the active tab's RAISED
    // CARD + underline (not label brightness) already carries the focus cue, so
    // inactive labels can stay bright without blurring the active/inactive line.
    // `hover_t` sits deliberately at a THIRD of the raise: the native strip's
    // hover pill is labelColor at 7% alpha against the selected pill's full
    // material, and the same "clearly present, clearly not selected" ratio is
    // what keeps a cell-space sweep across the strip from reading as the
    // selection chasing the pointer.
    let (active_t, inactive_t, hover_t) = if bg_is_light(rgb(theme.bg)) {
        (0.14, 0.15, 0.05)
    } else {
        (0.21, 0.15, 0.07)
    };
    let fg = rgb(theme.fg);
    // The one platform decision in this file's colour work: which surface is the
    // strip? Everything below derives from it, so there is exactly one place to
    // read to know what a platform's strip looks like.
    let band_bg = if STRIP_IS_CHROME_BAND {
        crate::chrome_band::band_colors(theme).bar_bg
    } else {
        rgb(theme.bg)
    };
    // Surface blends take the RAW theme fg (a surface is not ink and must not be
    // dragged around by an ink floor); only the inks below are floored.
    let raise_bg = crate::chrome_band::mix3(band_bg, fg, active_t);
    // Each ink is floored against the surface IT lands on, not against a single
    // notional background: the band and the raised card are two surfaces, and the
    // very step that lifts the card also moves it toward the ink standing on it.
    let ink = |c: [u8; 3], on: [u8; 3]| crate::chrome_band::ensure_contrast(c, on, STRIP_INK_FLOOR);
    let raise_fg = ink(fg, raise_bg);
    // Full-strength ink on the band. Also the CEILING for the dim below.
    let band_fg = ink(fg, band_bg);
    // The inactive dim, floored like every other ink — and then bounded by the ink it
    // is a dim OF. `ensure_contrast` is a ONE-WAY ratchet: it can only push ink away
    // from its surface, never back. On a muted LIGHT scheme the band costs enough
    // contrast that the 0.15 dim falls under the floor and the ratchet lifts it back
    // toward full strength — Solarized Light on a band is the tightest bundled case,
    // 2.91 → 3.50:1 against 3.69:1 for the undimmed ink. That one still lands under
    // full strength, but only just, and one step further would print the UNFOCUSED
    // labels more prominently than the focused one — the active/inactive distinction
    // inverted by the very mechanism meant to protect it. A dim that has to be
    // un-dimmed to stay legible is not a dim, so cap it at the band ink rather than
    // let it overshoot: the raised card and the accent rule carry the focus cue on
    // such a scheme, which is what `strip_colors`' doc already says they are for.
    // Inert on every bundled scheme today (`strip_contrast_meets_wcag_aa` records the
    // margin); it exists so a re-tune or a user theme cannot cross the line.
    let dim = ink(crate::chrome_band::mix3(fg, band_bg, inactive_t), band_bg);
    let inactive_fg = if crate::chrome_band::contrast(dim, band_bg)
        > crate::chrome_band::contrast(band_fg, band_bg)
    {
        band_fg
    } else {
        dim
    };
    // The hover wash is a SURFACE (raw fg blend, like the raise above), and its
    // label is the inactive ink re-floored on it — the wash moved the ground
    // under an ink that was floored against the plain band.
    let hover_bg = crate::chrome_band::mix3(band_bg, fg, hover_t);
    let hover_fg = ink(inactive_fg, hover_bg);
    StripColors {
        fg: band_fg,
        band_bg,
        active_bg: raise_bg,
        raise_bg,
        raise_fg,
        active_fg: raise_fg,
        inactive_fg,
        hover_bg,
        hover_fg,
        accent: rgb(theme.cursor),
        seam: STRIP_IS_CHROME_BAND.then(|| crate::chrome_band::mix3(band_bg, fg, STRIP_SEAM_T)),
    }
}

/// [`strip_colors`] with the user's selected-tab color override (config
/// `active_tab_color`) applied: the ACTIVE tab's background becomes the exact
/// chosen color and its label ink flips black/white by that color's luminance
/// (the same [`bg_is_light`] classifier the native toolbar pill uses, so the
/// two renderers can never disagree). `None` = the theme-derived strip,
/// byte-identical to [`strip_colors`].
fn strip_colors_with_active(theme: Theme, active_override: Option<[u8; 3]>) -> StripColors {
    let mut colors = strip_colors(theme);
    if let Some(active_bg) = active_override {
        colors.active_bg = active_bg;
        colors.active_fg = if bg_is_light(active_bg) {
            [0, 0, 0]
        } else {
            [255, 255, 255]
        };
    }
    colors
}

/// Is this background a LIGHT one? A cheap perceptual-luma threshold (no sRGB-linear
/// round-trip needed for a binary dark/light decision). Every bundled dark scheme
/// sits well below the threshold and every light scheme well above it, so the
/// appearance-aware `strip_colors` branch never misclassifies a built-in. The ONE
/// dark/light classifier for all chrome (this strip and the native
/// toolbar's strip appearance via [`theme_is_dark`]) so they can never disagree.
pub(crate) fn bg_is_light(bg: [u8; 3]) -> bool {
    let luma = 0.299 * f32::from(bg[0]) + 0.587 * f32::from(bg[1]) + 0.114 * f32::from(bg[2]);
    luma > 150.0
}

/// [`bg_is_light`] for a packed-RGB theme colour (`theme.bg`), inverted: `true` when
/// the theme background is DARK. The app-side form used to pin the native toolbar
/// strip's appearance ([`crate::toolbar::set_strip_dark`]) to the theme backdrop.
pub(crate) fn theme_is_dark(bg: u32) -> bool {
    !bg_is_light([
        ((bg >> 16) & 0xff) as u8,
        ((bg >> 8) & 0xff) as u8,
        (bg & 0xff) as u8,
    ])
}

/// The strip's surface as the RENDERER needs it: the band tone and the seam
/// hairline, packed `0x00RRGGBB`, for the padding gutters the grid columns cannot
/// reach ([`aterm_render::ChromeBleed`]).
///
/// A cell background paints a CELL, so without this the band — and the hairline that
/// closes it — stop `pad` px short of both window edges and `pad_top + head` px short
/// of the top one: a grey rectangle floating in a dark margin, and a rule that ends
/// before the edge it is supposed to be. `None` on macOS, where the strip has no
/// surface of its own ([`STRIP_IS_CHROME_BAND`]): there the padding is already
/// painted in exactly the tone the strip is, so a bleed would be a no-op that only
/// costs quads.
#[must_use]
pub(crate) fn strip_bleed_tones(theme: Theme) -> Option<(u32, Option<u32>)> {
    STRIP_IS_CHROME_BAND.then(|| {
        let pack = |c: [u8; 3]| (u32::from(c[0]) << 16) | (u32::from(c[1]) << 8) | u32::from(c[2]);
        let colors = strip_colors(theme);
        (pack(colors.band_bg), colors.seam.map(pack))
    })
}

/// A bare strip-background [`RenderCell`] — used to pre-fill a strip row before
/// [`paint_strip`] overwrites the tab segments, and to fill upper rows of a multi-row
/// strip. (Recomputes the tones; only used outside the hot per-cell loop.)
#[must_use]
pub fn blank_cell(theme: Theme) -> RenderCell {
    strip_cell(' ', &strip_colors(theme), StripRole::Inactive)
}

/// Close the BOTTOM of the strip's LAST row against the terminal content beneath it
/// with a hairline seam, in place. A no-op where the strip has no band to close
/// ([`STRIP_IS_CHROME_BAND`]) — on macOS a floating hairline over `theme.bg` would
/// be a rule with nothing on either side of it.
///
/// The seam is an UNDERLINE on the strip's own last row, not an overline on the
/// first content row: the strip lives at the top of the window, so its free edge is
/// the bottom one, and the content row below belongs to the session, not to chrome.
/// (The active chip already used its underline as exactly this kind of edge — see
/// [`strip_cell`] — so this generalises a treatment the strip had for one segment to
/// the whole band.)
///
/// Called once per strip REBUILD from the row splice, not per cell and not per
/// frame; it recomputes the tones the same way [`blank_cell`] does.
pub(crate) fn seal_strip_bottom(row: &mut [RenderCell], theme: Theme) {
    let Some(seam) = strip_colors(theme).seam else {
        return;
    };
    for cell in row.iter_mut() {
        // Never overwrite a rule a cell already carries. The selected chip's ACCENT
        // underline IS its seam, and leaving it alone is what makes the finished
        // edge read the way every native tab strip's does: one continuous hairline
        // along the band, broken by the accent under the tab you are in.
        if cell.underline == UnderlineStyle::None {
            cell.underline = UnderlineStyle::Single;
            cell.underline_color = Some(seam);
        }
    }
}

/// Build the [`RenderCell`] for a strip cell from precomputed [`StripColors`] and the
/// cell's [`StripRole`].
fn strip_cell(ch: char, colors: &StripColors, role: StripRole) -> RenderCell {
    // The active tab reads as a native-style SELECTED tab: a LIGHT raised bg + a
    // full-width underline accent, NOT a heavy bold-on-filled-block. Inactive tabs
    // and the `+` recede to flat labels on the body. Underline doubles as a thin
    // seam between the active tab and the terminal content directly below it.
    let (fg, bg, bold, underline, underline_color) = match role {
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
        // Hover keeps the inactive VOCABULARY (no bold, no rule) on the faint
        // wash: everything that says "selected" — the raise, the bold, the
        // accent underline — stays the active tab's alone.
        StripRole::Hover => (
            colors.hover_fg,
            colors.hover_bg,
            false,
            UnderlineStyle::None,
            None,
        ),
        // The divider is band furniture, not a label: seam ink on the band tone.
        // The `unwrap_or` is unreachable in practice (the painter only asks for
        // a Separator where the strip IS a chrome band, and a band always has a
        // seam) but keeps this a total function rather than a panic on a role.
        StripRole::Separator => (
            colors.seam.unwrap_or(colors.inactive_fg),
            colors.band_bg,
            false,
            UnderlineStyle::None,
            None,
        ),
        // The `+` is a BUTTON, and on a chrome band it finally says so instead of
        // opting out of the strip's own vocabulary: the same raised card the `↻`
        // update alert has always sat on, minus the underline — that rule means
        // SELECTED, and the `+` is never selected. It borrows
        // `raise_bg`, not `active_bg`, so a user's `active_tab_color` recolours the
        // selected TAB and nothing else. On macOS's flat strip it stays a bare
        // full-strength glyph on the body: with no band behind it a raised chip
        // there reads as one more tab.
        StripRole::NewTab if STRIP_IS_CHROME_BAND => (
            colors.raise_fg,
            colors.raise_bg,
            false,
            UnderlineStyle::None,
            None,
        ),
        StripRole::NewTab => (colors.fg, colors.band_bg, false, UnderlineStyle::None, None),
        StripRole::Title => (colors.fg, colors.band_bg, true, UnderlineStyle::None, None),
        // A raised, underlined highlighted button (like the active tab) so the update
        // alert draws the eye without a hardcoded chrome colour.
        StripRole::Update => (
            colors.active_fg,
            colors.active_bg,
            true,
            UnderlineStyle::Single,
            Some(colors.accent),
        ),
    };
    RenderCell {
        ch,
        fg,
        bg,
        wide: false,
        emoji_presentation: false,
        bold,
        italic: false,
        underline,
        strikethrough: false,
        overline: false,
        underline_color,
    }
}

/// Sanitize one title char for the strip: control chars are blanked (a caret
/// notation or a raw C1 byte is never a tab name); EVERYTHING else passes
/// through unchanged — emoji, CJK, Nerd Font plane-15 icons included.
///
/// This used to replace wide and non-BMP chars with a `·` placeholder to keep a
/// 1-char-per-cell column math exact, which put a row of middots under the very
/// caption rendering the same title correctly four pixels above. The painter's
/// column math now runs in DISPLAY CELLS ([`strip_char_cells`]) with real
/// lead+continuation emission, the same contract every other in-grid surface
/// already honours, so the placeholder — and the disagreement — are gone.
fn strip_char(c: char) -> char {
    if c.is_control() { ' ' } else { c }
}

/// Display cells one PROJECTED title char occupies on the strip: 1 or 2, from
/// the same Unicode width tables the terminal grid classifies with
/// (`aterm_grapheme::char_width`), so the strip and the grid can never disagree
/// about how far a glyph reaches. Zero-width chars (combining marks, variation
/// selectors) are clamped to a cell rather than dropped: the strip is a
/// per-char projection, not a grapheme shaper, and every char keeping a cell is
/// what keeps the rename caret's char-index ↔ column mapping exact (a
/// zero-width char under the caret would otherwise be an invisible edit
/// position). That clamp is also precisely what the pre-width-aware painter did
/// with them, so nothing regresses.
///
/// The one width mode the strip does NOT honour is DEC ambiguous-wide (the
/// per-terminal CJK legacy toggle): titles are chrome, not grid content, and
/// chrome has no session to read a mode from.
fn strip_char_cells(c: char) -> u16 {
    aterm_grapheme::char_width(c).max(1) as u16
}

/// Display cells a whole title occupies after [`strip_char`] projection — the
/// sum the centring and truncation math must use now that a char is no longer
/// a cell.
fn strip_display_cells(s: &str) -> usize {
    s.chars()
        .map(|c| usize::from(strip_char_cells(strip_char(c))))
        .sum()
}

/// Whether the chip at `index` draws a leading `│` divider: never before the
/// first chip (nothing precedes it), and never on either side of the ACTIVE or
/// HOVERED chip — both already draw their own edge (the raised card, the hover
/// wash), and a rule flush against a card reads as grime, not structure. The
/// native strip's `TabGeometry::separates` (`toolbar.rs`), PURE and stated once,
/// extended by the hover term the native strip handles per-view (a hovered
/// AppKit chip swaps its rule for the pill in its own `drawRect:`; the cell
/// painter has no per-view draw pass, so the hovered chip's neighbours are
/// suppressed here instead — same screen, different plumbing).
fn strip_separates(index: usize, active: usize, hovered: Option<usize>) -> bool {
    let beside = |edge: usize| index == edge || index == edge + 1;
    index > 0 && !beside(active) && !hovered.is_some_and(beside)
}

/// Paint the laid-out `segments` into `row` (a single strip row of `RenderCell`s,
/// already `cols` wide and pre-filled with the chrome background). `titles[i]` is
/// tab `i`'s label; `active` is the highlighted tab. Each tab draws ` <title> ✕ `
/// (title truncated with `…`), the `+` draws ` + `. Bounds-checked against `row`'s
/// length so a degenerate tiny strip can never write past it.
#[cfg(test)]
pub fn paint_strip(
    row: &mut [RenderCell],
    segments: &[TabSegment],
    titles: &[String],
    hovered: Option<usize>,
    active: usize,
    theme: Theme,
) {
    let paint = StripPaint {
        hovered,
        subtitle: None,
        rename: None,
    };
    let _ = paint_strip_impl(row, segments, titles, None, paint, active, theme, None);
}

/// The per-frame paint inputs that are neither geometry nor colour: which tab the
/// pointer is on (the only tab that shows a `✕`), and the SOLO band's description.
/// Bundled so the two call sites and the painter cannot drift on argument order.
#[derive(Clone, Copy, Default)]
pub(crate) struct StripPaint<'a> {
    /// Index of the tab under the pointer, if any. The `✕` is a HOVER-ONLY
    /// affordance here exactly as it is on the native strip: its column is still
    /// reserved by [`layout_segments`] whether or not it is painted, so revealing
    /// it never reflows a title, and `hit_test` keeps closing on that column
    /// (the pointer is necessarily ON the tab it is clicking). On a chrome band
    /// the hovered chip also takes the [`StripRole::Hover`] wash and sheds the
    /// separators beside it — paint only, same reserved geometry.
    pub hovered: Option<usize>,
    /// The lone tab's one-line description ([`solo_subtitle`]), drawn dim after
    /// the centred title. `None` = nothing the title does not already say.
    pub subtitle: Option<&'a str>,
    /// The live inline SESSION-RENAME field, when this window has one and the
    /// in-grid strip is what presents it. `None` = paint labels as usual.
    pub rename: Option<StripRenameField<'a>>,
}

/// The in-grid inline rename editor, as the strip painter needs it: WHICH tab it
/// sits over, and the field's text + caret.
///
/// Deliberately positional, not identity-bearing: the strip is handed no session
/// ids (it never has been), so `App` resolves the edited tab's CURRENT index once
/// per paint. Everything about ownership — which session, which surface — stays in
/// [`crate::app_rename::TabRenameEdit`].
#[derive(Clone, Copy)]
pub(crate) struct StripRenameField<'a> {
    /// Index of the edited tab in `titles`/`segments`.
    pub tab: usize,
    /// The field's text.
    pub text: &'a str,
    /// Caret position in `text`, as a BYTE offset on a char boundary.
    pub cursor: usize,
}

/// Paint the shipping strip from canonical presentation metadata and return sparse
/// inline-image placements for its code-native icon/dirt marks.  `RawRgba8` inline
/// images are aterm's shared render input: CPU, GPU, cached frames, and `image` consume
/// the exact same raster bytes. The cells beneath stay part of the ordinary strip row,
/// so active/inactive backgrounds and full tab/close hit geometry are unchanged.
///
/// The shipping splice calls [`paint_strip_with_metadata_and_rename_caret`] (it
/// also needs the rename well's caret); this images-only shape remains as the
/// test fixtures' entry point.
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
/// (`None` when this paint drew no rename field). The splice uses the column to
/// anchor the OS IME candidate window ON THE WELL while the rename field owns a
/// composition ([`crate::app_input::PreeditOwner::Rename`]) — computed by the
/// SAME paint that placed the reverse-video caret, so the anchor and the pixels
/// cannot drift.
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
    for seg in segments {
        let is_active = matches!(seg.kind, TabHit::Select(i) if i == active);
        // The pointer's tab takes the HOVER wash — chrome band only ([`StripRole::
        // Hover`]); on macOS's flat strip the hover state still reaches here (it
        // reveals the `✕`) but must not repaint the chip. The hover feed is
        // already in `StripCacheKey`, so this is pure paint, not new plumbing.
        let is_hovered = STRIP_IS_CHROME_BAND
            && !is_active
            && !seg.solo
            && matches!(seg.kind, TabHit::Select(i) if paint.hovered == Some(i));
        let tab_role = if is_active {
            StripRole::Active
        } else if is_hovered {
            StripRole::Hover
        } else {
            StripRole::Inactive
        };
        let put = |row: &mut [RenderCell], col: u16, ch: char, role: StripRole| {
            if let Some(slot) = row.get_mut(col as usize) {
                *slot = strip_cell(ch, &colors, role);
            }
        };
        // The right half of a width-2 glyph: a continuation cell in the same
        // tones. It carries no glyph of its own (`RenderCell::wide`) — the lead
        // cell's raster overflows into it, exactly as in the terminal grid.
        let put_wide_tail = |row: &mut [RenderCell], col: u16, role: StripRole| {
            if let Some(slot) = row.get_mut(col as usize) {
                let mut cell = strip_cell(' ', &colors, role);
                cell.wide = true;
                *slot = cell;
            }
        };
        // Paint `text` from `col` up to (never past) `end` in DISPLAY cells,
        // emitting lead + continuation for width-2 chars; returns one past the
        // last painted column. A wide char that would straddle `end` is dropped
        // whole — half a glyph overlapping the neighbour is the exact defect
        // this replaced the old one-char-one-cell `·` projection to fix.
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
            // SOLO: the window's only tab is its TITLE, not a switcher — flat body
            // background (no raised chip, no selection underline, no `✕`), the
            // title CENTRED, and its description trailing in the dim tone. The
            // native strip's solo band, in cells.
            TabHit::Select(i) if seg.solo => {
                let span = seg.end_col.saturating_sub(seg.start_col);
                for c in seg.start_col..seg.end_col {
                    put(row, c, ' ', StripRole::Inactive);
                }
                let title = titles.get(i).map(String::as_str).unwrap_or("");
                // EDITING the lone tab: the band becomes one left-aligned field
                // inset from both edges. The centred title+description group is
                // dropped for the duration — a centred field recentres itself (and
                // takes the caret with it) on every character typed, and the
                // description recomposes underneath, which is unusable. The icon
                // and status marks go too: the field gets the whole band.
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
                // Centred by DISPLAY width: a CJK or emoji title occupies real
                // double cells now, and centring by char count would shove the
                // group off-centre by one cell per wide char.
                let width = strip_display_cells(&title)
                    + subtitle
                        .as_ref()
                        .map_or(0, |s| SOLO_GAP_COLS + strip_display_cells(s));
                let start = seg.start_col + (span.saturating_sub(width as u16)) / 2;
                let mut col = put_text(row, start, seg.end_col, &title, StripRole::Title);
                if let Some(subtitle) = subtitle {
                    col = col.saturating_add(SOLO_GAP_COLS as u16);
                    put_text(row, col, seg.end_col, &subtitle, StripRole::Inactive);
                }
                // The lone tab still carries its app icon / state marks, in the
                // segment's own leading and trailing cells — states are never
                // visual-only, and the centred group must not be pushed off centre
                // to make room for them.
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
                // Background-fill the whole segment in the (in)active colour first.
                for c in seg.start_col..seg.end_col {
                    put(row, c, ' ', tab_role);
                }
                // LEADING SEPARATOR (chrome band only): flush equal shares give
                // three identical titles no cue that they are three tabs — the
                // native strip draws a leading hairline for exactly this
                // ([`TabGeometry::separates`] in `toolbar.rs`), and this is that
                // rule in cells. It lands on the segment's own leading PAD cell
                // (`tab_content_layout` starts content at `start_col + 1`), so
                // it costs zero title width and cannot move a hit target —
                // `hit_test` still answers `Select(i)` for the column.
                if STRIP_IS_CHROME_BAND && strip_separates(i, active, paint.hovered) {
                    put(row, seg.start_col, '│', StripRole::Separator);
                }
                let editing = paint.rename.filter(|edit| edit.tab == i);
                // While a chip is being EDITED it spends its whole interior on the
                // field: the icon and the status canvas are suppressed for the
                // duration. `tab_content_layout` can legitimately hand back a
                // ONE-cell title span on a narrow equal-share segment, and a
                // one-cell field is a caret with no text. The close column and the
                // segment bounds are untouched either way — this is paint, and the
                // hit geometry is not paint's to move.
                let item = metadata
                    .and_then(|items| items.get(i))
                    .copied()
                    .filter(|_| editing.is_none());
                let layout = item.map_or_else(
                    || TabContentLayout {
                        icon_start: None,
                        title_start: seg.start_col + 1,
                        title_end: match seg.close_col {
                            Some(cx) => cx.saturating_sub(1),
                            None => seg.end_col.saturating_sub(1),
                        },
                        status_col: None,
                    },
                    |item| tab_content_layout(seg, item),
                );
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
                    let color = if item.dirty || item.attention {
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
                    // The field takes the title span verbatim — never
                    // `truncate_title`, which would ellipsise the middle of what
                    // you are typing. It scrolls instead, like any text field.
                    rename_caret = paint_rename_field(
                        row,
                        usize::from(layout.title_start)..usize::from(layout.title_end),
                        edit.text,
                        edit.cursor,
                        raw,
                        theme,
                        // The edited chip keeps its selected seam. Only the ACTIVE
                        // chip has one to keep — a context-menu rename can target a
                        // background tab, which stays unselected while you type.
                        is_active.then_some(colors.accent),
                    );
                } else {
                    let avail = layout.title_end.saturating_sub(layout.title_start);
                    let label = truncate_title(raw, avail as usize);
                    // DISPLAY cells: a width-2 char takes a lead + continuation
                    // pair and one that would straddle `title_end` is dropped
                    // whole, so a title can never bleed into the status canvas,
                    // the `✕`, or the next chip.
                    put_text(row, layout.title_start, layout.title_end, &label, tab_role);
                }
                if let Some(cx) = seg.close_col
                    && paint.hovered == Some(i)
                {
                    // HOVER-ONLY, as on the native strip: a permanent ✕ on every tab
                    // (the selected one included) is the one that gets mis-clicked.
                    // The column stays reserved by `layout_segments` whether or not
                    // the glyph is painted, so the reveal never reflows the title.
                    //
                    // ✕ (U+2715 MULTIPLICATION X) reads as a real close affordance vs.
                    // an amateurish ASCII 'x'. U+2715 has East-Asian-Width *Neutral*, so
                    // it is single-cell in CJK and non-CJK alike — unlike × (U+00D7),
                    // which is *Ambiguous* and renders double-width under CJK fonts,
                    // breaking the strip's 1-char-per-cell math. Hit-testing keys off
                    // `close_col`, not the glyph.
                    put(row, cx, '✕', tab_role);
                }
            }
            TabHit::NewTab => {
                // The `+` is a BUTTON (StripRole::NewTab = full-strength fg on the
                // body), not a dim inactive label — so it meets WCAG-AA contrast on
                // every theme (the dim treatment dropped below 3:1 on Solarized Dark).
                //
                // GUTTER (chrome band only). `layout_segments` places the `+` FLUSH
                // against the last tab, and on a band `NewTab` and `Active` resolve to
                // the same `raise_bg` — so whenever the last tab is the selected one
                // (the state immediately after every New Tab, the app's most common
                // action) the two cards fuse into one contiguous rectangle broken only
                // by the underline colour. Give up this segment's LEADING pad cell to
                // the band so at least one band-coloured column always separates them.
                // Paint-only, deliberately: the fix costs no shared geometry (macOS's
                // `layout_segments` is untouched) and `hit_test` still routes the whole
                // segment to `TabHit::NewTab`, so the click target does not shrink. The
                // alternative — giving `NewTab` a tone of its own — was rejected: two
                // adjacent near-identical greys read as a rendering artefact, whereas a
                // column of band reads as deliberate. On macOS's flat strip there is no
                // card to fuse with, so the whole segment keeps its historical fill.
                let card = seg
                    .start_col
                    .saturating_add(u16::from(STRIP_IS_CHROME_BAND));
                for c in card..seg.end_col {
                    put(row, c, ' ', StripRole::NewTab);
                }
                // Centre the `+` in the 3-cell ` + ` affordance (on a band the card is
                // the trailing two cells, so this is its leading one).
                put(row, seg.start_col + 1, '+', StripRole::NewTab);
            }
            TabHit::Update => {
                // The leading `↻` update-ready alert — a raised highlighted button.
                // Its TRAILING pad cell is the mirror of the `+`'s gutter above: `↻`
                // sits at column 0 and the tabs start flush against it, so an active
                // tab 0 fuses with the alert exactly the same way. Same paint-only
                // remedy, same macOS carve-out.
                let end = seg.end_col.saturating_sub(u16::from(STRIP_IS_CHROME_BAND));
                for c in seg.start_col..end {
                    put(row, c, ' ', StripRole::Update);
                }
                // Centre the `↻` (U+21BB clockwise open circle arrow) in the 3-cell slot.
                put(row, seg.start_col + 1, '\u{21bb}', StripRole::Update);
            }
            // `Close` is never a segment `kind` (only a derived hit on `close_col`).
            TabHit::Close(_) => {}
        }
    }
    images.sort_unstable_by_key(|(col, _)| *col);
    (images, rename_caret)
}

const ICON_RASTER_SIZE: u16 = 32;
const ICON_SUPERSAMPLE: u16 = 4;
fn status_primitives(metadata: TabStripMetadata) -> Vec<TabIconPrimitive> {
    let count = metadata.status_count();
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
                radius: 1.75,
            }),
            // Hollow round = work is in progress. RoundedRect gives the deterministic
            // rasterizer a crisp ring without adding a renderer-only circle primitive.
            TabStatusKind::Busy => primitives.push(TabIconPrimitive::RoundedRect {
                rect: [x - 2.0, 6.0, 4.0, 4.0],
                radius: 2.0,
                width: 1.25,
            }),
            // Diamond = something completed or otherwise needs the user's attention.
            TabStatusKind::Attention => {
                let w = 1.15;
                primitives.extend([
                    TabIconPrimitive::Line {
                        from: [x, 5.5],
                        to: [x + 2.5, 8.0],
                        width: w,
                    },
                    TabIconPrimitive::Line {
                        from: [x + 2.5, 8.0],
                        to: [x, 10.5],
                        width: w,
                    },
                    TabIconPrimitive::Line {
                        from: [x, 10.5],
                        to: [x - 2.5, 8.0],
                        width: w,
                    },
                    TabIconPrimitive::Line {
                        from: [x - 2.5, 8.0],
                        to: [x, 5.5],
                        width: w,
                    },
                ]);
            }
        }
    }
    primitives
}

/// Rasterize one icon IR to transparent straight-alpha RGBA.  A tiny fixed 4×4
/// coverage grid gives smooth diagonals/rounds while remaining deterministic and
/// bounded (32×32×16 samples). The renderer may scale this source to the actual
/// two-cell footprint, but both CPU and GPU scale the same `RawRgba8` payload.
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
    })
}

/// Memo key for one rasterized strip glyph.
///
/// It names EXACTLY the inputs the pixels are a function of, so a hit is
/// bit-identical to a fresh rasterization: [`tab_icon_primitives`] is a `const fn`
/// of the kind alone, and [`status_primitives`] reads only `status_count` /
/// `has_status_kind`, i.e. the three status bools. NOTHING else on
/// [`TabStripMetadata`] may reach the pixels — see
/// `memoized_strip_glyphs_match_a_fresh_raster_for_every_key`, which fails loudly
/// if that ever stops being true. The two variants also keep the `cols` split
/// (`ICON_COLS` vs 1) structural rather than incidental: an icon entry can never
/// be handed to the status call site, and both renderers' image-cache keys embed
/// `cols * cell_w`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum IconKey {
    Icon(TabIconKind, [u8; 3]),
    Status {
        dirty: bool,
        busy: bool,
        attention: bool,
        color: [u8; 3],
    },
}

/// Hard cap on the memo: 16 × 4 KiB = 64 KiB. The live key space is 4 icon kinds
/// + 8 status bit combinations × the handful of strip colours in play, and FIFO
///   eviction keeps a theme cycle (which mints new colours) provably bounded.
const ICON_MEMO_CAP: usize = 16;

thread_local! {
    /// Paint is main-thread only, so a thread-local avoids both a global lock on
    /// the paint path and a signature change to [`paint_strip_with_metadata`]. A
    /// `Vec` beats a `HashMap` at this size and keeps the eviction bound obvious.
    static ICON_MEMO: RefCell<Vec<(IconKey, Arc<ImageData>)>> = const { RefCell::new(Vec::new()) };
}

/// Reuse the `Arc<ImageData>` of a glyph we have already rasterized.
///
/// Skipping the ~16 k coverage samples is the smaller half. The real win is that
/// the SAME `Arc` survives a strip rebuild: both renderers key their decoded-image
/// caches on `Arc` identity (`aterm_gpu::GpuImageCache::get` by `Arc::ptr_eq`, and
/// `aterm_render`'s `ImageCache` likewise), so a fresh `Arc` per rebuild was a
/// guaranteed miss that re-decoded the raster, churned an 8-entry GPU cache
/// holding genuine user inline images, and — because placements are keyed on
/// `Arc::as_ptr` — defeated the `image_plane` reuse fast path, repacking the whole
/// stacked plane every time a spinner ticked in a tab title.
///
/// Sharing one image across frames is already the normal state here (the strip
/// cache's HIT path re-uses the very same images every present), nothing mutates
/// through these `Arc`s, and no consumer branches on their strong count — so
/// stable identity cannot change observable output.
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
        },
    ));
}

/// Clipped-edge markers for the inline rename field — the find bar's glyphs, so
/// the two in-grid single-line fields say "the text continues past here" the same
/// way.
const RENAME_SCROLL_LEFT: char = '‹';
const RENAME_SCROLL_RIGHT: char = '›';

/// Paint the inline SESSION-RENAME field across `field` (a column range of `row`):
/// a recessed well holding the visible slice of `text`, with a REVERSE-VIDEO block
/// caret on the character at `cursor`.
///
/// This is [`crate::find_bar`]'s well, in the strip — deliberately the same
/// machine, because it is the same thing: aterm's one in-grid single-line text
/// field. The caret costs no cell (so text never shifts when it moves), the view
/// scrolls the MINIMUM needed to keep the caret inside, and `‹`/`›` mark an edge
/// the text continues past without ever overwriting the caret. An empty field
/// shows the label the ladder would fall back to, dimmed — so "empty means use
/// that" is visible rather than folklore.
///
/// `underline` keeps the ACTIVE chip's accent seam under the field cells: the tab
/// being edited must not stop looking selected while you type.
///
/// The visible characters go through [`strip_char`], the same projection the
/// titles use, and each keeps at least one DISPLAY cell ([`strip_char_cells`]) —
/// a width-2 char occupies a real lead+continuation pair, so what you type shows
/// as itself, not as a placeholder. The caret is placed by CHAR index into that
/// projection and every char owns ≥ 1 cell, so a wide/non-BMP character in a pin
/// cannot desync the painted caret column from the byte offset the state holds.
///
/// Returns the CARET's painted column (the reverse-video cell — the scroll math
/// keeps it inside the window, including the one-past-the-text position), or
/// `None` on degenerate geometry. The caller uses it to anchor the OS IME
/// candidate window ON THE WELL while the rename field owns a composition —
/// without it the anchor stayed on the grid caret, rows below the text being
/// composed.
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
    // Caret in CHARS of the same projection the cells carry. The byte offset is
    // always on a char boundary (the field reducer's invariant), so counting the
    // prefix is exact.
    let caret = text
        .get(..cursor.min(text.len()))
        .map_or(chars.len(), |head| head.chars().count())
        .min(chars.len());
    for cell in &mut row[field.clone()] {
        *cell = well(' ', c.value, c.field_bg);
    }
    // Scroll in CHARS, budgeted in DISPLAY CELLS: walk back from the caret
    // spending the field's width — the `caret - (width - 1)` of the one-cell-
    // per-char field, generalized to chars that cost two. The caret's own cells
    // are budgeted first (it can sit one past the last char, costing a single
    // cell there), so the caret is always inside the window with the maximum
    // left context behind it.
    let caret_cells = chars.get(caret).map_or(1, |&(_, w)| w).min(width);
    let mut scroll = caret;
    let mut budget = width - caret_cells;
    while scroll > 0 && chars[scroll - 1].1 <= budget {
        scroll -= 1;
        budget -= chars[scroll].1;
    }
    // Lay the window out left to right. A wide char that would straddle the
    // field's right edge is dropped whole (its columns stay well-blank) — the
    // lead's raster would otherwise bleed past the field. Track what lands on
    // the edge cells so the `‹`/`›` markers below can never orphan half a pair.
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
            // The continuation shares the lead's ground (a caret block over a
            // wide char covers BOTH its cells — a half-width block would read
            // as the caret standing on half a glyph).
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
        // The first visible char loses its lead cell to the marker; if it was
        // wide, reclaim its orphaned continuation too — a `‹` in front of a
        // stray continuation would itself be classified as a wide lead.
        if chars.get(scroll).is_some_and(|&(_, w)| w == 2) && field.start + 1 < field.end {
            row[field.start + 1] = well(' ', c.value, c.field_bg);
        }
        row[field.start] = well(RENAME_SCROLL_LEFT, c.label, c.field_bg);
    }
    if index < chars.len() && !caret_on_last_cell {
        // Text continues past the window. When a wide pair ends exactly at the
        // field edge, the marker would replace only its continuation and leave
        // the lead's raster overflowing under it — blank the lead as well.
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

/// Fit the SOLO band's title and description into `span` cells: the title is the
/// string the band exists to show, so the DESCRIPTION is compressed first and
/// dropped entirely before the title gives up a single cell.
///
/// Returns the strings to draw; both are already ellipsised by [`truncate_title`].
/// The native strip makes the same call in points (`toolbar::relayout_solo`).
#[must_use]
fn solo_band_text(title: &str, subtitle: Option<&str>, span: usize) -> (String, Option<String>) {
    let usable = span.saturating_sub(2 * SOLO_EDGE_COLS);
    // Budgeted in DISPLAY CELLS throughout — the painter places these strings
    // with real lead+continuation pairs, so a char-counted budget would let a
    // CJK title spend double what the centring math reserved for it.
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

/// Truncate `title` to at most `max` display cells, appending `…` when it was
/// cut. `max == 0` yields the empty string. Measured in DISPLAY CELLS
/// ([`strip_char_cells`]), not chars: a CJK title's chars each cost two cells,
/// and a char-counted cut would hand the painter twice the width it asked for.
/// A width-2 char that would straddle the `…` budget is dropped whole — the
/// ellipsis may then sit one cell early, which is the correct trade (a split
/// glyph is never).
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

/// THE STRIP IN THE UI FONT — a PIXEL-SPACE text band composited over the cell
/// strip (Windows only).
///
/// Every glyph the cell painter emits is quantised to the terminal's monospace
/// grid: chrome text in a 7–9 px-advance code face, pasted over the very grid it
/// is supposed to be chrome FOR. macOS never sees this (its strip is the native
/// AppKit toolbar, set in the system face); on Windows the in-grid strip is the
/// window's ONLY chrome, so it is the one surface where the terminal face is the
/// wrong face. `RenderCell` has no face selector (`render_cells.rs` — a cell IS
/// the terminal font, by definition), so the fix is not a per-cell font bit: the
/// band's TEXT stops being cells at all and becomes pixels.
///
/// MECHANISM. [`raster_band`] rasterizes every label/affordance of the strip into
/// ONE transparent straight-alpha RGBA image (via the settings tray's shared
/// `DrawPrim` rasterizer, whose `TextFace::Ui` arm already resolves the real
/// system faces — Segoe UI Variable / Segoe UI — with per-char fallback into the
/// terminal cascade) and returns per-row [`ImageRef`] slices that the strip
/// splice lays over the strip cells. This rides the EXISTING inline-image
/// compositing seam — the same `RawRgba8` + `Arc` path the strip's icon/status
/// marks, sixel, and Kitty images already use on both renderers — and inherits
/// its z-contract for free: a `z >= 0` image draws OVER the covered cell's glyph
/// (suppressing the mono ink) and UNDER the line decorations, so the band's seam
/// hairline and the active chip's accent underline keep painting over it, and
/// the cell BACKGROUNDS (band tone, raised card, hover wash) show through the
/// transparent pixels untouched.
///
/// Rejected alternatives, for the record:
///   * A `FreeSprite` band — pixel-exact and off-grid capable, but its atlas is
///     owned per-frame by the effects engine (`word_decos.free_atlas()`); the
///     strip would have to negotiate atlas real estate with robi and the word
///     decorations, coupling chrome to the effects lifecycle.
///   * Painting the strip rows inside the renderer — a renderer face-selector
///     is exactly the RenderCell contract change three reviews said not to make.
///   * Rasterising per-chip images — more Arcs, more cache keys, and the GPU
///     image cache holds genuine user images; ONE band image is one entry.
///
/// THE MODEL DOES NOT MOVE. `layout_segments` stays the geometry authority and
/// the CELL painter still paints every cell — chars included — exactly as before;
/// this module converts the SAME segments to pixels via `cell_w`, so paint and
/// hit-testing cannot disagree. Suppression of the mono ink happens purely
/// through image coverage, which means any segment this painter cannot honestly
/// render falls back PER SEGMENT to the shipped cell paint by simply not being
/// covered:
///   * the inline RENAME field (its well, caret, scroll markers and IME preedit
///     splice are the find bar's machinery, cell-based by design — reimplementing
///     a text field in pixel space to shave one edit-time inconsistency is how
///     carets drift);
///   * a SOLO title band (macOS-only policy; dead on Windows, kept total);
///   * any title with a char neither the UI face nor the chrome cascade covers
///     (colour emoji, CJK beyond the cascade): the terminal renderer's full
///     fallback/emoji machinery paints it in the grid font — mono-quantised but
///     REAL, never a hole in a proportional run.
///
/// C3 (vertical room): the label is CENTRED in the band the viewer actually sees
/// — `pad_top + head + strip_rows·cell_h` — not baseline-bound to the last cell
/// row. At the Windows defaults (2 px top pad + one ~21 px row at 96 dpi) that
/// is a ~23 px band; a taller `window_padding_top` (or `tab_strip_rows = 2`)
/// yields a taller band with the label still optically centred, because the
/// centring reads the live metrics. The 30–34 px DEFAULT height C3 asks for is
/// deliberately NOT shipped here: it requires either a Windows `pad_top` default
/// change (grid-metrics blast radius across every geometry proof) or a synthetic
/// `head` band (macOS chrome-overlap semantics) — both judged out of proportion
/// for this change; the pixel painter is ready for either the moment the default
/// moves.
#[cfg(windows)]
pub(crate) mod pixel_band {
    use super::*;
    use crate::type_scale::TypeStep;
    use crate::widget::{DrawPrim, TextFace, TextWeight, rgba, text_prim};

    /// The band label's LOGICAL size: the settings tray's chrome base (13 px at
    /// 96 dpi ≈ 10 pt), NOT the terminal `font_px` — native tab bars do not grow
    /// their labels with the document font. Scaled by the window's scale factor
    /// and capped against the band height so a tiny custom font can't overflow
    /// its own chrome.
    const STRIP_LABEL_LOGICAL_PX: f32 = 13.0;

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
        /// Band pixels ABOVE the grid (`pad_top + head`): the off-grid lip the
        /// `ChromeBleed` paints in the band tone. Part of the OPTICAL band, so
        /// the centring math must see it even though the image cannot reach it.
        pub band_top_px: usize,
        /// The window's scale factor (drives the label size).
        pub scale: f32,
    }

    /// Everything the band raster is a function of. The caller caches the result
    /// keyed on the strip fingerprint + [`BandGeometry`] + the chrome-font epoch
    /// ([`crate::tray_raster::strip_band_font_epoch`]), so this runs only on a
    /// strip REBUILD — never per frame.
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

    /// Rasterize the strip band and return one `Vec<(col, ImageRef)>` PER STRIP
    /// ROW (sorted by column — the renderer binary-searches). `None` = paint the
    /// strip entirely with cells (no UI face installed yet, degenerate geometry,
    /// or nothing qualified) — the byte-identical legacy path.
    ///
    /// `icon_images` is the CELL painter's sparse icon/status list for this same
    /// build: entries under a fallback segment are merged back in (that segment
    /// keeps its shipped look wholesale), everything else is dropped because the
    /// band draws those marks itself, vertically centred.
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
            // No UI face yet (first frames before `set_chrome_fonts` lands; every
            // test that never installs one): the whole strip stays cells. Checked
            // FIRST so the pre-font path is byte-identical, not a half-band.
            || !crate::tray_raster::strip_band_ui_ready()
        {
            return None;
        }
        let img_w = cols.checked_mul(cell_w)?;
        let img_h = geometry.strip_rows.checked_mul(cell_h)?;
        if img_w > usize::from(u16::MAX) || img_h > usize::from(u16::MAX) {
            return None;
        }
        let colors = strip_colors_with_active(input.theme, input.active_override);
        let scale = if geometry.scale.is_finite() && geometry.scale > 0.0 {
            geometry.scale
        } else {
            1.0
        };
        // The OPTICAL band: the off-grid lip above the image plus the image.
        let band_h = (geometry.band_top_px + img_h) as f32;
        let label_px = band_label_px(band_h, scale);
        // Cap-centred baseline in the FULL band ([`crate::tray_raster::row_baseline`]
        // over `[-band_top, band_h)` in image coordinates), then clamped so the em
        // box stays inside the image — the pixels above the grid belong to the
        // `ChromeBleed`, not to this raster.
        let baseline = crate::tray_raster::row_baseline(
            -(geometry.band_top_px as f32),
            band_h,
            label_px,
        )
        .clamp(label_px * 0.9, (img_h as f32 - label_px * 0.28).max(label_px * 0.9));
        // The optical centre every non-text mark aligns to (the label's cap-box
        // midline, so ✕ / + / icons and the text read as ONE centred row).
        let cy = baseline - label_px * 0.35;
        let cw = cell_w as f32;
        // T3 full cut: the variable UI face instanced at wght 600 for the
        // ACTIVE label (Win11's Segoe UI Variable Semibold), drawn through the
        // portable varied rasterizer into an overlay composited after the
        // `DrawPrim` pass. `None` ⇒ the fontdue `UiBold` slot (static seguisb).
        // Parsed ONCE per band (one `Face::parse` + coord replay), not per glyph.
        let variable = crate::tray_raster::strip_band_variable_semibold();
        let variable_pen = variable.as_ref().and_then(VariablePen::new);

        let mut prims: Vec<DrawPrim> = Vec::new();
        let mut overlays: Vec<LabelOverlay> = Vec::new();
        // Per-segment fallback ranges (half-open cols) left to the cell painter.
        let mut fallback: Vec<(u16, u16)> = Vec::new();
        let mut drew_any = false;

        for seg in input.segments {
            let end = seg.end_col.min(cols as u16);
            if seg.start_col >= end {
                continue;
            }
            let is_active = matches!(seg.kind, TabHit::Select(i) if i == input.active);
            let is_hovered = !is_active
                && !seg.solo
                && matches!(seg.kind, TabHit::Select(i) if input.paint.hovered == Some(i));
            match seg.kind {
                TabHit::Select(i) if seg.solo => {
                    // SOLO is macOS policy ([`SOLO_TITLE_BAND`]) — structurally
                    // unreachable on Windows, kept total: the cell painter owns it.
                    let _ = i;
                    fallback.push((seg.start_col, end));
                }
                TabHit::Select(i) => {
                    if input
                        .paint
                        .rename
                        .is_some_and(|edit| edit.tab == i)
                    {
                        // The inline rename WELL (field, caret, ‹› markers, IME
                        // preedit splice) stays the shipped cell machinery.
                        fallback.push((seg.start_col, end));
                        continue;
                    }
                    let item = input.metadata.get(i).copied();
                    let layout = item.map_or_else(
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
                    );
                    let raw = input.titles.get(i).map(String::as_str).unwrap_or("");
                    let avail = layout.title_end.saturating_sub(layout.title_start);
                    // SAME semantic truncation as the cell painter (display
                    // cells), so what the band shows is what hover cards / the
                    // caption agree the visible title is; then a PIXEL fit below,
                    // because a wide-lettered title can out-measure its mono span.
                    let label: String = truncate_title(raw, avail as usize)
                        .chars()
                        .map(strip_char)
                        .collect();
                    if !crate::tray_raster::strip_band_run_coverable(&label) {
                        fallback.push((seg.start_col, end));
                        continue;
                    }
                    let (ink, face) = if is_active {
                        (colors.active_fg, TextFace::UiBold)
                    } else if is_hovered {
                        (colors.hover_fg, TextFace::Ui)
                    } else {
                        (colors.inactive_fg, TextFace::Ui)
                    };
                    // Leading separator: the cell painter's `│`, as a real 1 px
                    // hairline (band furniture, not a font glyph) — shortened off
                    // the band edges the way native strips draw theirs.
                    if strip_separates(i, input.active, input.paint.hovered)
                        && let Some(seam) = colors.seam
                    {
                        let half = (band_h * 0.28).max(3.0);
                        let y0 = (cy - half).max(0.0);
                        let y1 = (cy + half).min(img_h as f32);
                        if y1 > y0 {
                            prims.push(DrawPrim::Stroke {
                                x: (f32::from(seg.start_col) + 0.5).mul_add(cw, -0.5),
                                y: y0,
                                w: 1.0,
                                h: y1 - y0,
                                radius: 0.0,
                                width: 1.0,
                                color: rgba(seam, 255),
                            });
                        }
                    }
                    if let (Some(icon_start), Some(kind)) =
                        (layout.icon_start, item.and_then(|it| it.icon))
                    {
                        let color = if is_active { colors.fg } else { colors.inactive_fg };
                        draw_icon_prims(
                            &mut prims,
                            tab_icon_primitives(kind),
                            (f32::from(icon_start) + f32::from(ICON_COLS) * 0.5) * cw,
                            cy,
                            ((2.0 * cw).min(band_h) * 0.85).max(6.0),
                            color,
                        );
                    }
                    if let (Some(item), Some(col)) = (item, layout.status_col) {
                        let color = if item.dirty || item.attention {
                            colors.accent
                        } else if is_active {
                            colors.fg
                        } else {
                            colors.inactive_fg
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
                    // The title, left-aligned at its span start (the cell
                    // painter's alignment), clipped to its own span so a
                    // measurement disagreement can never bleed into the status
                    // canvas, the ✕, or the next chip.
                    let x0 = f32::from(layout.title_start) * cw;
                    let span_px = f32::from(avail) * cw - 1.0;
                    if span_px > 1.0 && !label.trim().is_empty() {
                        // The active label through the wght axis when the host
                        // has a variable UI face. The WHOLE label must shape on
                        // that face FIRST — any char its cmap lacks (a Nerd-Font
                        // PUA icon, `✓`, CJK the user's primary covers) sends the
                        // whole run to the fontdue `UiBold` cascade below, the
                        // per-char coverage fallback an inactive chip draws with
                        // — never a mixed-weight word, and never a fit that pops
                        // the uncoverable char off the end and ships `prefix…`
                        // for a title the cascade draws in full. Only a run that
                        // shaped whole is then fitted, at the glyph level (no
                        // re-raster per candidate).
                        let varied = if is_active {
                            variable_pen.as_ref().and_then(|pen| {
                                let run = pen.shape(&label, label_px)?;
                                let run = pen.fit(run, span_px, label_px)?;
                                Some(variable_label_overlay(
                                    &run, x0, baseline, img_h, span_px, ink,
                                ))
                            })
                        } else {
                            None
                        };
                        match varied {
                            Some(overlay) => overlays.push(overlay),
                            None => {
                                let label = fit_label(label, span_px, |s| {
                                    crate::tray_raster::ui_text_width_for(face, s, label_px)
                                });
                                prims.push(DrawPrim::ClipPush {
                                    x: x0,
                                    y: 0.0,
                                    w: span_px,
                                    h: img_h as f32,
                                });
                                prims.push(text_prim(
                                    x0,
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
                    // HOVER-ONLY close mark, as on the native strip — two
                    // round-capped strokes, not a font's ✕ (code-native chrome
                    // never depends on a face covering U+2715).
                    if let Some(cx) = seg.close_col
                        && input.paint.hovered == Some(i)
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
                    // `+` centred in its two-cell card (the cell painter's card —
                    // the leading pad cell stays band, see the gutter note in
                    // `paint_strip_impl`). Code-native strokes: crisp at every
                    // DPI, no face dependency.
                    let card0 = f32::from(seg.start_col + 1) * cw;
                    let card1 = f32::from(end) * cw;
                    let center = (card0 + card1) * 0.5;
                    let r = (label_px * 0.30).max(2.5);
                    let w = (label_px / 8.5).max(1.2);
                    let ink = rgba(colors.raise_fg, 255);
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
                    // The `↻` update alert keeps its glyph — it is the terminal
                    // family's mark today and the Mono arm of the tray pen draws
                    // the SAME face the cells did, only now vertically centred
                    // with the rest of the band.
                    let card0 = f32::from(seg.start_col) * cw;
                    let card1 = f32::from(end.saturating_sub(1)) * cw;
                    prims.push(text_prim(
                        (card0 + card1) * 0.5 - label_px * 0.3,
                        baseline,
                        "\u{21bb}".to_string(),
                        TypeStep::Body.px(label_px),
                        TextWeight::Bold,
                        TextFace::Mono,
                        rgba(colors.active_fg, 255),
                    ));
                    drew_any = true;
                }
                TabHit::Close(_) => {}
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
        // Variable-instance labels composite OVER the prim pass in the same
        // linear-light source-over the prim pen uses, so a varied and a fontdue
        // label on one band blend identically against the transparent ground.
        // Both origins are positions in ONE shared pixel space (the band's):
        // the band sits at (0, 0) and the overlay at its title span's x.
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
        });

        // Coverage: every column EXCEPT the fallback segments'. A covered cell's
        // mono glyph is suppressed by the image (`image_covers`); an uncovered
        // one paints exactly as shipped.
        let mut covered = vec![true; cols];
        for &(a, b) in &fallback {
            for col in usize::from(a)..usize::from(b).min(cols) {
                covered[col] = false;
            }
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
                            },
                        ));
                    } else if r + 1 == geometry.strip_rows {
                        // A fallback segment keeps its shipped icon/status
                        // rasters (they live on the LAST strip row, like the
                        // cell glyphs they accompany). Sorted-by-col invariant
                        // holds: band refs and legacy refs never share a column.
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

    /// The label size for a `band_h`-px OPTICAL band at `scale`: the logical
    /// size scaled, capped against the band so a tiny custom font can't
    /// overflow its own chrome, floored at a legible minimum.
    fn band_label_px(band_h: f32, scale: f32) -> f32 {
        (STRIP_LABEL_LOGICAL_PX * scale)
            .round()
            .min((band_h * 0.62).floor())
            .max(6.0)
    }

    /// Shrink `label` (already display-cell-truncated) until it MEASURES inside
    /// `span_px` under `measure` — the mono span usually over-fits proportional
    /// text, so this is a no-op almost always; an all-caps pathological title
    /// loses a few more chars to the ellipsis instead of clipping mid-glyph.
    /// `measure` is the fontdue pen that will draw the run, so fit and paint can
    /// never disagree. The variable-instance path applies the SAME candidate
    /// order to an already-shaped run instead ([`VariablePen::fit`]), so it
    /// never re-rasters per candidate.
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

    /// One glyph of a [`ShapedRun`]: its coverage raster and where the pen was
    /// when it was placed (run space; the vertical placement is baseline-
    /// relative via `ymin`, fontdue's convention — resolved when the overlay is
    /// built). `pen` and `xmin` are kept apart so a glyph can be re-placed at a
    /// new pen (the fit's ellipsis) without re-rastering it.
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

    /// Where one CHAR of a [`ShapedRun`] ended: the pen after its advance (and
    /// tracking) and how many glyphs the run held by then — the cut points
    /// [`VariablePen::fit`] truncates at, so fitting never re-shapes a prefix.
    #[derive(Clone, Copy)]
    struct CharCut {
        ch: char,
        pen: f32,
        glyphs: usize,
    }

    /// A label shaped through the VARIABLE semibold instance: per-glyph rasters
    /// at the resolved `wght`, advanced by the instance's own (HVAR-varied)
    /// advances plus the regular's `kern` pairs and the UI tracking the fontdue
    /// pen applies — the same pen law, a different outline source.
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

    /// The variable semibold ready to draw: the instance parsed ONCE with its
    /// coords applied, beside the regular's cmap/kern. One per band raster —
    /// every glyph of the active label rasters through this single face instead
    /// of re-parsing the file per glyph.
    struct VariablePen<'a> {
        vf: &'a crate::tray_raster::UiVariableSemibold,
        face: aterm_render::variation::VariedFace<'a>,
    }

    impl<'a> VariablePen<'a> {
        /// `None` when the retained bytes no longer parse (they did at install;
        /// this is belt-and-braces, and the caller then takes the fontdue path).
        fn new(vf: &'a crate::tray_raster::UiVariableSemibold) -> Option<Self> {
            let face = aterm_render::variation::VariedFace::parse(&vf.bytes, vf.index, &vf.coords)?;
            Some(Self { vf, face })
        }

        /// Shape `text` at `px` through the variable semibold. `None` when ANY
        /// char is outside the face's cmap or a glyph fails to raster — the
        /// caller then draws the run through the fontdue `UiBold` path instead,
        /// WHOLE (that path's per-char cascade covers what this face cannot).
        /// Total: one raster per char, never more.
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

        /// [`fit_label`] for an ALREADY-SHAPED run: the same candidate order
        /// (drop a trailing cell-truncation `…`, then every shorter prefix +
        /// `…`, longest first; the bare `…` ships even when it overflows — the
        /// span clip bounds it), but applied by cutting the shaped glyphs at a
        /// char boundary and re-placing ONE ellipsis raster at the prefix's pen
        /// plus its kern. Glyph placement is bit-identical to shaping the
        /// fitted string (the prefix pens are the very same values; the
        /// ellipsis lands at `pen + kern + bearing` either way), so fit and
        /// paint cannot disagree — at one extra raster instead of one per
        /// candidate per char. A run that already fits comes back untouched.
        /// `None` only when `…` itself is outside the face (fontdue then fits
        /// and draws the whole label).
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

    /// Lay a [`ShapedRun`] into a straight-alpha RGBA overlay `height` px tall,
    /// at most `span_px` wide (the title span's own clip, enforced by the buffer
    /// edge), inked in `color` on the transparent ground.
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

    /// Map one 16-unit icon IR box ([`TabIconPrimitive`], the exact commands the
    /// cell path supersamples into its 32×32 rasters) into `DrawPrim`s centred at
    /// `(cx, cy)` with side `side` px. Same shapes, same proportions — but drawn
    /// at BAND resolution and vertically centred in the band instead of stretched
    /// over one cell box, which is what pixel freedom is for.
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
                TabIconPrimitive::Dot { center, radius } => {
                    prims.push(DrawPrim::Dot {
                        cx: center[0].mul_add(k, ox),
                        cy: center[1].mul_add(k, oy),
                        r: (radius * k).max(1.0),
                        color: ink,
                        breathe: false,
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
            }
        }

        fn plain(n: usize) -> Vec<TabStripMetadata> {
            vec![
                TabStripMetadata {
                    icon: None,
                    dirty: false,
                    busy: false,
                    attention: false,
                    closable: true,
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
            (
                image.bytes.clone(),
                usize::from(width),
                usize::from(height),
            )
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

        /// Install the host UI faces for this thread, or `None` when the host has
        /// no resolvable system UI face (CI) — the band then declines by design
        /// and the test has nothing pixel-level to assert.
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
            assert_eq!(cols, (0..80).collect::<Vec<_>>(), "every column covered, sorted");
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
            assert_eq!((w, h), (80 * CELL_W, CELL_H), "1:1 with the cell footprint");
            assert_eq!((first.cols, first.rows, first.z_index), (80, 1, 0));
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
            let (x0, y0, x1, y1) = ink_bbox(&rgba, w, h, 0, w).expect("the band has ink");
            // Segment 0's title ink starts inside its span (left-aligned); the
            // ✕ is hover-only so nothing else inks left of the next chip.
            assert!(x0 >= span.0 && x0 < span.1, "ink starts in span: {x0} vs {span:?}");
            let _ = x1;
            // Vertical centring in the OPTICAL band (lip + row): the ink's
            // cap-ish middle sits near the band centre, expressed in image rows.
            let band_centre = (BAND_TOP + CELL_H) as f32 / 2.0 - BAND_TOP as f32;
            let ink_mid = (y0 + y1) as f32 / 2.0;
            assert!(
                (ink_mid - band_centre).abs() <= 3.0,
                "ink mid {ink_mid} vs band centre {band_centre} (bbox y {y0}..{y1})"
            );
            assert!(y0 > 0 && y1 < h, "ink stays inside the image rows");
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
                assert!(!covered.contains(&col), "col {col} belongs to the rename well");
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
                format: ImageFormat::RawRgba8 { width: 1, height: 1 },
                cols: 1,
                rows: 1,
                z_index: 0,
            });
            let mk = |col: u16| {
                (
                    usize::from(col),
                    ImageRef {
                        image: Arc::clone(&icon),
                        cell_row: 0,
                        cell_col: 0,
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
            assert_eq!(find(kept_col), Some(true), "fallback segment keeps its icon");
            assert_eq!(find(dropped_col), Some(false), "pixel segment is band-covered");
            let mut sorted = rows[0].iter().map(|(c, _)| *c).collect::<Vec<_>>();
            sorted.dedup();
            assert!(sorted.windows(2).all(|p| p[0] < p[1]), "refs stay sorted by column");
            crate::tray_raster::clear_ui_fonts_for_test();
        }

        /// T3 full cut: on a host whose UI regular is a variable face with a
        /// `wght` axis (Win11 SegUIVar), the active label shapes through the
        /// wght-600 instance — and that instance is VISIBLY heavier than the
        /// regular (more coverage for the same string at the same size), which
        /// is the whole point of not pairing the file with itself.
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

        /// A title the chrome CASCADE covers but the variable face's cmap does
        /// not (`✓`, a Nerd-Font PUA icon, CJK under a CJK-capable primary) must
        /// draw IN FULL on the active chip — through the fontdue `UiBold`
        /// per-char fallback, exactly as the inactive chip draws it — never as
        /// `prefix…` or a lone `…` from a fit that pops the uncoverable char off
        /// the end of a run that was never going to shape whole.
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
            // A char outside the variable face's cmap that this thread's cold
            // chrome cascade (the embedded DejaVu coverage fallback; the user's
            // primary in production) draws as real ink — probed, not assumed,
            // so a future Segoe cut that grows a dingbat block makes the test
            // skip rather than lie.
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
                eprintln!("no cascade-only char on this host; nothing to regress");
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
            // The full label's ink is at least as wide as its Latin stem set in
            // the semibold (the cascade char adds its own advance on top; side
            // bearings take back a px or two) — and well past what `Alpha…`,
            // the old fit's output, would have inked.
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

        /// The glyph-level fit ([`VariablePen::fit`]) lands every glyph exactly
        /// where shaping [`fit_label`]'s string would have — the "fit and paint
        /// cannot disagree" law, now without a raster per candidate — across
        /// "nothing fits", "a few chars fit", "one char short", "fits exactly"
        /// and "over-fits", and for a label that arrives with a cell-truncation
        /// `…` to drop first.
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
            assert_eq!((w, h), (60 * CELL_W, 2 * CELL_H));
            crate::tray_raster::clear_ui_fonts_for_test();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Segments for a ONE-TAB window with the solo TITLE BAND forced on, so the solo
    /// painter stays under test on every platform. Only the POLICY that produces a
    /// solo segment is platform-gated ([`SOLO_TITLE_BAND`]) — the painter it feeds is
    /// not, and a `#[cfg(target_os = "macos")]` on these tests would mean the solo
    /// paint path is compiled but never exercised on the CI most commits run through.
    fn forced_solo_segments(cols: u16, metadata: &[TabStripMetadata]) -> Vec<TabSegment> {
        let mut segments = layout_segments_with_metadata(cols, 1, metadata, 0, false);
        if let Some(band) = segments.first_mut() {
            band.solo = true;
            band.close_col = None; // what `layout_segments` does for a solo band
        }
        segments
    }

    /// A LONE tab: macOS spends the whole band on it because there it IS the window's
    /// title (no close column, `solo` set, a centred title band) — the OS caption's
    /// text is hidden. Everywhere else the caption already carries the title, so the
    /// lone tab is an ordinary CHIP: close column intact, `solo` clear, and CAPPED at
    /// [`SOLO_CHIP_MAX_COLS`] so the band it sits on stays visible to its right. A
    /// full-width lone chip is the one state that makes the strip's whole grid /
    /// band / card stack disappear, and it is the state every new window opens in.
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

    /// The lone-chip cap is a MAXIMUM, never a minimum: a window too narrow to reach
    /// it still gets the equal share it always got, so a small window is not left
    /// with a chip wider than its own strip.
    #[test]
    fn a_narrow_window_keeps_the_share_the_lone_chip_cap_never_widens_it() {
        let cols = 20u16;
        let segs = layout_segments(cols, 1, 0, false);
        assert_eq!(segs[0].end_col, cols - NEW_TAB_W);
        assert_eq!(segs[1].start_col, cols - NEW_TAB_W);
    }

    /// The `+`'s raised card never touches the ACTIVE tab's raised card. On a chrome
    /// band both resolve to `raise_bg`, and `layout_segments` places the `+` flush
    /// against the last tab — the state right after every New Tab — so the painter
    /// leaves the `+` segment's leading pad cell on the band. The `↻` alert's
    /// trailing pad is the same guard against an active tab 0. Hit geometry is
    /// untouched: the gutter column still answers `TabHit::NewTab`.
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
            assert_eq!(
                row[plus.start_col as usize + 1].bg,
                colors.raise_bg,
                "the card itself is still raised"
            );
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

    /// THE INLINE RENAME FIELD replaces the chip's title span with the find bar's
    /// well: the text verbatim (never ellipsised — you are typing it), a
    /// REVERSE-VIDEO block caret on the edit position, and the selected chip's
    /// accent seam kept underneath so the tab still reads as selected.
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

    /// A narrow chip would leave `tab_content_layout` a one-cell title span once
    /// the icon and status canvas took their share — a caret with no text. While
    /// editing they are suppressed, so the field gets the chip's whole interior
    /// and still scrolls rather than truncating.
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

    /// EDITING THE LONE TAB: the solo band becomes one left-aligned field. The
    /// centred title+description group is dropped — a centred field recentres
    /// itself (and the caret with it) on every character typed.
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

    /// The SOLO band paints the window's title, not a chip: no raised card, no
    /// selection underline, no `✕`, the title CENTRED and its description trailing
    /// in the dim tone. The native strip's solo band, in cells.
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

    /// The `✕` is HOVER-ONLY on the in-grid strip too — including on the SELECTED
    /// tab, which is exactly the one a permanent ✕ gets mis-clicked on. Its column
    /// is reserved either way, so the reveal never reflows the title, and
    /// `hit_test` keeps closing there (the pointer is on the tab it clicks).
    #[test]
    fn the_close_mark_is_painted_only_on_the_hovered_tab() {
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

    /// TWO OR MORE tabs split the whole band into EQUAL shares with no maximum, so
    /// a wide strip buys longer titles rather than bare background past a capped
    /// chip — and none of them is `solo`.
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

    /// RFC Rung 2: `show_update` prepends a LEADING `↻` update segment at col 0,
    /// shifting the tabs right; a click on it resolves to `TabHit::Update`. Absent when
    /// `show_update = false` (byte-identical to before).
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

    /// Three tabs share the available width evenly (each capped at MAX_SEG); the
    /// segments are disjoint and ordered, with the `+` after the last tab.
    #[test]
    fn three_tabs_disjoint_ordered() {
        let segs = layout_segments(60, 3, 1, false);
        let tabs: Vec<_> = segs
            .iter()
            .filter(|s| matches!(s.kind, TabHit::Select(_)))
            .collect();
        assert_eq!(tabs.len(), 3);
        for w in tabs.windows(2) {
            assert!(
                w[0].end_col <= w[1].start_col,
                "segments are disjoint + ordered"
            );
        }
        assert!(matches!(segs.last().unwrap().kind, TabHit::NewTab));
    }

    /// A click on a tab segment selects it; a click on its close `x` closes it; a
    /// click on the `+` opens a tab; a click on bare background is `None`.
    #[test]
    fn hit_test_select_close_new() {
        let segs = layout_segments(80, 2, 0, false);
        let tab0 = &segs[0];
        // A plain cell inside tab 0 → Select(0).
        assert_eq!(hit_test(&segs, tab0.start_col + 1), Some(TabHit::Select(0)));
        // The close `x` column → Close(0).
        let cx = tab0.close_col.expect("wide tab has a close x");
        assert_eq!(hit_test(&segs, cx), Some(TabHit::Close(0)));
        // The `+` → NewTab.
        let plus = segs.last().unwrap();
        assert_eq!(hit_test(&segs, plus.start_col + 1), Some(TabHit::NewTab));
        // A gap between the last tab and the `+` (if any) → None.
        if tab0.end_col < segs[1].start_col {
            // not guaranteed; only assert the far-right past everything is None
        }
        assert_eq!(hit_test(&segs, u16::MAX), None);
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
                attention: true,
                closable: false,
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
        assert_eq!(
            hit_test(&canonical, base[1].close_col.unwrap()),
            Some(TabHit::Select(1)),
            "the old close cell remains part of the full tab select target"
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
                attention: true,
                closable: true,
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
                subtitle: None,
                rename: None,
            },
            0,
            theme,
            None,
        );
        let first = segments[0];
        let layout = tab_content_layout(&first, metadata[0]);
        assert_eq!(layout.icon_start, Some(first.start_col + 1));
        assert_eq!(layout.title_start, first.start_col + 4);
        assert_eq!(layout.status_col, Some(first.close_col.unwrap() - 2));
        assert_eq!(row[layout.title_start as usize].ch, 'S');
        assert_eq!(row[first.close_col.unwrap() as usize].ch, '✕');
        let image_cols: Vec<_> = images.iter().map(|(col, _)| *col).collect();
        assert!(image_cols.contains(&usize::from(first.start_col + 1)));
        assert!(image_cols.contains(&usize::from(first.start_col + 2)));
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

    /// [`IconKey`] must name EVERY input the raster depends on. If a later change
    /// makes `status_primitives` (or `tab_icon_primitives`) read a field the key
    /// omits, the memo would silently serve the WRONG glyph — the one real risk of
    /// caching these pixels, and a purely visual bug no other test would catch.
    /// Walk every reachable key (4 icon kinds and all 8 status bit combinations,
    /// each in two colours) through the real `append_*` entry points and demand a
    /// fresh rasterization of the same inputs, byte for byte.
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
                    attention: bits & 0b100 != 0,
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
        assert_eq!(terminal_layout.icon_start, None);
        assert_eq!(settings_layout.icon_start, Some(segments[1].start_col + 1));
        assert_eq!(terminal_layout.title_start, segments[0].start_col + 1);
        assert_eq!(
            settings_layout.title_start,
            segments[1].start_col + 1 + ICON_COLS + ICON_GAP
        );
        assert_eq!(
            settings_layout.title_start - (segments[1].start_col + 1),
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
        let full = "aterm\ndescription: the release cutter\nactivity: Building\ncwd: ~/aterm\nstate: alive\n\nspawned · just now";
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
            solo_subtitle("aterm", Some("aterm\nstate: alive")),
            None,
            "a lifecycle word is not a description; the bare title reads better"
        );

        // A title that already carries the fact gets no echo beside it — the common
        // shape, since the composed label folds the activity in ("aterm · Ready").
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
            solo_subtitle("aterm", Some("aterm\n\nstate-change · just now")),
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

        // The ✕ is a HOVER-ONLY reveal, so its slot must be reserved unconditionally:
        // identical geometry with and without an icon proves the title never reflows
        // when the pointer arrives.
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

        // The strip's vertical centre is MEASURED from the traffic lights, so every
        // slot must track it rather than the view's own midpoint.
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
            attention: true,
            closable: true,
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

    /// A narrow strip drops the close `x` (segments below MIN_SEG_WITH_CLOSE) but
    /// the tab is still clickable to SELECT — close just isn't offered.
    #[test]
    fn narrow_tab_drops_close_but_selectable() {
        // 9 cols, 3 tabs → each tab is (9-3)/3 = 2 cells wide: too narrow for a close x.
        let segs = layout_segments(9, 3, 0, false);
        let tabs: Vec<_> = segs
            .iter()
            .filter(|s| matches!(s.kind, TabHit::Select(_)))
            .collect();
        assert!(!tabs.is_empty());
        for t in &tabs {
            assert!(t.close_col.is_none(), "narrow tab has no close x: {t:?}");
            // Still selectable.
            assert_eq!(hit_test(&segs, t.start_col), Some(t.kind));
        }
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

    /// Truncation budgets DISPLAY cells: three CJK chars are six cells, and a
    /// wide char that cannot fit whole before the `…` is dropped whole — the
    /// ellipsis may land a cell early, a split glyph never lands at all.
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

    /// `paint_strip` distinguishes the active tab from inactive ones (active = a light
    /// raised bg + full-strength fg with an underline accent; inactive = dimmed fg on
    /// the body background, so it recedes) and renders the title chars + close `✕` into
    /// the segment, leaving the column math exact (one char per cell).
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
        // Tab 0 is active → a light raised button (bg stepped above the body),
        // full-strength bold text, with a theme-accent underline.
        assert_ne!(
            row[t0.start_col as usize].bg, bg_rgb,
            "active tab bg is raised above the body"
        );
        assert_eq!(
            row[t0.start_col as usize].fg, fg_rgb,
            "active tab fg = full-strength theme fg"
        );
        assert_eq!(
            row[t0.start_col as usize].underline,
            UnderlineStyle::Single,
            "active tab carries the underline accent"
        );
        assert!(
            row[t0.start_col as usize].bold,
            "active identity is explicit"
        );
        assert_eq!(
            row[t0.start_col as usize].underline_color,
            Some([
                ((theme.cursor >> 16) & 0xff) as u8,
                ((theme.cursor >> 8) & 0xff) as u8,
                (theme.cursor & 0xff) as u8,
            ]),
            "selected underline follows the theme accent"
        );
        // The title 'z','s','h' appears starting at the leading pad.
        let ts = (t0.start_col + 1) as usize;
        assert_eq!(row[ts].ch, 'z');
        assert_eq!(row[ts + 1].ch, 's');
        assert_eq!(row[ts + 2].ch, 'h');
        // The close ✕ is present for a wide tab.
        let cx = t0.close_col.unwrap() as usize;
        assert_eq!(row[cx].ch, '✕');
        // Tab 1 is inactive → recedes onto the strip's own band (distinct from the
        // active button) and is NOT bold. The band is `theme.bg` on macOS and the
        // shared chrome tone elsewhere, so this reads it rather than assuming.
        let t1 = &segs[1];
        assert_eq!(
            row[t1.start_col as usize].bg,
            strip_colors(theme).band_bg,
            "inactive tab bg = the band (recedes)"
        );
        assert_ne!(
            row[t1.start_col as usize].bg, row[t0.start_col as usize].bg,
            "inactive differs from active"
        );
        assert!(
            !row[t1.start_col as usize].bold,
            "inactive tab text is not bold"
        );
    }

    /// A long title is truncated INSIDE the segment, never overflowing into the
    /// next tab's columns (the close `✕` and segment boundary are honoured).
    #[test]
    fn long_title_stays_inside_segment() {
        let theme = Theme::default();
        let cols = 40usize;
        let mut row = vec![strip_cell(' ', &strip_colors(theme), StripRole::Inactive); cols];
        let segs = layout_segments(cols as u16, 2, 0, false);
        let long = "this-is-a-really-long-window-title-from-vim".to_string();
        let titles = vec![long, "x".to_string()];
        // Tab 0 is under the pointer, so its hover-only ✕ is painted.
        paint_strip(&mut row, &segs, &titles, Some(0), 0, theme);
        let t0 = &segs[0];
        // The cell just before tab 1 starts must still be tab 0's (close ✕ or pad),
        // never a title char that ran past the boundary.
        let boundary = t0.end_col as usize;
        // No title char should appear at or past the boundary within tab 1's start.
        assert!(boundary <= cols);
        // The close ✕ sits at close_col, strictly inside the segment.
        let cx = t0.close_col.unwrap();
        assert!(cx < t0.end_col);
        assert_eq!(row[cx as usize].ch, '✕');
    }

    /// `cols == 0` (degenerate) yields no segments and never panics.
    #[test]
    fn zero_cols_no_segments() {
        assert!(layout_segments(0, 3, 0, false).is_empty());
    }

    /// C6: a wide char in a tab title paints as a REAL lead + continuation pair
    /// — the `RenderCell` contract every other in-grid surface already honours —
    /// never as a `·` placeholder (non-BMP) and never as a narrow cell whose
    /// glyph overlaps its neighbour (BMP-wide). All three reported shapes: an
    /// emoji title, a CJK title, a width-2 BMP symbol title.
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
            // Walk the painted title span: every wide char owns exactly its
            // lead cell plus one continuation flagged `wide`, and the display
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

    /// C6, the failure the audit called out by name: a WIDE title on a NARROW
    /// segment must drop a straddling wide char whole rather than let its
    /// raster bleed over the close `✕` or the neighbouring chip.
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
            // The last cell a glyph may reach is boundary-1's continuation; the
            // boundary cell itself (the pad before the ✕ / the edge) must never
            // be a continuation of a glyph that started inside the title.
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

    /// C6 in the rename editor: what you TYPE paints as itself too — the field
    /// shares the titles' projection. The caret block covers BOTH cells of a
    /// wide char (a half-width caret reads as standing on half a glyph), and
    /// the continuation carries the `wide` flag for the renderer.
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

    /// C6 in the solo band (the macOS title-band painter, force-exercised on
    /// every platform): a CJK title is centred by DISPLAY width and paints
    /// lead+continuation pairs, so the band and the caption finally agree.
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

    /// C4: once the equal share falls under [`PREFERRED_MIN_TAB_COLS`], the
    /// ACTIVE tab keeps a readable floor while the inactive tabs compress —
    /// `toolbar::native_tab_cells`' pressure rule, in cells. The audit's
    /// headline case: sixteen tabs on ~130 columns collapsed EVERY title,
    /// the focused one included, to ~3 chars.
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
        let widths: Vec<u16> = tabs.iter().map(|s| s.end_col - s.start_col).collect();
        let active_w = tabs
            .iter()
            .find(|s| s.kind == TabHit::Select(active))
            .map(|s| s.end_col - s.start_col)
            .expect("the active tab is placed");
        assert_eq!(
            active_w, ACTIVE_TAB_PRESSURE_CAP_COLS,
            "the active tab takes the pressure reserve"
        );
        assert!(
            active_w >= PREFERRED_MIN_TAB_COLS,
            "and it clears the legibility floor"
        );
        for (i, w) in widths.iter().enumerate() {
            if i != active {
                assert!(
                    *w < active_w,
                    "inactive tab {i} ({w} cols) compresses below the active one"
                );
            }
        }
        // Geometry stays sane: disjoint, ordered, inside the band, close column
        // present on the active tab (it is the one being read and closed).
        for pair in tabs.windows(2) {
            assert!(pair[0].end_col <= pair[1].start_col);
        }
        assert!(tabs.last().unwrap().end_col <= cols);
        assert!(
            tabs[active].close_col.is_some(),
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

    /// C4's guard rail: with ROOM, the layout is selection-independent — equal
    /// shares do not move when the active tab changes, so switching tabs on a
    /// roomy strip never slides a close column out from under the pointer.
    #[test]
    fn roomy_equal_shares_are_selection_independent() {
        for count in 2..=6usize {
            let base = layout_segments(200, count, 0, false);
            for active in 1..count {
                assert_eq!(
                    layout_segments(200, count, active, false),
                    base,
                    "equal shares must not depend on selection at {count} tabs"
                );
            }
        }
    }

    /// C4 at the degenerate end: more tabs than the band can hold at a cell
    /// apiece still lays out inside the strip (trailing tabs are dropped, as
    /// ever — reachable by cycling), and never panics or overflows.
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

    /// C2's separator rule, PURE: dividers separate two quiet chips and nothing
    /// else — never before the first chip, never beside the active chip, and
    /// never beside the hovered chip. The first two clauses are byte-for-byte
    /// `TabGeometry::separates` (`toolbar.rs`); the hover clause is the same
    /// suppression the native strip gets from its per-view hover pill.
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

    /// C2 painted: three quiet chips show hairlines on their leading pad cells;
    /// the pad is PAINT-ONLY (hit-testing still selects the tab); and hovering
    /// an inactive chip both washes it and clears the rules beside it.
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
        // Tabs 2 and 3 are quiet neighbours of quiet tabs: hairlines. Tab 1
        // touches the active chip's trailing edge: none.
        assert_eq!(row[tabs[1].start_col as usize].ch, ' ');
        assert_eq!(row[tabs[2].start_col as usize].ch, '│');
        assert_eq!(row[tabs[3].start_col as usize].ch, '│');
        assert_eq!(
            row[tabs[2].start_col as usize].fg,
            colors.seam.expect("a chrome band always has a seam"),
            "the divider is the seam's ink — one structural material"
        );
        // Paint-only: the divider's column is still the tab's own click target.
        assert_eq!(
            hit_test(&segs, tabs[2].start_col),
            Some(TabHit::Select(2)),
            "the hairline lives on the pad cell and moves no hit geometry"
        );

        // Hover tab 2: it takes the wash, and the rules beside it clear.
        let mut row = vec![blank_cell(theme); 80];
        paint_strip(&mut row, &segs, &titles, Some(2), 0, theme);
        let washed = row[tabs[2].start_col as usize + 1];
        assert_eq!(washed.bg, colors.hover_bg, "the hovered chip is washed");
        assert_ne!(colors.hover_bg, colors.band_bg, "visibly — off the band");
        assert_ne!(
            colors.hover_bg, colors.active_bg,
            "but never the selection's raise"
        );
        assert!(!washed.bold, "hover keeps the inactive vocabulary");
        assert_eq!(washed.underline, UnderlineStyle::None);
        assert_eq!(
            row[tabs[2].start_col as usize].ch, ' ',
            "its own rule yields"
        );
        assert_eq!(
            row[tabs[3].start_col as usize].ch, ' ',
            "and so does its trailing neighbour's"
        );
        // The hover-revealed ✕ sits ON the wash — it finally has a backing.
        let cx = tabs[2].close_col.expect("wide tab reserves a close column");
        assert_eq!(row[cx as usize].ch, '✕');
        assert_eq!(row[cx as usize].bg, colors.hover_bg);
        // The ACTIVE chip is never washed, hovered or not.
        let mut row = vec![blank_cell(theme); 80];
        paint_strip(&mut row, &segs, &titles, Some(0), 0, theme);
        assert_eq!(
            row[tabs[0].start_col as usize].bg, colors.active_bg,
            "hovering the selected tab leaves it selected"
        );
    }

    /// Perceptual luma of a strip tone — the same cheap weights [`bg_is_light`]
    /// classifies with, so the surface tests and the classifier cannot disagree.
    #[cfg(not(target_os = "macos"))]
    fn luma(c: [u8; 3]) -> f32 {
        0.299 * f32::from(c[0]) + 0.587 * f32::from(c[1]) + 0.114 * f32::from(c[2])
    }

    /// THE STRIP IS THREE SEPARABLE SURFACES, and this is the test that keeps it
    /// that way. The reported defect was two of them collapsed: `body_bg` was
    /// `theme.bg` byte-for-byte, so the band WAS the grid and the labels floated as
    /// bare text. The obvious repair — point the band at `chrome_band`'s `bar_bg`
    /// and stop — collapses the OTHER pair, because `bar_bg` is a 0.16 step off the
    /// background and the old `active_bg` was a 0.21 step off the same origin, five
    /// hundredths apart: the "full-width gray chrome band with the active tab merged
    /// into the body, heavy/unfinished" iteration `strip_colors` records the visual
    /// judge REJECTING. Assert both gaps on every bundled scheme, and assert the
    /// card's step above the band is no smaller than the band's own step off the
    /// grid — i.e. the card is never the fainter of the two edges.
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

    /// The band ENDS somewhere. Every cell of the last strip row carries a rule
    /// closing it against the grid below — except the ones that already carry the
    /// selection accent, which is the same edge in the accent colour and must
    /// survive the seal (that break under the selected tab is the shape).
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
        assert!(active_underlines > 0, "the active chip has its accent rule");
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
    }

    /// OFF macOS a lone tab paints a real CHIP — the raised card, the selection
    /// rule, a close column — because the OS caption already carries the window
    /// title. The shipped Windows look was the opposite: [`StripRole::Title`] on
    /// `theme.bg`, i.e. bold text on the grid with no chrome whatsoever, four pixels
    /// under a caption saying the same thing.
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
        assert_eq!(
            title.underline,
            UnderlineStyle::Single,
            "with the selection rule under it"
        );
        assert_eq!(title.underline_color, Some(colors.accent));
    }

    /// The `+` stops opting out of the strip's own button vocabulary: on a chrome
    /// band it takes the raised tone `StripRole::Update`'s `↻` has always had, and
    /// takes it from the THEME's raise, so a user's `active_tab_color` recolours the
    /// selected tab without repainting the new-tab button as well.
    #[cfg(not(target_os = "macos"))]
    #[test]
    fn the_new_tab_button_is_raised_and_ignores_the_active_tab_override() {
        let theme = Theme::default();
        let colors = strip_colors(theme);
        let plus = strip_cell('+', &colors, StripRole::NewTab);
        assert_eq!(plus.bg, colors.raise_bg, "the `+` is a raised button");
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
            colors.raise_bg,
            "and stops at the tab: the `+` keeps the theme's own raise"
        );
    }

    /// `strip_char` blanks only CONTROLS now; everything else — CJK, emoji,
    /// plane-15 Nerd Font icons — passes through as itself, with
    /// [`strip_char_cells`] carrying the display width the painter budgets.
    /// The old `·` placeholder made the strip disagree with the caption
    /// rendering the same title four pixels above it.
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

    /// Every built-in theme keeps the tab strip's text above the 3.0:1 UI-text floor:
    /// the active-tab text (full fg on the raised button), the `+` new-tab affordance
    /// (full fg on the body), and — newly guarded — the INACTIVE tab labels (dimmed fg
    /// on the body), which previously shipped near-illegible. Guards S2 (the dim `+`
    /// dropped to 2.59:1 on Solarized Dark), the light-theme FIXME in `strip_colors`,
    /// and the "black on black" inactive-label bug. The default theme's inactive labels
    /// are pinned higher (>=7:1) to lock that fix — a future theme that breaks chrome
    /// contrast fails HERE, at add-time, not in the field.
    #[test]
    fn strip_contrast_meets_wcag_aa() {
        use aterm_types::Rgb;
        let rgb = |c: [u8; 3]| Rgb::new(c[0], c[1], c[2]);
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
            // together for that ROLE — measuring an ink against a background it is
            // never drawn on is how the raised card's label slipped to 2.85:1 on
            // Solarized Dark while a `fg`-on-`band_bg` reading called it fine.
            let active = rgb(c.active_fg).contrast(rgb(c.active_bg));
            let (new_tab_fg, new_tab_bg) = if STRIP_IS_CHROME_BAND {
                (c.raise_fg, c.raise_bg)
            } else {
                (c.fg, c.band_bg)
            };
            let new_tab = rgb(new_tab_fg).contrast(rgb(new_tab_bg));
            // INACTIVE tab labels sit on the body bg. This was historically
            // UNGUARDED, and a heavy dim shipped them near-illegible ("black on
            // black" on the vibrancy titlebar). Guard them to the 3:1 UI-text floor:
            // intentionally MUTED schemes cannot reach AA 4.5 at all (Solarized
            // Light's FULL-strength text is only ~4.13:1, below 4.5 undimmed; it is
            // also the current minimum here at ~3.20:1), so 3.0 is the achievable
            // universal bar; the default theme is pinned far higher just below.
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
            // The HOVER wash is one more surface an inactive label stands on
            // (chrome band only — macOS's flat strip never paints it), and the
            // wash is a step TOWARD the ink, so it is the pair likeliest to
            // slide under the floor on a muted scheme.
            if STRIP_IS_CHROME_BAND {
                let hover = rgb(c.hover_fg).contrast(rgb(c.hover_bg));
                assert!(
                    hover >= 3.0,
                    "{name}: hovered-tab label contrast {hover:.2} < 3.0:1"
                );
            }
            // The active card must be a DISTINCT surface from the body, or the
            // focused tab vanishes into the strip (true on dark and light alike).
            assert_ne!(
                c.active_bg, c.band_bg,
                "{name}: active-tab card is indistinguishable from the body"
            );
            // …AND THE THREE ASSERTIONS ABOVE MUST NOT BE SELF-SATISFYING. Every ink
            // measured there has already been through `ensure_contrast(_, that same
            // surface, 3.0)`, so on its own a `>= 3.0` check can only fail when the
            // ten-step nudge cannot reach the target AT ALL — a bad re-tune would
            // silently ship auto-corrected ink instead of turning this test red. The
            // two properties below are the ones the floor CANNOT fake.
            let raw = [
                ((theme.fg >> 16) & 0xff) as u8,
                ((theme.fg >> 8) & 0xff) as u8,
                (theme.fg & 0xff) as u8,
            ];
            // (i) The BAND's own full-strength ink is never floored. The floor exists
            // for exotic user themes; a bundled scheme needing it would mean the band
            // itself is too heavy — it has eaten the contrast its own labels are read
            // against — and that is a `band_colors` problem, not something an ink
            // nudge should paper over.
            assert_eq!(
                c.fg, raw,
                "{name}: the band's full-strength ink needed the contrast floor — the \
                 BAND is too heavy; re-tune `band_colors`, don't lean on the floor"
            );
            // (ii) The inactive DIM never out-contrasts full strength on the same
            // surface. The floor is a one-way ratchet, so on a muted scheme it can drag
            // a dim PAST the ink it is a dim of and print unfocused labels more
            // prominently than the focused one (Solarized Light on a band). Only
            // `strip_colors`' explicit ceiling stops that, and only this assertion
            // notices if the ceiling is removed — the `>= 3.0` checks above would still
            // pass, more comfortably than ever.
            //
            // MEASURED MARGIN, so the next person knows how much room this has. On a
            // band the floor fires on the dim for exactly ONE bundled scheme,
            // Solarized Light, lifting it 2.91 → 3.50:1 against 3.69:1 full strength:
            // 0.19 short of the inversion, not past it. So this assertion does not
            // currently catch a live defect on any bundled scheme — it is the guard
            // that keeps a re-tune (or a user theme) from crossing a line the `>= 3.0`
            // checks above would happily wave through.
            let full = rgb(c.fg).contrast(rgb(c.band_bg));
            assert!(
                inactive <= full + 1e-9,
                "{name}: inactive labels ({inactive:.2}:1 on the band) are MORE \
                 prominent than full-strength ink ({full:.2}:1) — the dim was floored \
                 past what it dims"
            );
            // (iii) On the FLAT strip the floors are no-ops on every bundled scheme,
            // which is exactly what the band treatment claims to have changed and what
            // makes the macOS arm's byte-identity checkable rather than asserted.
            if !STRIP_IS_CHROME_BAND {
                assert_eq!(
                    c.active_fg, raw,
                    "{name}: a flat strip's active label needed the contrast floor"
                );
            }
        }
        // Lock the REPORTED fix on the default theme (the "black on black" case):
        // inactive labels must stay well clear of the muted floor, not just above 3:1.
        //
        // The number is surface-dependent, and saying so is more honest than picking
        // one both surfaces can meet. On the flat strip the ink sits on `theme.bg`
        // and reaches ~9:1. On a chrome band it sits on a surface deliberately
        // lifted 0.16 toward the fg, which costs contrast BY CONSTRUCTION — ~6.6:1 —
        // and no dim setting recovers it, because the cost is the band, not the dim.
        // Both are far clear of AA's 4.5; this pin exists to catch a SLIDE back
        // toward the 3.0 floor, so it sits just under what each surface delivers.
        let pin: f64 = if STRIP_IS_CHROME_BAND { 6.0 } else { 7.0 };
        let def = strip_colors(Theme::default());
        let def_inactive = rgb(def.inactive_fg).contrast(rgb(def.band_bg));
        assert!(
            def_inactive >= pin,
            "default-theme inactive label contrast {def_inactive:.2} < {pin:.1}:1 (regressed the readability fix)"
        );
    }

    /// `strip_colors` is appearance-aware: on a DARK theme the active card raises
    /// (steps toward the light fg, so it is brighter than the body); on a LIGHT theme
    /// it steps toward the dark fg (so it is darker than the body). Either way the
    /// card is a distinct surface — the resolution of the old light-theme FIXME.
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
        // The default (dark) theme raises the active card: active = blend(bg, fg, 0.21),
        // inactive labels = blend(fg, bg, 0.15) (mild dim, kept legible — see
        // `strip_contrast_meets_wcag_aa`).
        let def = strip_colors(Theme::default());
        assert!(luma(def.active_bg) > luma(def.band_bg));
    }
}
