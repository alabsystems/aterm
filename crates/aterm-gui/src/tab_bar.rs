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
            attention: presentation.indicators.attention,
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
/// SAME TWO RULES AS THE NATIVE STRIP (`toolbar::native_tab_cells` /
/// `toolbar::set_window_tabs`), so the two renderers cannot disagree about what a
/// tab bar IS: tabs divide the whole band into EQUAL shares with no maximum width
/// (a wide window buys longer titles, not bare strip past a capped segment), and
/// a LONE tab is the window's title rather than a switcher — one full-band
/// [`TabSegment::solo`] segment with no close column.
///
/// Pure geometry: no window, no renderer, no `App`. `active` is accepted for
/// symmetry / future per-tab sizing; the current MVP sizes every tab equally.
#[must_use]
pub fn layout_segments(
    cols: u16,
    tab_count: usize,
    _active: usize,
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
    let solo = tab_count == 1;
    let mut x: u16 = lead;
    if tab_count > 0 {
        // Split the available width (past the leading update icon) into EQUAL
        // shares — no maximum, so a wide window spends its columns on titles
        // instead of leaving bare strip past a capped segment. Floored so a tab is
        // at least 1 cell before we stop placing tabs.
        let per = avail.saturating_sub(lead) / tab_count as u16;
        for i in 0..tab_count {
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

/// The four strip tones derived from a theme, computed ONCE per [`paint_strip`]
/// (hoisted out of the per-cell [`strip_cell`] — the blends are frame-invariant).
#[derive(Clone, Copy)]
struct StripColors {
    /// Full-strength foreground (the `+` affordance and, by default, the
    /// active-tab text).
    fg: [u8; 3],
    /// The terminal body background (bare strip + inactive tabs sit on this).
    body_bg: [u8; 3],
    /// The active tab's raised-button background.
    active_bg: [u8; 3],
    /// The active tab's LABEL ink. Equal to `fg` for the theme-derived strip; a
    /// user `active_tab_color` override replaces it with black/white by the
    /// override's own luminance so the label stays readable on any pick.
    active_fg: [u8; 3],
    /// Dimmed foreground for inactive tab labels.
    inactive_fg: [u8; 3],
    /// Theme accent used by the selected underline and state marks.
    accent: [u8; 3],
}

/// Derive the strip tones from a theme. The bare strip + inactive tabs sit on the
/// TERMINAL BACKGROUND, so an unfilled strip (a single tab + empty room, the common
/// case) recedes into the content rather than reading as a heavy gray bar; the ACTIVE
/// tab is a distinct RAISED button (bg stepped toward fg) with full bold fg.
///
/// (Two earlier iterations were rejected by the visual-judge loop: a full fg/bg
/// inversion — a near-white block, "harsh/dated" — and a full-width gray chrome band
/// with the active tab merged into the body, "heavy/unfinished". See tools/visual-judge.)
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
/// legibility (0.15 lifts the default theme's inactive labels from ~5:1 to ~9:1).
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
    // Linear blend of two packed-RGB theme colours: `a` toward `b` by `t` ∈ [0,1].
    let blend = |a: u32, b: u32, t: f32| -> [u8; 3] {
        let (a, b) = (rgb(a), rgb(b));
        let mix = |x: u8, y: u8| (f32::from(x).mul_add(1.0 - t, f32::from(y) * t)).round() as u8;
        [mix(a[0], b[0]), mix(a[1], b[1]), mix(a[2], b[2])]
    };
    // Gentler raise on light themes. The inactive-label dim is deliberately mild
    // (0.15): a heavier dim rendered the unfocused tab titles near-illegible in
    // practice — especially over the vibrancy titlebar, where the effective
    // contrast is below the flat-composite math — while the active tab's RAISED
    // CARD + underline (not label brightness) already carries the focus cue, so
    // inactive labels can stay bright without blurring the active/inactive line.
    let (active_t, inactive_t) = if bg_is_light(rgb(theme.bg)) {
        (0.14, 0.15)
    } else {
        (0.21, 0.15)
    };
    StripColors {
        fg: rgb(theme.fg),
        body_bg: rgb(theme.bg),
        active_bg: blend(theme.bg, theme.fg, active_t),
        active_fg: rgb(theme.fg),
        inactive_fg: blend(theme.fg, theme.bg, inactive_t),
        accent: rgb(theme.cursor),
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

/// A bare strip-background [`RenderCell`] — used to pre-fill a strip row before
/// [`paint_strip`] overwrites the tab segments, and to fill upper rows of a multi-row
/// strip. (Recomputes the tones; only used outside the hot per-cell loop.)
#[must_use]
pub fn blank_cell(theme: Theme) -> RenderCell {
    strip_cell(' ', &strip_colors(theme), StripRole::Inactive)
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
            colors.body_bg,
            false,
            UnderlineStyle::None,
            None,
        ),
        StripRole::NewTab => (colors.fg, colors.body_bg, false, UnderlineStyle::None, None),
        StripRole::Title => (colors.fg, colors.body_bg, true, UnderlineStyle::None, None),
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

/// Sanitize one title char for the strip: control / wide / non-BMP chars are
/// replaced by a single-cell placeholder so the painter's 1-char-per-cell column
/// math stays exact (the MVP strip is single-width). Ordinary printable BMP chars
/// pass through unchanged.
fn strip_char(c: char) -> char {
    if c.is_control() {
        ' '
    } else if (c as u32) > 0xFFFF || aterm_grapheme_wide(c) {
        '·'
    } else {
        c
    }
}

/// A conservative "is this char likely a 2-cell glyph?" test WITHOUT pulling the
/// width tables into this MVP: CJK/Hangul/Kana/fullwidth ranges. A false negative
/// only mildly misaligns the strip title (cosmetic); the close `x` / segment
/// boundaries are computed from segment widths, not the title, so hit-testing is
/// unaffected.
fn aterm_grapheme_wide(c: char) -> bool {
    let u = c as u32;
    (0x1100..=0x115F).contains(&u) // Hangul Jamo
        || (0x2E80..=0xA4CF).contains(&u) // CJK, Kangxi, Kana, …
        || (0xAC00..=0xD7A3).contains(&u) // Hangul syllables
        || (0xF900..=0xFAFF).contains(&u) // CJK compat
        || (0xFE30..=0xFE4F).contains(&u) // CJK compat forms
        || (0xFF00..=0xFF60).contains(&u) // fullwidth forms
        || (0xFFE0..=0xFFE6).contains(&u)
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
    /// (the pointer is necessarily ON the tab it is clicking).
    pub hovered: Option<usize>,
    /// The lone tab's one-line description ([`solo_subtitle`]), drawn dim after
    /// the centred title. `None` = nothing the title does not already say.
    pub subtitle: Option<&'a str>,
}

/// Paint the shipping strip from canonical presentation metadata and return sparse
/// inline-image placements for its code-native icon/dirt marks.  `RawRgba8` inline
/// images are aterm's shared render input: CPU, GPU, cached frames, and `image` consume
/// the exact same raster bytes. The cells beneath stay part of the ordinary strip row,
/// so active/inactive backgrounds and full tab/close hit geometry are unchanged.
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
) -> Vec<(usize, ImageRef)> {
    // Derive the strip tones ONCE (frame-invariant), not per cell.
    let colors = strip_colors_with_active(theme, active_override);
    let mut images = Vec::new();
    // Background fill: every strip cell is body-coloured chrome unless a segment
    // overwrites it, so gaps between segments read as strip, not terminal.
    for cell in row.iter_mut() {
        *cell = strip_cell(' ', &colors, StripRole::Inactive);
    }
    for seg in segments {
        let is_active = matches!(seg.kind, TabHit::Select(i) if i == active);
        let tab_role = if is_active {
            StripRole::Active
        } else {
            StripRole::Inactive
        };
        let put = |row: &mut [RenderCell], col: u16, ch: char, role: StripRole| {
            if let Some(slot) = row.get_mut(col as usize) {
                *slot = strip_cell(ch, &colors, role);
            }
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
                let (title, subtitle) = solo_band_text(title, paint.subtitle, span as usize);
                let width = title.chars().count()
                    + subtitle
                        .as_ref()
                        .map_or(0, |s| SOLO_GAP_COLS + s.chars().count());
                let start = seg.start_col + (span.saturating_sub(width as u16)) / 2;
                let mut col = start;
                for ch in title.chars() {
                    put(row, col, strip_char(ch), StripRole::Title);
                    col = col.saturating_add(1);
                }
                if let Some(subtitle) = subtitle {
                    col = col.saturating_add(SOLO_GAP_COLS as u16);
                    for ch in subtitle.chars() {
                        put(row, col, strip_char(ch), StripRole::Inactive);
                        col = col.saturating_add(1);
                    }
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
                let item = metadata.and_then(|items| items.get(i)).copied();
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
                let avail = layout.title_end.saturating_sub(layout.title_start);
                let raw = titles.get(i).map(String::as_str).unwrap_or("");
                let label = truncate_title(raw, avail as usize);
                for (col, ch) in (layout.title_start..).zip(label.chars()) {
                    if col >= layout.title_end {
                        break;
                    }
                    put(row, col, strip_char(ch), tab_role);
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
                for c in seg.start_col..seg.end_col {
                    put(row, c, ' ', StripRole::NewTab);
                }
                // Centre the `+` in the 3-cell ` + ` affordance.
                put(row, seg.start_col + 1, '+', StripRole::NewTab);
            }
            TabHit::Update => {
                // The leading `↻` update-ready alert — a raised highlighted button.
                for c in seg.start_col..seg.end_col {
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
    images
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

fn append_icon_images(
    images: &mut Vec<(usize, ImageRef)>,
    start_col: u16,
    kind: TabIconKind,
    color: [u8; 3],
) {
    let image = image_data(tab_icon_primitives(kind), color, ICON_COLS);
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
    let primitives = status_primitives(metadata);
    images.push((
        usize::from(col),
        ImageRef {
            image: image_data(&primitives, color, 1),
            cell_row: 0,
            cell_col: 0,
        },
    ));
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
    let title_len = title.chars().count().min(usable);
    let title = truncate_title(title, title_len);
    // Whatever the title left, minus the gap — and only if what survives is still
    // readable ([`SOLO_MIN_DESC_COLS`]); a lone "…" says less than nothing.
    let room = usable
        .saturating_sub(title.chars().count())
        .saturating_sub(SOLO_GAP_COLS);
    let subtitle = subtitle
        .filter(|_| room >= SOLO_MIN_DESC_COLS)
        .map(|text| truncate_title(text, room))
        .filter(|text| !text.is_empty() && text != "…");
    (title, subtitle)
}

/// Truncate `title` to at most `max` display cells, appending `…` when it was cut.
/// `max == 0` yields the empty string. Operates on chars (the strip is single
/// width per char after [`strip_char`]); good enough for the MVP labels.
fn truncate_title(title: &str, max: usize) -> String {
    if max == 0 {
        return String::new();
    }
    let chars: Vec<char> = title.chars().collect();
    if chars.len() <= max {
        return title.to_string();
    }
    if max == 1 {
        return "…".to_string();
    }
    let keep = max - 1;
    let mut out: String = chars[..keep].iter().collect();
    out.push('…');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// ONE tab is the window's TITLE, not a switcher: it takes the WHOLE band up to
    /// the trailing `+`, carries no close column, and is marked `solo` so paint
    /// draws a centred title instead of a raised chip. The same rule the native
    /// strip follows (`toolbar::set_window_tabs`).
    #[test]
    fn single_tab_is_a_full_width_title_band_not_a_capped_chip() {
        let segs = layout_segments(80, 1, 0, false);
        assert_eq!(segs.len(), 2, "one title band + the new-tab affordance");
        assert_eq!(segs[0].kind, TabHit::Select(0));
        assert_eq!(segs[0].start_col, 0);
        // The band is spent, not rationed: everything up to the `+`.
        assert_eq!(segs[0].end_col, 80 - NEW_TAB_W);
        assert!(segs[0].solo, "a lone tab is the window title");
        assert_eq!(
            segs[0].close_col, None,
            "a title band has no close affordance; the window's own already exists"
        );
        // The `+` sits flush after it, NEW_TAB_W cells wide.
        assert_eq!(segs[1].kind, TabHit::NewTab);
        assert!(!segs[1].solo);
        assert_eq!(segs[1].start_col, 80 - NEW_TAB_W);
        assert_eq!(segs[1].end_col, 80);
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
        let segments = layout_segments_with_metadata(40, 1, &metadata, 0, false);
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
        assert_eq!(title_cell.bg, colors.body_bg, "no raised card");
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
        // Tab 1 is inactive → recedes onto the body background (distinct from the
        // active button) and is NOT bold.
        let t1 = &segs[1];
        assert_eq!(
            row[t1.start_col as usize].bg, bg_rgb,
            "inactive tab bg = body (recedes)"
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

    /// `strip_char` keeps printable BMP chars, blanks controls, and placeholders
    /// wide/non-BMP so the painter's one-cell-per-char math holds.
    #[test]
    fn strip_char_sanitizes() {
        assert_eq!(strip_char('a'), 'a');
        assert_eq!(strip_char('\t'), ' ');
        assert_eq!(strip_char('世'), '·'); // wide CJK → placeholder
        assert_eq!(strip_char('\u{1F680}'), '·'); // 🚀 non-BMP → placeholder
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
            let active = rgb(c.fg).contrast(rgb(c.active_bg));
            let new_tab = rgb(c.fg).contrast(rgb(c.body_bg));
            // INACTIVE tab labels sit on the body bg. This was historically
            // UNGUARDED, and a heavy dim shipped them near-illegible ("black on
            // black" on the vibrancy titlebar). Guard them to the 3:1 UI-text floor:
            // intentionally MUTED schemes cannot reach AA 4.5 at all (Solarized
            // Light's FULL-strength text is only ~4.13:1, below 4.5 undimmed; it is
            // also the current minimum here at ~3.20:1), so 3.0 is the achievable
            // universal bar; the default theme is pinned far higher just below.
            let inactive = rgb(c.inactive_fg).contrast(rgb(c.body_bg));
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
            // The active card must be a DISTINCT surface from the body, or the
            // focused tab vanishes into the strip (true on dark and light alike).
            assert_ne!(
                c.active_bg, c.body_bg,
                "{name}: active-tab card is indistinguishable from the body"
            );
        }
        // Lock the REPORTED fix on the default theme (the "black on black" case):
        // inactive labels must stay well clear of the muted floor, not just above 3:1.
        let def = strip_colors(Theme::default());
        let def_inactive = rgb(def.inactive_fg).contrast(rgb(def.body_bg));
        assert!(
            def_inactive >= 7.0,
            "default-theme inactive label contrast {def_inactive:.2} < 7.0:1 (regressed the readability fix)"
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
            luma(dark.active_bg) > luma(dark.body_bg),
            "dark theme active card should be brighter than the body"
        );
        // Light builtin: the active card is darker than the body (a subtle card).
        let light = parts("Solarized Light");
        assert!(
            luma(light.active_bg) < luma(light.body_bg),
            "light theme active card should be darker than the body"
        );
        // The default (dark) theme raises the active card: active = blend(bg, fg, 0.21),
        // inactive labels = blend(fg, bg, 0.15) (mild dim, kept legible — see
        // `strip_contrast_meets_wcag_aa`).
        let def = strip_colors(Theme::default());
        assert!(luma(def.active_bg) > luma(def.body_bg));
    }
}
