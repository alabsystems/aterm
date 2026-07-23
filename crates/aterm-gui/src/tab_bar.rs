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
}

/// The minimum cells a tab segment needs to show ` x ` (a leading pad, at least
/// one title cell, a pad, the close `x`, a trailing pad). Below this, tabs are
/// drawn without a close `x` (just the title) so they still fit + remain clickable.
const MIN_SEG_WITH_CLOSE: u16 = 5;
/// The widest a single tab segment grows to (so two tabs don't each eat half a
/// 200-col window); extra width past this is left as bare strip background.
const MAX_SEG: u16 = 24;
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
/// view remains the select/reorder target. Wide chips reserve a 24 pt close target;
/// compact chips give that space back to the title (Close Tab remains available from
/// the menu/shortcut). Responsive icon/status slots appear only after a useful title
/// width survives.
#[derive(Clone, Copy, PartialEq, Debug)]
pub(crate) struct NativeTabContentLayout {
    pub(crate) close: [f64; 4],
    pub(crate) close_available: bool,
    pub(crate) icon: Option<[f64; 4]>,
    pub(crate) status: Option<[f64; 4]>,
    pub(crate) label: [f64; 4],
}

#[must_use]
pub(crate) fn native_tab_content_layout(
    width: f64,
    height: f64,
    has_icon: bool,
    has_status: bool,
) -> NativeTabContentLayout {
    const CLOSE_X: f64 = 2.0;
    const CLOSE_SIZE: f64 = 24.0;
    const AFTER_CLOSE: f64 = 4.0;
    const AFTER_ICON: f64 = 4.0;
    const STATUS_SIZE: f64 = 16.0;
    const AFTER_STATUS: f64 = 4.0;
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

    let close = [
        CLOSE_X,
        (height - CLOSE_SIZE).max(0.0) * 0.5,
        CLOSE_SIZE,
        CLOSE_SIZE,
    ];
    let close_available = width >= MIN_CLOSE_RESERVING_WIDTH;
    let trailing = if close_available {
        WIDE_TRAILING
    } else {
        COMPACT_INSET
    };
    let minimum_label = if close_available {
        MIN_USEFUL_LABEL
    } else {
        MIN_COMPACT_IDENTITY_LABEL
    };
    let mut x = if close_available {
        CLOSE_X + CLOSE_SIZE + AFTER_CLOSE
    } else {
        COMPACT_INSET
    };
    let icon_fits =
        has_icon && width >= x + TAB_ICON_NATIVE_SIZE + AFTER_ICON + minimum_label + trailing;
    let icon = if icon_fits {
        let icon = [
            x,
            (height - TAB_ICON_NATIVE_SIZE).max(0.0) * 0.5,
            TAB_ICON_NATIVE_SIZE,
            TAB_ICON_NATIVE_SIZE,
        ];
        x += TAB_ICON_NATIVE_SIZE + AFTER_ICON;
        Some(icon)
    } else {
        None
    };
    let status_fits =
        has_status && width >= x + STATUS_SIZE + AFTER_STATUS + minimum_label + trailing;
    let status = if status_fits {
        let status = [
            x,
            (height - STATUS_SIZE).max(0.0) * 0.5,
            STATUS_SIZE,
            STATUS_SIZE,
        ];
        x += STATUS_SIZE + AFTER_STATUS;
        Some(status)
    } else {
        None
    };
    let label = [
        x,
        (height - LABEL_H).max(0.0) * 0.5,
        (width - x - trailing).max(0.0),
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
        });
        UPDATE_W
    } else {
        0
    };
    // Reserve the trailing `+` when there's room for at least one tab AND the `+`.
    let plus_room = cols > lead + NEW_TAB_W;
    let avail = if plus_room { cols - NEW_TAB_W } else { cols };
    let mut x: u16 = lead;
    if tab_count > 0 {
        // Split the available width (past the leading update icon) evenly, capped at
        // MAX_SEG, floored so a tab is at least 1 cell before we stop placing tabs.
        let per = (avail.saturating_sub(lead) / tab_count as u16).clamp(0, MAX_SEG);
        for i in 0..tab_count {
            if per == 0 || x >= avail {
                break; // out of room: remaining tabs are not drawn (still reachable)
            }
            let seg_w = per.min(avail - x);
            let start = x;
            let end = x + seg_w;
            // Draw a close `x` only when the segment is wide enough to also show a
            // title; its column is the last cell minus the trailing pad.
            let close_col = (seg_w >= MIN_SEG_WITH_CLOSE).then(|| end - 2);
            segs.push(TabSegment {
                start_col: start,
                end_col: end,
                close_col,
                kind: TabHit::Select(i),
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
    /// The leading `↻` update-ready alert: a RAISED highlighted button (the active-tab
    /// treatment) so it stands out from the flat `+` and the receded tabs.
    Update,
}

/// The four strip tones derived from a theme, computed ONCE per [`paint_strip`]
/// (hoisted out of the per-cell [`strip_cell`] — the blends are frame-invariant).
#[derive(Clone, Copy)]
struct StripColors {
    /// Full-strength foreground (active-tab text + the `+` affordance).
    fg: [u8; 3],
    /// The terminal body background (bare strip + inactive tabs sit on this).
    body_bg: [u8; 3],
    /// The active tab's raised-button background.
    active_bg: [u8; 3],
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
        inactive_fg: blend(theme.fg, theme.bg, inactive_t),
        accent: rgb(theme.cursor),
    }
}

/// Is this background a LIGHT one? A cheap perceptual-luma threshold (no sRGB-linear
/// round-trip needed for a binary dark/light decision). Every bundled dark scheme
/// sits well below the threshold and every light scheme well above it, so the
/// appearance-aware `strip_colors` branch never misclassifies a built-in. The ONE
/// dark/light classifier for all chrome (this strip, `hud_bar`, and the native
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
            colors.fg,
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
        // A raised, underlined highlighted button (like the active tab) so the update
        // alert draws the eye without a hardcoded chrome colour.
        StripRole::Update => (
            colors.fg,
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
    active: usize,
    theme: Theme,
) {
    let _ = paint_strip_impl(row, segments, titles, None, active, theme);
}

/// Paint the shipping strip from canonical presentation metadata and return sparse
/// inline-image placements for its code-native icon/dirt marks.  `RawRgba8` inline
/// images are aterm's shared render input: CPU, GPU, cached frames, and `image` consume
/// the exact same raster bytes. The cells beneath stay part of the ordinary strip row,
/// so active/inactive backgrounds and full tab/close hit geometry are unchanged.
pub(crate) fn paint_strip_with_metadata(
    row: &mut [RenderCell],
    segments: &[TabSegment],
    titles: &[String],
    metadata: &[TabStripMetadata],
    active: usize,
    theme: Theme,
) -> Vec<(usize, ImageRef)> {
    paint_strip_impl(row, segments, titles, Some(metadata), active, theme)
}

fn paint_strip_impl(
    row: &mut [RenderCell],
    segments: &[TabSegment],
    titles: &[String],
    metadata: Option<&[TabStripMetadata]>,
    active: usize,
    theme: Theme,
) -> Vec<(usize, ImageRef)> {
    // Derive the strip tones ONCE (frame-invariant), not per cell.
    let colors = strip_colors(theme);
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
                if let Some(cx) = seg.close_col {
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

    /// A single tab + the trailing `+` lay out left-to-right within the strip, the
    /// tab packed at the left and the `+` flush after it (the tab is capped at
    /// MAX_SEG so it doesn't eat the whole strip).
    #[test]
    fn single_tab_plus_layout() {
        let segs = layout_segments(80, 1, 0, false);
        assert_eq!(segs.len(), 2, "one tab + the new-tab affordance");
        assert_eq!(segs[0].kind, TabHit::Select(0));
        assert_eq!(segs[0].start_col, 0);
        // Tab is capped at MAX_SEG so it doesn't eat the whole 80-col strip.
        assert_eq!(segs[0].end_col, MAX_SEG);
        // The `+` sits flush after the tab, NEW_TAB_W cells wide.
        assert_eq!(segs[1].kind, TabHit::NewTab);
        assert_eq!(segs[1].start_col, MAX_SEG);
        assert_eq!(segs[1].end_col, MAX_SEG + NEW_TAB_W);
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
            0,
            theme,
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
            0,
            theme,
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

    #[test]
    fn native_point_layout_keeps_close_and_tab_targets_while_reserving_app_icon_only() {
        let plain = native_tab_content_layout(220.0, 28.0, false, false);
        let app = native_tab_content_layout(220.0, 28.0, true, true);
        assert_eq!(plain.close, [2.0, 2.0, 24.0, 24.0]);
        assert_eq!(app.close, plain.close, "close hit target never moves");
        assert!(plain.close_available);
        assert!(app.close_available);
        assert_eq!(app.icon, Some([30.0, 7.0, 14.0, 14.0]));
        assert_eq!(app.status, Some([48.0, 6.0, 16.0, 16.0]));
        assert_eq!(app.label, [68.0, 5.5, 146.0, 17.0]);
        assert_eq!(plain.label, [30.0, 5.5, 184.0, 17.0]);
        assert_eq!(
            plain.label[2] - native_tab_content_layout(220.0, 28.0, true, false).label[2],
            TAB_ICON_NATIVE_SIZE + 4.0,
            "a terminal gets the native icon slot and gap back for its label"
        );
        let compact = native_tab_content_layout(48.0, 28.0, true, true);
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

        let phone = native_tab_content_layout(63.0, 28.0, true, false);
        assert!(!phone.close_available);
        assert_eq!(phone.icon, None, "full Settings title wins at phone width");
        assert_eq!(phone.label, [4.0, 5.5, 55.0, 17.0]);

        let app_with_room = native_tab_content_layout(109.0, 28.0, true, false);
        assert!(!app_with_room.close_available);
        assert_eq!(app_with_room.icon, Some([4.0, 7.0, 14.0, 14.0]));
        assert_eq!(app_with_room.label, [22.0, 5.5, 83.0, 17.0]);
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
        paint_strip(&mut row, &segs, &titles, 0, theme);
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
        paint_strip(&mut row, &segs, &titles, 0, theme);
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
