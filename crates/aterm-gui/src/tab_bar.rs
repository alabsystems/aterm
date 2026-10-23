// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Clickable top-of-window tab strip layout ([`layout_segments`]), hit testing ([`hit_test`]),
//! and pixel-band rendering ([`pixel_band`], [`paint_strip`]).

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
/// The connection-mark role, re-exported beside the icon kind so both renderers consume one
/// vocabulary defined on the tab model.
pub(crate) use crate::tab_model::TabConnRole;
/// The four first-party non-terminal tab application identities.
pub(crate) use crate::tab_model::TabIconKind;

/// Non-title presentation metadata consumed by both tab-strip renderers.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub(crate) struct TabStripMetadata {
    pub(crate) icon: Option<TabIconKind>,
    pub(crate) dirty: bool,
    pub(crate) busy: bool,
    /// The need mark: `Wait` the hollow diamond (a decide need only), `Stop` a filled one (a stuck
    /// need), `Yours` the person's hold.
    pub(crate) attention: ChipLevel,
    /// The connection-mark role.
    pub(crate) conn: Option<TabConnRole>,
    pub(crate) closable: bool,
    /// Who drives: the tab's team marks, `↳` before a worker's title and `⇉N` after a manager's
    /// ([`team_title`]).
    pub(crate) team: TabTeam,
    /// The tab's load meter: its focused session's cores-worth on the machine's capacity in 255ths
    /// ([`chip_load_step`]).
    pub(crate) load: u8,
}

/// A tab's team marks: whether its session is a worker (its task's `from=` names another session)
/// and how many workers name it.
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

/// `title` as a strip shows it for a tab wearing `team`.
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

/// Every title of a strip as it shows them ([`team_title`] by index over `metadata`).
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

/// Independently visible status marks carried by a tab.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub(crate) enum TabStatusKind {
    Dirty,
    Busy,
    Attention,
    /// A live session connection touches this tab.
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
/// How far above the pill's bottom the meter sits, in points: clear of the selected chip's 2 pt
/// accent keyline.
pub(crate) const CHIP_LOAD_LIFT_PT: f32 = 2.5;
/// The meter's thickness, in points.
pub(crate) const CHIP_LOAD_H_PT: f32 = 1.5;

/// A tab's load step ([`TabStripMetadata::load`]) from its cores-worth and the machine's capacity
/// in cores.
#[must_use]
pub(crate) fn chip_load_step(cores: f32, capacity: f32) -> u8 {
    if !(cores >= 0.1 && capacity > 0.0) {
        return 0;
    }
    let share = (cores / capacity).clamp(0.0, 1.0);
    (share * 255.0).round().clamp(1.0, 255.0) as u8
}

/// The load meter's length in points on a pill `pill_w_pt` wide.
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

/// Horizontal centre for one status mark in a 16-unit design box.
#[must_use]
pub(crate) fn tab_status_center(ordinal: usize, count: usize) -> f32 {
    match count {
        0 => 8.0,
        1 => 8.0,
        2 => 4.5 + ordinal.min(1) as f32 * 7.0,
        3 => 3.5 + ordinal.min(2) as f32 * 4.5,
        _ => 2.0 + ordinal.min(3) as f32 * 4.0,
    }
}

/// Per-shape scale for `count` packed status marks.
#[must_use]
pub(crate) fn tab_status_mark_scale(count: usize) -> f32 {
    // past it, and the edge marks (centres 2.0/14.0) stay inside the box.
    if count >= 4 { 0.75 } else { 1.0 }
}

/// One normalized code-native icon primitive in a 16×16 design box.
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
    /// A filled triangle (the connection mark's arrows).
    Triangle {
        points: [[f32; 2]; 3],
    },
}

pub(crate) const TAB_ICON_DESIGN_SIZE: f32 = 16.0;
/// Native toolbar icon size in logical points.
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
    /// Switch the window to this tab index (a click anywhere on the segment but the close `x`).
    Select(usize),
    /// Close this tab index (a click on the segment's close `x`).
    Close(usize),
    /// Open a new tab (a click on the trailing `+`).
    NewTab,
    /// Apply the staged update (a click on the leading `↻`, shown only while a build is staged).
    Update,
    /// The build's version label at the strip's trailing end (Windows only,
    /// [`STRIP_VERSION_SLOT`]).
    Version {
        /// Which length of the label this layout had room for.
        rung: crate::menu::VersionRung,
        /// An update is staged ([`crate::menu::staged_for_version_menu`]).
        attention: bool,
    },
    /// The window's needs pill, where this strip carries it ([`layout_segments_with_pill`]).
    Pill(crate::presence::PillHit),
    /// One of the window's own buttons at the strip's far right.
    Caption(CaptionButton),
}

/// One laid-out tab strip segment.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) struct TabSegment {
    /// First column of this segment (inclusive).
    pub start_col: u16,
    /// One past the last column of this segment (exclusive).
    pub end_col: u16,
    /// The column of the close `x`, if this segment drew one.
    pub close_col: Option<u16>,
    /// The action a plain (non-close) click on this segment performs — on its status marks too,
    /// which are part of the chip.
    pub kind: TabHit,
    /// This is the window's only tab, drawn as the window title (name and description centred on
    /// the body background) rather than a chip with a close affordance.
    pub solo: bool,
}

const MIN_SEG_WITH_CLOSE: u16 = 5;
const NEW_TAB_W: u16 = 3;
const UPDATE_W: u16 = 3;

/// Whether the tab band uses the Windows title-band palette and geometry.
pub(crate) const STRIP_WINDOWS_PAINT: bool = cfg!(windows);

/// Whether the strip draws Windows 11 tabs rather than chips (Windows Terminal's, Edge's and
/// Explorer's tab rows).
pub(crate) const SELECTED_TAB_IS_GROUND: bool = STRIP_WINDOWS_PAINT;

const LIGHT_TAB_HOVER_T: f32 = 0.08;

const CHIP_MAX_COLS: Option<u16> = if STRIP_WINDOWS_PAINT {
    Some(MAX_CHIP_COLS)
} else {
    None
};

const SOLO_CHIP_MAX_COLS: Option<u16> = if cfg!(target_os = "macos") {
    None
} else {
    Some(LONE_CHIP_COLS)
};

const LONE_CHIP_COLS: u16 = if STRIP_WINDOWS_PAINT {
    MAX_CHIP_COLS
} else {
    28
};

const MAX_CHIP_COLS: u16 = 27;

const TAB_MAX_DIP: f32 = 240.0;

const TAB_GROW_DIP: f32 = 432.0;

/// How wide the Windows tab row's tabs are ([`layout_segments_with_caption_sized`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct TabSizing<'a> {
    /// The equal width in columns ([`TAB_MAX_DIP`] at the window's metrics).
    pub(crate) cap: u16,
    /// The most a tab grows to, in columns ([`TAB_GROW_DIP`]).
    pub(crate) grow: u16,
    /// Each tab's wanted width in columns, title and chrome ([`pixel_band::tab_wants`]).
    pub(crate) wants: &'a [u16],
}

impl<'a> TabSizing<'a> {
    /// Equal tabs at [`MAX_CHIP_COLS`] that never grow: the layout's contract where no metrics or
    /// titles are at hand.
    #[cfg(test)]
    pub(crate) const FIXED: TabSizing<'static> = TabSizing {
        cap: MAX_CHIP_COLS,
        grow: MAX_CHIP_COLS,
        wants: &[],
    };

    /// The sizing of a window whose cells are `cell_w` device px at `scale`.
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

    /// Tab `i`'s width when the strip has room: its want, held between the equal width and the
    /// growth cap.
    fn wanted(&self, i: usize) -> u16 {
        self.wants
            .get(i)
            .copied()
            .unwrap_or(0)
            .clamp(self.cap, self.grow.max(self.cap))
    }
}

/// The widths of `count` tabs from `first` that each start at `base` and may grow toward
/// [`TabSizing::wanted`] within `band` columns.
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

const PREFERRED_MIN_TAB_COLS: u16 = 12;

const ACTIVE_TAB_PRESSURE_CAP_COLS: u16 = 18;

const STRIP_SEATS_FLOOR: u16 = PREFERRED_MIN_TAB_COLS;

const STRIP_MIN_SEATS: usize = 2;

/// How many chips `band` columns seat.
fn strip_seats(band: u16, tab_count: usize, seat: u16) -> usize {
    usize::from(band / seat.max(1))
        .clamp(STRIP_MIN_SEATS.min(tab_count.max(1)), tab_count.max(1))
        .min(usize::from(band).max(1))
}

/// The window of tabs the strip seats when `active` is selected, as `(first, len)`.
fn strip_window(active: usize, seats: usize, tab_count: usize) -> (usize, usize) {
    let seats = seats.max(1);
    if tab_count <= seats {
        return (0, tab_count);
    }
    let pages = tab_count.div_ceil(seats);
    let narrow = tab_count / pages;
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

const ICON_COLS: u16 = 2;
const ICON_GAP: u16 = 1;

/// Point-space content geometry shared with the macOS view renderer.
#[derive(Clone, Copy, PartialEq, Debug)]
#[cfg(any(target_os = "macos", test))]
pub(crate) struct NativeTabContentLayout {
    pub(crate) close: [f64; 4],
    pub(crate) close_available: bool,
    pub(crate) icon: Option<[f64; 4]>,
    pub(crate) status: Option<[f64; 4]>,
    pub(crate) label: [f64; 4],
}

const SOLO_SUBTITLE_KEYS: [&str; 3] = ["description", "cwd", "activity"];

/// The one-line description a single-tab window shows beside its title.
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

/// Lay a tab chip's interior out around `center_y`.
#[must_use]
#[cfg(any(target_os = "macos", test))]
pub(crate) fn native_tab_content_layout(
    width: f64,
    center_y: f64,
    has_icon: bool,
    has_status: bool,
) -> NativeTabContentLayout {
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
    const MIN_COMPACT_IDENTITY_LABEL: f64 = 55.0;
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

/// Title/icon/status slots for one laid-out tab segment.
fn tab_content_layout(seg: &TabSegment, metadata: TabStripMetadata) -> TabContentLayout {
    let pad = u16::from(
        STRIP_CHIP_CARDS && seg.end_col.saturating_sub(seg.start_col) >= PREFERRED_MIN_TAB_COLS,
    );
    let leading = seg.start_col.saturating_add(1 + pad);
    let mut title_end = match seg.close_col {
        Some(close) => close.saturating_sub(1),
        None => seg.end_col.saturating_sub(1 + pad),
    };

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

/// How many title cells a `width`-column chip wearing `chrome` keeps: [`tab_content_layout`] asked
/// of a hypothetical segment.
fn chip_title_cols(width: u16, chrome: TabStripMetadata) -> u16 {
    let seg = TabSegment {
        start_col: 0,
        end_col: width,
        close_col: (chrome.closable && width >= MIN_SEG_WITH_CLOSE).then(|| width - 2),
        kind: TabHit::Select(0),
        solo: false,
    };
    let layout = tab_content_layout(&seg, chrome);
    layout.title_end.saturating_sub(layout.title_start)
}

/// The legibility floor in title cells: what a plain chip keeps at [`PREFERRED_MIN_TAB_COLS`].
fn preferred_min_title_cols() -> u16 {
    chip_title_cols(PREFERRED_MIN_TAB_COLS, PLAIN_TAB)
}

/// The narrowest chip that still carries a legible title while wearing `chrome`.
fn chip_seat_cols(chrome: TabStripMetadata) -> u16 {
    let floor = preferred_min_title_cols();
    (STRIP_SEATS_FLOOR..=SEAT_COLS_CEILING)
        .find(|&width| chip_title_cols(width, chrome) >= floor)
        .unwrap_or(SEAT_COLS_CEILING)
}

const SEAT_COLS_CEILING: u16 = STRIP_SEATS_FLOOR + (ICON_COLS + ICON_GAP) + 3 + 1;

/// The narrowest chip this strip can seat: the widest seat any of its tabs needs
/// ([`chip_seat_cols`]).
fn strip_seat_cols(tab_count: usize, metadata: &[TabStripMetadata]) -> u16 {
    (0..tab_count)
        .map(|i| chip_seat_cols(metadata.get(i).copied().unwrap_or(PLAIN_TAB)))
        .max()
        .unwrap_or(PREFERRED_MIN_TAB_COLS)
}

/// Lay out the tab strip across `cols` columns for `tab_count` tabs (`active` is the highlighted
/// one).
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

/// [`layout_segments`] with the seat width (`seat`, from [`strip_seat_cols`]).
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
    let plus_room = cols > lead + NEW_TAB_W;
    let avail = if plus_room { cols - NEW_TAB_W } else { cols };
    let solo = tab_count == 1 && SOLO_TITLE_BAND;
    let mut x: u16 = lead;
    if tab_count > 0 {
        let band = avail.saturating_sub(lead);
        let share = band / tab_count as u16;
        let per = match (tab_count, SOLO_CHIP_MAX_COLS, CHIP_MAX_COLS) {
            (1, Some(_), _) | (_, _, Some(_)) if STRIP_WINDOWS_PAINT => share.min(sizing.cap),
            (1, Some(cap), _) | (_, _, Some(cap)) => share.min(cap),
            _ => share,
        };
        let active = active.min(tab_count - 1);
        let mut seats = tab_count;
        let mut first = 0usize;
        let (mut mark_left, mut mark_right) = (false, false);
        let (active_w, inactive_w) = if tab_count > 1 && share < seat {
            if strip_seats(band, tab_count, seat) >= tab_count {
                let selected = u16::try_from(u32::from(band) * 3 / 5)
                    .unwrap_or(u16::MAX)
                    .min(ACTIVE_TAB_PRESSURE_CAP_COLS)
                    .max(share);
                let inactive = (band.saturating_sub(selected) / (tab_count as u16 - 1)).max(1);
                (selected, inactive)
            } else {
                let marked = band >= seat + 2;
                let counted = if marked { band - 2 } else { band };
                let (start, len) =
                    strip_window(active, strip_seats(counted, tab_count, seat), tab_count);
                seats = len;
                first = start;
                mark_left = marked && first > 0;
                mark_right = marked && first + seats < tab_count;
                let chip_band = band - u16::from(mark_left) - u16::from(mark_right);
                let per = (chip_band / seats.max(1) as u16).max(1);
                (per, per)
            }
        } else {
            (per, per)
        };
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
                // Distinct branch.
                break;
            }
            let seg_w = per.min(avail - x);
            let start = x;
            let end = x + seg_w;
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
        x = x.saturating_add(u16::from(mark_right)).min(avail);
    }
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

/// The metadata-aware layout.
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

/// How many columns each rung of the strip's version label needs at this window's metrics
/// ([`pixel_band::version_slot_cols`] measures them in the band's UI face and pen).
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

/// The strip layout without a pill, its tabs equal at [`MAX_CHIP_COLS`]
/// ([`layout_segments_with_version_sized`] sizes them).
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

/// [`layout_segments_with_version`] with the Windows tabs sized by `sizing` ([`TabSizing`]).
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

/// The window's needs pill as the in-grid strip carries it: what lit it and its words.
#[derive(Clone, Copy, Debug)]
pub(crate) struct StripPill<'a> {
    /// What lit it: its glyph's tone.
    pub(crate) kind: crate::presence::PillKind,
    /// Its words, in the pieces the layout clips.
    pub(crate) words: &'a crate::presence::PillWords,
}

/// The in-grid pill's share of a `cols`-wide strip, its pads included.
fn strip_pill_share(cols: u16) -> u16 {
    u16::try_from(u32::from(cols) * 2 / 5).unwrap_or(u16::MAX)
}

/// The in-grid pill laid out in its share of a `cols`-wide strip.
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

/// A whole-cell span edge of the in-grid pill's layout as a column offset.
fn cell_of(x: f64) -> u16 {
    x.round().clamp(0.0, f64::from(u16::MAX)) as u16
}

/// The pill's regions from column `start` on.
fn push_pill_segments(
    segments: &mut Vec<TabSegment>,
    layout: &aterm_messages::presence::PillLayout,
    start: u16,
) {
    let width = strip_pill_cols(layout);
    let mut region = crate::presence::PillHit::Go;
    let mut run_start = start;
    for offset in 0..width {
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

const CAPTION_STRIP_FLOOR: u16 = NEW_TAB_W + 3;

/// [`layout_segments_with_caption_sized`] with no needs pill and equal tabs.
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

/// Which rung of the version label fits beside `tab_count` tabs on a `cols`-wide strip, and how
/// wide it is.
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

/// Map a strip click at column `col` to its [`TabHit`], or `None` for a click on bare strip
/// background (between/after segments).
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

/// The columns the `‹` / `›` overflow marks paint on, or `None` for an edge that reaches the end of
/// the tab list.
#[must_use]
pub(crate) fn strip_overflow(segments: &[TabSegment], tab_count: usize) -> StripOverflow {
    let mut lo: Option<(usize, u16)> = None;
    let mut hi: Option<(usize, u16)> = None;
    for seg in segments {
        let TabHit::Select(i) = seg.kind else {
            continue;
        };
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
    /// An unfocused tab's card on a chip-card band ([`STRIP_CHIP_CARDS`]): dim label on a quiet
    /// chip surface.
    Chip,
    /// An unfocused tab under the pointer (chrome band only).
    Hover,
    /// The vertical hairline on a quiet chip's leading pad cell (chrome band only).
    Separator,
    /// The trailing `+` new-tab affordance.
    NewTab,
    /// The `+` under the pointer: the band's hover material ([`StripColors::hover_bg`]) under the
    /// button's full-strength ink.
    NewTabHover,
    /// The solo band's title: full-strength fg on the body, no raised card and no selection
    /// underline.
    Title,
    /// The leading `↻` update-ready alert.
    Update,
}

/// Whether the in-grid strip paints itself as a chrome band (own surface tone, a hairline seam
/// against the grid, a raised `+`) instead of receding into the terminal background.
pub(crate) const STRIP_IS_CHROME_BAND: bool = !cfg!(target_os = "macos");

/// Whether the strip carries the build's version label at its trailing end ([`TabHit::Version`]).
pub(crate) const STRIP_VERSION_SLOT: bool = cfg!(windows);

/// The trailing version label's card and ink ([`TabHit::Version`]).
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

const SOLO_TITLE_BAND: bool = cfg!(target_os = "macos");

/// Whether the chrome band uses chip cards.
pub(crate) const STRIP_CHIP_CARDS: bool = STRIP_IS_CHROME_BAND;

/// Whether the strip resolves every chip's label in one pass over all tabs
/// ([`distinct_chip_labels`]) instead of truncating each title on its own.
pub(crate) const STRIP_DISTINCT_LABELS: bool = STRIP_IS_CHROME_BAND;

const STRIP_LEGIBILITY_FLOOR: f64 = 3.0;

const STRIP_ACCENT_FLOOR: f64 = 3.0;

/// The strip tones derived from a theme.
#[derive(Clone, Copy)]
struct StripColors {
    fg: [u8; 3],
    band_bg: [u8; 3],
    foot: [u8; 3],
    active_bg: [u8; 3],
    raise_bg: [u8; 3],
    raise_fg: [u8; 3],
    active_fg: [u8; 3],
    inactive_fg: [u8; 3],
    chip_bg: [u8; 3],
    chip_fg: [u8; 3],
    chip_button_fg: [u8; 3],
    hover_bg: [u8; 3],
    hover_fg: [u8; 3],
    button_hover_fg: [u8; 3],
    accent: [u8; 3],
    update_bg: [u8; 3],
    update_fg: [u8; 3],
    update_rule: [u8; 3],
    seam: Option<[u8; 3]>,
    active_rim: Option<[u8; 3]>,
}

const TAB_RIM_BELOW: f64 = 1.15;

/// The selected tab's outline over the band ([`StripColors::active_rim`]).
fn selected_tab_rim(active: [u8; 3], band: [u8; 3], seam: Option<[u8; 3]>) -> Option<[u8; 3]> {
    seam.filter(|_| SELECTED_TAB_IS_GROUND && windows_tab_wears_rim(active, band))
}

/// Whether a selected Windows tab filled `active` on `band` wears the seam's outline.
pub(crate) fn windows_tab_wears_rim(active: [u8; 3], band: [u8; 3]) -> bool {
    bg_is_light(band) || crate::chrome_band::contrast(active, band) < TAB_RIM_BELOW
}

/// The strip tones under an OS-forced chrome palette ([`crate::chrome_band::ForcedChrome`], i.e.
/// Windows High Contrast).
fn forced_strip_colors(hc: crate::chrome_band::ForcedChrome, foot: [u8; 3]) -> StripColors {
    let ink = crate::chrome_band::forced_ink;
    let band_fg = ink(hc.window_text, hc.btn_face);
    let plain_fg = ink(hc.window_text, hc.window);
    StripColors {
        fg: band_fg,
        band_bg: hc.btn_face,
        foot,
        active_bg: hc.highlight,
        raise_bg: hc.window,
        raise_fg: plain_fg,
        active_fg: ink(hc.highlight_text, hc.highlight),
        inactive_fg: band_fg,
        chip_bg: hc.btn_face,
        chip_fg: band_fg,
        chip_button_fg: band_fg,
        hover_bg: hc.window,
        hover_fg: plain_fg,
        button_hover_fg: plain_fg,
        accent: hc.highlight,
        update_bg: hc.window,
        update_fg: plain_fg,
        update_rule: plain_fg,
        seam: Some(ink(hc.window_text, hc.btn_face)),
        active_rim: None,
    }
}

/// Derive the strip tones from a theme.
fn strip_colors(p: impl Into<ChromePalette>) -> StripColors {
    strip_colors_on(p, StripFloor::Ground)
}

/// What lies right under the strip's seam this frame, across the window.
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

    /// The selected tab's surface over this floor.
    fn selected_surface(self, ground: [u8; 3], band: [u8; 3]) -> [u8; 3] {
        let tone = self.tone(ground);
        if crate::chrome_band::contrast(tone, band) >= TAB_FLOOR_APART {
            tone
        } else {
            ground
        }
    }
}

const TAB_FLOOR_APART: f64 = 1.03;

/// [`strip_colors`] over `floor`.
fn strip_colors_on(p: impl Into<ChromePalette>, floor: StripFloor) -> StripColors {
    let p = p.into();
    if let Some(hc) = crate::chrome_band::forced_chrome() {
        return forced_strip_colors(hc, floor.tone(crate::settings::u32_rgb(p.theme.bg)));
    }
    let theme = p.theme;
    let tokens = p.tokens();
    let rgb = |c: u32| {
        [
            ((c >> 16) & 0xff) as u8,
            ((c >> 8) & 0xff) as u8,
            (c & 0xff) as u8,
        ]
    };
    let light = bg_is_light(rgb(theme.bg));
    let (active_t, inactive_t, hover_t, chip_t) = match (light, STRIP_CHIP_CARDS) {
        (true, true) if SELECTED_TAB_IS_GROUND => (0.6, 0.15, LIGHT_TAB_HOVER_T, 0.0),
        (false, true) if SELECTED_TAB_IS_GROUND => (0.21, 0.15, 0.13, 0.0),
        (true, false) => (0.14, 0.15, 0.05, 0.0),
        (true, true) => (0.14, 0.15, 0.10, 0.055),
        (false, false) => (0.21, 0.15, 0.07, 0.0),
        (false, true) => (0.21, 0.15, 0.13, 0.06),
    };
    let fg = rgb(theme.fg);
    let ground = rgb(theme.bg);
    let band_bg = if STRIP_IS_CHROME_BAND {
        crate::chrome_band::band_colors(p).bar_bg
    } else {
        rgb(theme.bg)
    };
    let rise = if light && SELECTED_TAB_IS_GROUND {
        [0xff, 0xff, 0xff]
    } else {
        fg
    };
    let raise_bg = crate::chrome_band::mix3(band_bg, rise, active_t);
    let ink = |c: [u8; 3], on: [u8; 3]| {
        crate::chrome_band::ensure_contrast(c, on, STRIP_LEGIBILITY_FLOOR)
    };
    let raise_fg = ink(fg, raise_bg);
    let band_fg = ink(fg, band_bg);
    let dim = ink(crate::chrome_band::mix3(fg, band_bg, inactive_t), band_bg);
    let inactive_fg = if crate::chrome_band::contrast(dim, band_bg)
        > crate::chrome_band::contrast(band_fg, band_bg)
    {
        band_fg
    } else {
        dim
    };
    let hover_bg = crate::chrome_band::mix3(band_bg, fg, hover_t);
    let hover_fg = ink(inactive_fg, hover_bg);
    let button_hover_fg = ink(fg, hover_bg);
    let chip_bg = crate::chrome_band::mix3(band_bg, fg, chip_t);
    let chip_fg = ink(inactive_fg, chip_bg);
    let chip_button_fg = ink(fg, chip_bg);
    let foot = floor.tone(ground);
    let (active_bg, active_fg) = if SELECTED_TAB_IS_GROUND {
        let selected = floor.selected_surface(ground, band_bg);
        (selected, ink(fg, selected))
    } else {
        (raise_bg, raise_fg)
    };
    let accent = floor_on_both(
        tokens.accent_on(active_bg, STRIP_ACCENT_FLOOR),
        active_bg,
        band_bg,
        STRIP_ACCENT_FLOOR,
    );
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
        update_bg: raise_bg,
        update_fg: raise_fg,
        update_rule,
        seam,
        active_rim: selected_tab_rim(active_bg, band_bg, seam),
    }
}

/// [`floor_on_both`]'s last rule over any number of grounds.
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

/// `mark` held to `floor` on the card and the band it may sit on: floored on the card, then on the
/// band.
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

/// [`strip_colors`] with the authored `active_tab_color` (config over theme).
fn strip_colors_for(p: impl Into<ChromePalette>) -> StripColors {
    strip_colors_for_on(p, StripFloor::Ground)
}

/// [`strip_colors_for`] over `floor` ([`strip_colors_on`]): what the live strip paints.
fn strip_colors_for_on(p: impl Into<ChromePalette>, floor: StripFloor) -> StripColors {
    let p = p.into();
    let active_override = p.authored.active_tab;
    let mut colors = strip_colors_on(p, floor);
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
        colors.update_bg = active_bg;
        colors.update_fg = active_fg;
    }
    colors
}

/// How strongly macOS's native toolbar paints the selected tab's pill.
pub(crate) const NATIVE_PILL_AUTO_ALPHA: f32 = 0.42;

/// The automatic selected-tab colour.
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

/// The strip's tones as a picture of it paints them (the Themes page's demo window).
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

/// Whether this background is light: black text out-contrasts white on it (the wcag pole).
pub(crate) fn bg_is_light(bg: [u8; 3]) -> bool {
    aterm_messages::ink::bg_is_light(bg)
}

/// [`bg_is_light`] inverted for a packed-RGB theme colour: `true` when the theme background is
/// dark.
pub(crate) fn theme_is_dark(bg: u32) -> bool {
    !bg_is_light([
        ((bg >> 16) & 0xff) as u8,
        ((bg >> 8) & 0xff) as u8,
        (bg & 0xff) as u8,
    ])
}

/// The band tone and seam hairline, packed `0x00RRGGBB`.
#[must_use]
pub(crate) fn strip_bleed_tones(p: impl Into<ChromePalette>) -> Option<(u32, Option<u32>)> {
    STRIP_IS_CHROME_BAND.then(|| {
        let pack = |c: [u8; 3]| (u32::from(c[0]) << 16) | (u32::from(c[1]) << 8) | u32::from(c[2]);
        let colors = strip_colors(p);
        (pack(colors.band_bg), colors.seam.map(pack))
    })
}

/// The band's hover wash for `theme` ([`StripColors::hover_bg`]), exposed for the band-hover
/// pointer tests in `app_mouse`.
#[cfg(all(test, any(windows, target_os = "linux")))]
pub(crate) fn strip_hover_bg_for_test(theme: Theme) -> [u8; 3] {
    strip_colors(theme).hover_bg
}

/// A bare strip-background [`RenderCell`], to pre-fill strip rows before [`paint_strip`].
#[must_use]
pub(crate) fn blank_cell(p: impl Into<ChromePalette>) -> RenderCell {
    strip_cell(' ', &strip_colors(p), StripRole::Inactive)
}

/// The chrome label's logical size: the settings tray's chrome base (13 px at 96 dpi ≈ 10 pt), not
/// the terminal `font_px`.
pub(crate) const CHROME_LABEL_LOGICAL_PX: f32 = 13.0;

/// The chrome label base for a `row_h`-px row at the window's `scale`.
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

/// The band's foot in logical px (Windows): the terminal's ground under the seam, before the grid's
/// first row.
pub(crate) const STRIP_FOOT_LOGICAL_PX: f32 = if STRIP_WINDOWS_PAINT { 6.0 } else { 0.0 };

/// Bottom edge in device px for a strip canvas `canvas_px` tall targeting visible height
/// `target_px`.
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
    /// Window `wid`'s band foot ([`strip_foot_px`]) under a `strip`-row strip.
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

/// The strip seam's thickness in device px at `scale`.
#[must_use]
pub(crate) fn strip_seam_px(scale: f32) -> usize {
    if scale.is_finite() && scale >= 1.0 {
        scale.floor() as usize
    } else {
        1
    }
}

/// Close the bottom of the strip's last row against the terminal content with a hairline seam, in
/// place: Linux's seam.
pub(crate) fn seal_strip_bottom(row: &mut [RenderCell], p: impl Into<ChromePalette>) {
    if STRIP_WINDOWS_PAINT {
        return;
    }
    let Some(seam) = strip_colors(p).seam else {
        return;
    };
    for cell in row.iter_mut() {
        // its seam.
        if cell.underline == UnderlineStyle::None {
            cell.underline = UnderlineStyle::Single;
            cell.underline_color = Some(seam);
        }
    }
}

/// The hairline closing the band against the terminal content on Windows ([`STRIP_WINDOWS_PAINT`]).
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

/// Build the [`RenderCell`] for a strip cell from precomputed [`StripColors`] and the cell's
/// [`StripRole`].
fn strip_cell(ch: char, colors: &StripColors, role: StripRole) -> RenderCell {
    let (fg, bg, bold, underline, underline_color) = match role {
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
        StripRole::Chip => (
            colors.chip_fg,
            colors.chip_bg,
            false,
            UnderlineStyle::None,
            None,
        ),
        StripRole::Hover => (
            colors.hover_fg,
            colors.hover_bg,
            false,
            UnderlineStyle::None,
            None,
        ),
        StripRole::Separator => (
            colors.seam.unwrap_or(colors.inactive_fg),
            colors.band_bg,
            false,
            UnderlineStyle::None,
            None,
        ),
        StripRole::NewTab if STRIP_CHIP_CARDS => (
            colors.chip_button_fg,
            colors.chip_bg,
            false,
            UnderlineStyle::None,
            None,
        ),
        StripRole::NewTab if STRIP_IS_CHROME_BAND => (
            colors.raise_fg,
            colors.raise_bg,
            false,
            UnderlineStyle::None,
            None,
        ),
        StripRole::NewTab => (colors.fg, colors.band_bg, false, UnderlineStyle::None, None),
        StripRole::NewTabHover => (
            colors.button_hover_fg,
            colors.hover_bg,
            false,
            UnderlineStyle::None,
            None,
        ),
        StripRole::Title => (colors.fg, colors.band_bg, true, UnderlineStyle::None, None),
        StripRole::Update if STRIP_CHIP_CARDS => (
            colors.active_fg,
            colors.active_bg,
            true,
            UnderlineStyle::None,
            None,
        ),
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

/// Sanitize one title char for the strip: control chars are blanked.
fn strip_char(c: char) -> char {
    if c.is_control() { ' ' } else { c }
}

/// Display cells one projected title char occupies on the strip.
fn strip_char_cells(c: char) -> u16 {
    aterm_grapheme::char_width(c).max(1) as u16
}

/// Display cells a whole title occupies after [`strip_char`] projection.
fn strip_display_cells(s: &str) -> usize {
    s.chars()
        .map(|c| usize::from(strip_char_cells(strip_char(c))))
        .sum()
}

/// Whether the chip at `index` draws a leading `│` divider.
fn strip_separates(index: usize, active: usize, hovered: Option<usize>) -> bool {
    let beside = |edge: usize| index == edge || index == edge + 1;
    index > 0 && !beside(active) && !hovered.is_some_and(beside)
}

/// Paint the laid-out `segments` into `row` (one strip row, `cols` wide, pre-filled with the chrome
/// background).
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

/// Per-frame paint inputs that are neither geometry nor colour: the hovered tab and the solo band's
/// description.
#[derive(Clone, Copy, Default)]
pub(crate) struct StripPaint<'a> {
    /// Index of the tab under the pointer, if any.
    pub hovered: Option<usize>,
    /// Whether the pointer is on the trailing `+`.
    pub new_tab_hovered: bool,
    /// Whether the pointer is on the trailing version label ([`TabHit::Version`]).
    pub version_hovered: bool,
    /// The window button under the pointer ([`TabHit::Caption`]), which takes the band's hover
    /// material.
    pub caption_hovered: Option<CaptionButton>,
    /// The lone tab's one-line description ([`solo_subtitle`]), drawn dim after the centred title.
    pub subtitle: Option<&'a str>,
    /// The live inline session-rename field, when this window has one and the in-grid strip
    /// presents it.
    pub rename: Option<StripRenameField<'a>>,
}

/// The in-grid inline rename editor as the strip painter needs it: which tab it sits over, and the
/// field's text and caret.
#[derive(Clone, Copy)]
pub(crate) struct StripRenameField<'a> {
    /// Index of the edited tab in `titles`/`segments`.
    pub tab: usize,
    /// The field's text.
    pub text: &'a str,
    /// Caret position in `text`, as a byte offset on a char boundary.
    pub cursor: usize,
}

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

/// The rename composition's frame cell: its painted caret when one fits, otherwise the start of the
/// strip's last row.
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
    let colors = strip_colors_for_on(palette, floor);
    let mut images = Vec::new();
    let mut rename_caret = None;
    for cell in row.iter_mut() {
        *cell = strip_cell(' ', &colors, StripRole::Inactive);
    }
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
        let put_wide_tail = |row: &mut [RenderCell], col: u16, role: StripRole| {
            if let Some(slot) = row.get_mut(col as usize) {
                let mut cell = strip_cell(' ', &colors, role);
                cell.wide = true;
                *slot = cell;
            }
        };
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
            TabHit::Select(i) if seg.solo => {
                let span = seg.end_col.saturating_sub(seg.start_col);
                let band_role = StripRole::Inactive;
                let title_role = StripRole::Title;
                for c in seg.start_col..seg.end_col {
                    put(row, c, ' ', band_role);
                }
                let title = titles.get(i).map(String::as_str).unwrap_or("");
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
                let card_start =
                    if STRIP_CHIP_CARDS && seg.end_col.saturating_sub(seg.start_col) >= 2 {
                        seg.start_col + 1
                    } else {
                        seg.start_col
                    };
                for c in seg.start_col..card_start {
                    put(row, c, ' ', StripRole::Inactive);
                }
                for c in card_start..seg.end_col {
                    put(row, c, ' ', tab_role);
                }
                if STRIP_IS_CHROME_BAND
                    && !STRIP_CHIP_CARDS
                    && strip_separates(i, active, paint.hovered)
                {
                    put(row, seg.start_col, '│', StripRole::Separator);
                }
                let editing = paint.rename.filter(|edit| edit.tab == i);
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
                let raw = titles.get(i).map(String::as_str).unwrap_or("");
                if let Some(edit) = editing {
                    rename_caret = paint_rename_field(
                        row,
                        usize::from(layout.title_start)..usize::from(layout.title_end),
                        edit.text,
                        edit.cursor,
                        raw,
                        palette,
                        is_active.then_some(colors.accent),
                    );
                } else {
                    let label = labels.get_mut(i).and_then(Option::take).unwrap_or_else(|| {
                        let avail = layout.title_end.saturating_sub(layout.title_start);
                        chip_cut(raw, avail as usize)
                    });
                    put_text(row, layout.title_start, layout.title_end, &label, tab_role);
                }
                if let Some(cx) = seg.close_col
                    && (paint.hovered == Some(i) || (STRIP_CHIP_CARDS && is_active))
                {
                    put(row, cx, '✕', tab_role);
                    if STRIP_CHIP_CARDS && let Some(slot) = row.get_mut(cx as usize) {
                        // weight beside the selected card's bold title.
                        slot.bold = false;
                    }
                }
            }
            TabHit::NewTab => {
                let card = seg
                    .start_col
                    .saturating_add(u16::from(STRIP_IS_CHROME_BAND));
                let plus_role = if paint.new_tab_hovered {
                    StripRole::NewTabHover
                } else {
                    StripRole::NewTab
                };
                for c in card..seg.end_col {
                    put(row, c, ' ', plus_role);
                }
                put(row, seg.start_col + 1, '+', plus_role);
            }
            TabHit::Update => {
                let end = seg.end_col.saturating_sub(u16::from(STRIP_IS_CHROME_BAND));
                for c in seg.start_col..end {
                    put(row, c, ' ', StripRole::Update);
                }
                put(row, seg.start_col + 1, '\u{21bb}', StripRole::Update);
            }
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
            TabHit::Close(_) | TabHit::Pill(_) => {}
        }
    }
    for (col, mark) in strip_overflow(segments, titles.len()).columns() {
        if let Some(slot) = row.get_mut(usize::from(col)) {
            *slot = strip_cell(mark, &colors, StripRole::Inactive);
        }
    }
    images.sort_unstable_by_key(|(col, _)| *col);
    (images, rename_caret)
}

/// Half the length of the tab's own note's dash ([`ChipLevel::Note`]), in the 16-unit design box,
/// before the four-mark shrink.
pub(crate) const NOTE_DASH_HALF: f32 = 2.0;

/// The ink a chip's status marks are drawn in (one image, one ink): a need's mark wears the
/// border's tone.
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

/// Paint the window's needs pill into the strip row `row` over the columns its regions took
/// ([`TabHit::Pill`], [`layout_segments_with_pill`]).
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
            TabStatusKind::Dirty => primitives.push(TabIconPrimitive::Dot {
                center: [x, 8.0],
                radius: 1.75 * s,
            }),
            TabStatusKind::Busy => primitives.push(TabIconPrimitive::RoundedRect {
                rect: [x - 2.0 * s, 8.0 - 2.0 * s, 4.0 * s, 4.0 * s],
                radius: 2.0 * s,
                width: 1.25 * s,
            }),
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
            TabStatusKind::Connection => {
                let Some(role) = metadata.conn else {
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

/// Rasterize one icon ir to transparent straight-alpha RGBA.
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

const ICON_MEMO_CAP: usize = 16;

thread_local! {
    static ICON_MEMO: RefCell<Vec<(IconKey, Arc<ImageData>)>> = const { RefCell::new(Vec::new()) };
}

/// Reuse the `Arc<ImageData>` of an already-rasterized glyph.
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

const RENAME_SCROLL_LEFT: char = '‹';
const RENAME_SCROLL_RIGHT: char = '›';

const STRIP_OVERFLOW_LEFT: char = RENAME_SCROLL_LEFT;
const STRIP_OVERFLOW_RIGHT: char = RENAME_SCROLL_RIGHT;

/// Paint the inline session-rename field across `field` (a column range of `row`).
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
    let chars: Vec<(char, usize)> = text
        .chars()
        .map(|raw| {
            let ch = strip_char(raw);
            (ch, usize::from(strip_char_cells(ch)))
        })
        .collect();
    let caret = text
        .get(..cursor.min(text.len()))
        .map_or(chars.len(), |head| head.chars().count())
        .min(chars.len());
    for cell in &mut row[field.clone()] {
        *cell = well(' ', c.value, c.field_bg);
    }
    let caret_cells = chars.get(caret).map_or(1, |&(_, w)| w).min(width);
    let mut scroll = caret;
    let mut budget = width - caret_cells;
    while scroll > 0 && chars[scroll - 1].1 <= budget {
        scroll -= 1;
        budget -= chars[scroll].1;
    }
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
            caret_col = Some(col);
            (c.field_bg, c.caret)
        } else {
            (c.value, c.field_bg)
        };
        row[col] = well(ch, ink, ground);
        if w == 2 {
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
    if scroll > 0 && caret != scroll {
        // blank its orphaned continuation too.
        if chars.get(scroll).is_some_and(|&(_, w)| w == 2) && field.start + 1 < field.end {
            row[field.start + 1] = well(' ', c.value, c.field_bg);
        }
        row[field.start] = well(RENAME_SCROLL_LEFT, c.label, c.field_bg);
    }
    if index < chars.len() && !caret_on_last_cell {
        // continuation, so blank the lead as well.
        if pair_ends_field && field.end >= 2 && field.end - 2 >= field.start {
            row[field.end - 2] = well(' ', c.value, c.field_bg);
        }
        row[field.end - 1] = well(RENAME_SCROLL_RIGHT, c.label, c.field_bg);
    }
    caret_col
}

const SOLO_GAP_COLS: usize = 3;
const SOLO_EDGE_COLS: usize = 2;
const SOLO_MIN_DESC_COLS: usize = 4;

/// Fit the solo band's title and description into `span` cells.
#[must_use]
fn solo_band_text(title: &str, subtitle: Option<&str>, span: usize) -> (String, Option<String>) {
    let usable = span.saturating_sub(2 * SOLO_EDGE_COLS);
    let title_len = strip_display_cells(title).min(usable);
    let title = truncate_title(title, title_len);
    let room = usable
        .saturating_sub(strip_display_cells(&title))
        .saturating_sub(SOLO_GAP_COLS);
    let subtitle = subtitle
        .filter(|_| room >= SOLO_MIN_DESC_COLS)
        .map(|text| truncate_title(text, room))
        .filter(|text| !text.is_empty() && text != "…");
    (title, subtitle)
}

/// Truncate `title` to at most `max` display cells ([`strip_char_cells`]), appending `…` when cut.
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

const MIDDLE_MARK: &str = " \u{2026} ";

/// A chip's cut of a title that does not fit `max` display cells.
fn chip_cut(title: &str, max: usize) -> String {
    command_cut(title, max).unwrap_or_else(|| truncate_title(title, max))
}

/// A command line cut in the middle, so its last argument survives:
/// `vim -n -R … src/platform_win.rs`, `less … CHANGELOG.md`.
fn command_cut(title: &str, max: usize) -> Option<String> {
    if strip_display_cells(title) <= max {
        return None;
    }
    let title = title.trim();
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
        return max
            .checked_sub(1)
            .and_then(&fits)
            .map(|tail| format!("\u{2026}{tail}"));
    };
    let head_room = max - mark - strip_display_cells(tail);
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

/// Whether a title's last word names a file or a path, as a command line's argument does.
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

/// [`truncate_title`]'s mirror: keep the tail within `max` display cells, with a leading `…` for
/// the cut head.
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

/// Resolve every chip's painted label in one pass over all tabs.
fn distinct_chip_labels(
    segments: &[TabSegment],
    titles: &[String],
    metadata: Option<&[TabStripMetadata]>,
    active: usize,
    renaming: Option<usize>,
) -> Vec<Option<String>> {
    distinct_chip_labels_in(segments, titles, metadata, active, renaming, 1.0)
}

/// A chip's title budget, in display cells.
fn widened_budget(cells: usize, widen: f32) -> usize {
    if widen.is_finite() && widen > 1.0 {
        (cells as f32 * widen).floor() as usize
    } else {
        cells
    }
}

/// [`distinct_chip_labels`] with every chip's title budget widened to `widen` display cells per
/// cell column.
fn distinct_chip_labels_in(
    segments: &[TabSegment],
    titles: &[String],
    metadata: Option<&[TabStripMetadata]>,
    active: usize,
    renaming: Option<usize>,
    widen: f32,
) -> Vec<Option<String>> {
    let mut labels: Vec<Option<String>> = vec![None; titles.len()];
    let active = active.min(titles.len().saturating_sub(1));
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
    let mut cut: Vec<(usize, usize)> = Vec::new();
    let subjects: Vec<&str> = titles.iter().map(String::as_str).collect();
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
        path_cut[i] = over && is_path_subject(raw);
        let path = path_cut[i].then(|| path_chip_label(i, &subjects, avail));
        labels[i] = Some(path.unwrap_or_else(|| chip_cut(raw, avail)));
        if over {
            cut.push((i, avail));
        }
    }
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
    let mut relabelled = vec![false; cut.len()];
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
        let mut suffix = usize::MAX;
        for window in members.windows(2) {
            suffix = suffix.min(common_suffix_bytes(&titles[window[0]], &titles[window[1]]));
        }
        let suffix = snap_shed(&titles[i], suffix);
        let core = cut_core(&titles[i], suffix);
        let mut prefix = usize::MAX;
        for window in members.windows(2) {
            prefix = prefix.min(common_prefix_bytes(&titles[window[0]], &titles[window[1]]));
        }
        let head = &core[..prefix.min(core.len())];
        let boundary = head.rfind(char::is_whitespace).map_or(head.len(), |p| {
            p + head[p..].chars().next().map_or(1, char::len_utf8)
        });
        let remainder = &core[boundary..];
        let twins = members
            .iter()
            .filter(|&&m| cut_core(&titles[m], suffix) == core)
            .count();
        let family_of_twins = twins == members.len();
        let furniture = if family_of_twins {
            let outside: Vec<&str> = cut
                .iter()
                .map(|&(m, _)| m)
                .filter(|m| !members.contains(m))
                .map(|m| titles[m].as_str())
                .collect();
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
        let path_ordinal = (twins >= 2 && i != active && path_cut[i])
            .then(|| path_ordinal_label(i, &subjects, avail))
            .filter(|label| !family_of_twins || label.contains(" · "));
        let label = if twins >= 2 && i != active {
            if let Some(label) = path_ordinal {
                // family's own context ([`twin_tail`]) to speak.
                label
            } else {
                ordinal_chip_label(i, &titles[i], core, remainder, avail, furniture, tail_cut)
            }
        } else if path_cut[i] {
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
    let mut any_cluster = false;
    for a in 0..cut.len() {
        if relabelled[a] && !kept_head[find(&mut parent, a)] {
            any_cluster = true;
        }
    }
    if pressure && any_cluster {
        let mut flips: Vec<(usize, String)> = Vec::new();
        for (a, &(i, avail)) in cut.iter().enumerate() {
            // A path's first cut is already its own.
            if relabelled[a] || path_cut[i] {
                continue;
            }
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

/// Re-cut a cluster label that kept only shared furniture against the tab's own text.
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

/// The longest ending `title` shares with at least `quorum` of `others`, in bytes: what
/// [`furniture_survivor_recut`] sheds.
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

/// The label a twin paints.
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
    let room = avail.saturating_sub(digit_cells + 3);
    if room >= 2 {
        let tail = tail_cut(core, remainder, room);
        let tail = furniture_survivor_recut(title, furniture, room, tail);
        if !tail.is_empty() && tail != "…" {
            return format!("{digits} · {tail}");
        }
    }
    digits
}

/// The family's cut of a clustered title in `room` cells (the pressure strip's tail dialect).
fn family_tail(core: &str, remainder: &str, room: usize) -> String {
    if !remainder.is_empty() && strip_display_cells(remainder) < room {
        format!("…{remainder}")
    } else {
        truncate_title_tail(core, room)
    }
}

/// What a family of twins paints of its subject in `room` cells, after each ordinal and alone on
/// the active twin.
fn twin_tail(core: &str, remainder: &str, room: usize) -> String {
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

/// Whether the tail cut `cut` (`…<text>`) of `core` starts inside a path component or word (`…term`
/// of `~\aterm`) rather than at its start.
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

/// Whether `text` is a head cut of `core` ([`truncate_title`]): a non-empty start of it followed by
/// the mark.
fn is_head_cut(core: &str, text: &str) -> bool {
    text != core
        && text.strip_suffix('…').is_some_and(|head| {
            !head.is_empty() && !head.starts_with('…') && core.starts_with(head)
        })
}

/// What one chip's tail cut is taken from: its title minus the shared ending computed for it
/// ([`distinct_chip_labels`]).
fn cut_core(title: &str, shared: usize) -> &str {
    let shed = if shared >= title.len() { 0 } else { shared };
    &title[..title.len() - shed]
}

/// The characters a label may be cut after without splitting a name.
fn is_token_boundary(c: char) -> bool {
    matches!(c, '/' | '\\' | '-' | '_' | '.') || c.is_whitespace()
}

/// `shared` (a common suffix of `title`, in bytes) narrowed to start at a token boundary
/// ([`is_token_boundary`]).
fn snap_shed(title: &str, shared: usize) -> usize {
    if shared == 0 || shared >= title.len() {
        return shared;
    }
    let tail = &title[title.len() - shared..];
    tail.char_indices()
        .find(|&(_, c)| is_token_boundary(c))
        .map_or(0, |(at, _)| tail.len() - at)
}

/// Whether a chip's subject is a filesystem path: `~`, `~/…`, `~\…`, `/…`, `C:\…`, `\\host\…`.
fn is_path_subject(subject: &str) -> bool {
    matches!(
        subject.as_bytes(),
        [b'~'] | [b'~', b'/' | b'\\', ..] | [b'/', ..] | [b'\\', b'\\', ..]
    ) || matches!(
        subject.as_bytes(),
        [drive, b':', b'/' | b'\\', ..] if drive.is_ascii_alphabetic()
    )
}

/// The label a cut path chip paints in `room` cells.
fn path_chip_label(tab: usize, subjects: &[&str], room: usize) -> String {
    let subject = subjects[tab];
    let kin = path_kin(subject, subjects);
    path_tail(subject, room, &kin).unwrap_or_else(|| path_fallback(tab, subject, room, &kin))
}

/// The paths a path chip's cut is told apart from.
fn path_kin<'a>(subject: &str, subjects: &[&'a str]) -> Vec<&'a str> {
    subjects
        .iter()
        .copied()
        .filter(|other| *other != subject && is_path_subject(other))
        .collect()
}

/// A path twin's ordinal among a family of other paths.
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

/// Whether `text` ends `name` at the start of a token (`gui` ends `~\x\aterm-gui`, not
/// `~\x\argui`).
fn ends_with_token(name: &str, text: &str) -> bool {
    name.strip_suffix(text)
        .is_some_and(|before| before.is_empty() || before.ends_with(is_token_boundary))
}

/// The cut a path `subject` takes when it does not fit `room` cells.
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

/// What a cut path chip paints when no tail or leaf both fits and differs ([`path_tail`]).
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

/// Byte length of the longest common prefix of `a` and `b`, aligned to char boundaries.
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

/// Byte length of the longest common suffix of `a` and `b`.
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

/// The strip in the UI font.
#[cfg(any(windows, target_os = "linux"))]
pub(crate) mod pixel_band {
    use super::*;
    use crate::tray_raster::UnicodeChars;
    use crate::type_scale::TypeStep;
    use crate::widget::{DrawPrim, TextFace, TextWeight, rgba, text_prim};

    const _: () = assert!(
        STRIP_CHIP_CARDS,
        "the pixel band paints chip-card surfaces; the cell lane beneath it must \
         speak the same language (STRIP_CHIP_CARDS)"
    );

    /// Device-pixel geometry of the strip band for one window (mixed-DPI: every term is this
    /// window's own).
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
        /// Band pixels above the grid (`pad_top + head`, less the stats Deck over the head's top:
        /// `App::strip_band_top_px`).
        pub band_top_px: usize,
        /// The window's scale factor (drives the label size).
        pub scale: f32,
        /// Canvas y (device px from the optical band's top) of the seam's top row.
        pub seam_top_px: Option<usize>,
        /// The band's foot on Windows.
        pub foot_px: usize,
    }

    /// Everything the band raster depends on.
    pub(crate) struct BandInput<'a> {
        pub segments: &'a [TabSegment],
        pub titles: &'a [String],
        pub metadata: &'a [TabStripMetadata],
        pub paint: StripPaint<'a>,
        pub active: usize,
        /// The header's palette, its authored `active_tab_color` included.
        pub palette: ChromePalette,
        pub geometry: BandGeometry,
        /// What lies under the seam this frame ([`StripFloor`]): the selected tab's surface and the
        /// foot's tone.
        pub floor: StripFloor,
    }

    impl BandInput<'_> {
        /// The selected tab, clamped into range exactly as `layout_segments` and
        /// [`distinct_chip_labels`] clamp it.
        fn selected(&self) -> usize {
            self.active.min(self.titles.len().saturating_sub(1))
        }
    }

    /// Rasterize the strip band and return one `Vec<(col, ImageRef)>` per strip row.
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
            || !crate::tray_raster::strip_band_ui_ready()
        {
            return None;
        }
        let img_w = cols.checked_mul(cell_w)?;
        let cells_h = geometry.strip_rows.checked_mul(cell_h)?;
        let lift = geometry.band_top_px;
        let img_h = cells_h.checked_add(lift)?;
        if img_w > usize::from(u16::MAX) || img_h > usize::from(u16::MAX) {
            return None;
        }
        let active = input.selected();
        let colors = strip_colors_for_on(input.palette, input.floor);
        let scale = if geometry.scale.is_finite() && geometry.scale > 0.0 {
            geometry.scale
        } else {
            1.0
        };
        let band_h = (geometry.band_top_px + cells_h) as f32;
        let label_px = band_label_px(band_h, scale);
        let design = BandDesign::resolve(&geometry, img_h, label_px);
        let baseline = design.baseline;
        let cy = baseline - label_px * 0.35;
        let cw = cell_w as f32;
        let variable = crate::tray_raster::strip_band_variable_semibold();
        let variable_pen = variable.as_ref().and_then(VariablePen::new);

        let mut prims: Vec<DrawPrim> = Vec::new();
        let mut overlays: Vec<LabelOverlay> = Vec::new();
        let mut fallback: Vec<(u16, u16)> = Vec::new();
        let mut drew_any = false;

        prims.push(DrawPrim::Panel {
            x: 0.0,
            y: 0.0,
            w: img_w as f32,
            h: img_h as f32,
            radius: 0.0,
            fill: rgba(colors.band_bg, 255),
        });
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
        let opens = design.tabs && colors.active_bg == colors.foot;
        let seam_rows = strip_seam_px(scale).max(1);
        let seam_bottom = geometry
            .seam_top_px
            .map(|top| (top + seam_rows).min(img_h) as f32);
        let mut drawn_tabs: Vec<(u16, u16, bool, bool)> = Vec::new();

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
                TabHit::Pill(_) => {
                    fallback.push((seg.start_col, end));
                }
                TabHit::Select(i) if seg.solo => {
                    let _ = i;
                    fallback.push((seg.start_col, end));
                }
                TabHit::Select(i) => {
                    if input.paint.rename.is_some_and(|edit| edit.tab == i) {
                        fallback.push((seg.start_col, end));
                        drawn_tabs.push((seg.start_col, end, is_active || is_hovered, false));
                        continue;
                    }
                    let item = input.metadata.get(i).copied();
                    let layout = band_content_layout(seg, item, end);
                    let avail = layout.title_end.saturating_sub(layout.title_start);
                    let label: String =
                        labels.get_mut(i).and_then(Option::take).unwrap_or_default();
                    let label_face = if is_active && design.active_semibold {
                        TextFace::UiBold
                    } else {
                        TextFace::Ui
                    };
                    let pieces =
                        if crate::tray_raster::strip_band_run_coverable_in(label_face, &label) {
                            None
                        } else {
                            let title = input.titles.get(i).map_or("", String::as_str);
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
                    if let (Some(icon_start), Some(kind)) =
                        (layout.icon_start, item.and_then(|it| it.icon))
                    {
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
                        let color = status_mark_ink(item, &colors, input.palette, ink);
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
                        let varied = if is_active && wants_semibold {
                            variable_pen.as_ref().and_then(|pen| {
                                let mut run = pen.shape(&label, label_px)?;
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
                    let (center, ink) = {
                        let seg0 = f32::from(seg.start_col) * cw;
                        let seg1 = f32::from(end) * cw;
                        let center = (seg0 + seg1) * 0.5;
                        let card_h = design.card_bot - design.card_top;
                        let hot = input.paint.new_tab_hovered;
                        if design.tabs {
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
        for (col, mark) in strip_overflow(input.segments, input.titles.len()).columns() {
            let center = (f32::from(col) + 0.5) * cw;
            let arm_y = (label_px * 0.30).max(2.5);
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
            1.0,
            [0, 0, 0, 0],
        );
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
            z_index: 0,
            band_lift_px: lift as u16,
            cell_offset_px: (0, 0),
            scaling: ImageScaling::Fit,
            source_rect: None,
        });

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

    const LABELS_LEAD: bool = cfg!(windows);

    /// Where a `width`-px title starts in its `span_px` span at `x0` ([`LABELS_LEAD`]).
    fn label_x(x0: f32, span_px: f32, width: f32) -> f32 {
        if LABELS_LEAD {
            x0
        } else {
            ((span_px - width) * 0.5).max(0.0) + x0
        }
    }

    /// Where the `w`-px hairline between two quiet tabs that meet at device x `boundary` starts.
    fn hairline_x(boundary: f32, w: f32, a_band: bool, b_band: bool) -> Option<f32> {
        let boundary = boundary.round();
        match (a_band, b_band) {
            (true, true) => Some(boundary - (w * 0.5).floor()),
            (true, false) => Some(boundary - w),
            (false, true) => Some(boundary),
            (false, false) => None,
        }
    }

    const TITLE_SAMPLE: &str = "Claude Code: fix tab labels ~/src/aterm-gui MINGW64 pwsh 0.106";

    /// How many characters of a title the UI face sets in one terminal cell column (`cell_w` px) at
    /// `label_px`.
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

    /// Each tab's wanted width in columns ([`TabSizing::wants`]) on a band `band_h` px tall at
    /// `scale` with `cell_w`-px columns.
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
                let (px, by_cells) =
                    if crate::tray_raster::strip_band_run_coverable_in(TextFace::UiBold, title) {
                        let cells = (strip_display_cells(title) as f32 / widen).ceil();
                        (measure(&steady_title(title)), cells)
                    } else if let Some(pieces) = emoji_label(title, label_px, TextFace::UiBold) {
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

    /// `title` as [`tab_wants`] measures it.
    fn steady_title(title: &str) -> std::borrow::Cow<'_, str> {
        let mut chars = title.chars();
        match (chars.next(), chars.next()) {
            (Some(mark), Some(' ')) if !mark.is_alphanumeric() => {
                std::borrow::Cow::Owned(format!("M{}", &title[mark.len_utf8()..]))
            }
            _ => std::borrow::Cow::Borrowed(title),
        }
    }

    /// The label size for a `band_h`-px optical band at `scale`: the chrome label base
    /// ([`chrome_label_base_px`]).
    fn band_label_px(band_h: f32, scale: f32) -> f32 {
        chrome_label_base_px(band_h, scale)
    }

    /// The trailing version label's slot, in columns, for each rung.
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

    /// The version label's inset from each end of its slot, in device px.
    fn version_pad_px(scale: f32) -> f32 {
        (8.0 * scale).round()
    }

    /// One window button ([`TabHit::Caption`]) in its `[x0, x1)` px slot.
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
                prims.push(DrawPrim::Stroke {
                    x: cx - half,
                    y: cy - half + off,
                    w: 2.0 * half - off,
                    h: 2.0 * half - off,
                    radius: 0.0,
                    width: stroke,
                    color: ink,
                });
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

    /// The band's resolved metrics.
    struct BandDesign {
        /// The labels' baseline: cap-centred in the content region, or on Windows ink-centred in
        /// the tab ([`DESCENDER_EM`]).
        baseline: f32,
        /// Top edge of the card row.
        card_top: f32,
        /// Bottom edge of the card row — clear of the seam rule on the canvas's last rows
        /// ([`BandGeometry::seam_top_px`]).
        card_bot: f32,
        /// Windows tabs, not chips ([`SELECTED_TAB_IS_GROUND`]).
        tabs: bool,
        /// Card corner radius.
        radius: f32,
        /// Horizontal inset from a segment's cell edge to its card's painted edge.
        gap_h: f32,
        /// Whether quiet (unfocused, unhovered) tabs paint a chip card rather than bare labels on
        /// the band.
        quiet_cards: bool,
        /// Whether the `+` sits on a quiet chip square rather than a bare glyph.
        quiet_button: bool,
        /// Whether the active label is semibold.
        active_semibold: bool,
    }

    impl BandDesign {
        fn resolve(geometry: &BandGeometry, img_h: usize, label_px: f32) -> Self {
            let s = if geometry.scale.is_finite() && geometry.scale > 0.0 {
                geometry.scale
            } else {
                1.0
            };
            let content_h = geometry
                .seam_top_px
                .map(|y| y as f32)
                .unwrap_or_else(|| img_h as f32 - (2.0 * s).round())
                .clamp(label_px, img_h as f32);
            let tabs = SELECTED_TAB_IS_GROUND;
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
            let radius = (if tabs { 8.0 } else { 6.0 } * s).round().min(card_h * 0.5);
            let gap_h = (3.0 * s).round().max(2.0);
            BandDesign {
                baseline,
                card_top,
                card_bot,
                tabs,
                radius,
                gap_h,
                quiet_cards: !tabs,
                quiet_button: !tabs,
                active_semibold: true,
            }
        }
    }

    const DESCENDER_EM: f32 = 0.22;

    /// The baseline of a `label_px` title in a Windows tab spanning `[top, bottom)`.
    fn tab_baseline(top: f32, bottom: f32, label_px: f32, scale: f32) -> f32 {
        let cap_centred = crate::tray_raster::row_baseline(top, bottom - top, label_px);
        let cap = (2.0 * (cap_centred - (top + bottom) * 0.5)).max(0.0);
        let descent = label_px * DESCENDER_EM;
        let air = scale.max(1.0);
        let lowest = bottom - air - descent;
        let highest = top + air + cap;
        (cap_centred - descent * 0.5)
            .min(lowest)
            .max(highest.min(lowest))
    }

    /// The content geometry of one band segment: [`tab_content_layout`] with metadata, the plain
    /// ` title ✕ ` fallback without.
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

    /// Every chip label the band paints, resolved together.
    fn resolve_band_labels(
        input: &BandInput<'_>,
        active: usize,
        cols: usize,
        cw: f32,
        label_px: f32,
        wants_semibold: bool,
    ) -> Vec<Option<String>> {
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

    /// Each seated tab's cell-pass label at the widest step at which that tab's own label fits its
    /// span ([`resolve_band_labels`]).
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

    /// One chip's label on the way into the pixel fit: the cell pass's resolved string and the span
    /// it has to survive.
    struct BandLabel {
        tab: usize,
        span_px: f32,
        text: String,
    }

    /// Fit every band label to its own span and keep the strip distinct across the fit.
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
        let ordinal = |n: usize| {
            let tab = entries[n].tab;
            pixel_ordinal_label(tab, &entries[n].text, entries[n].span_px, &|s| {
                measure(tab, s)
            })
        };
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
            if fitted[n].1.is_empty() {
                continue;
            }
            let lost = shared[n]
                || label_says_nothing(&fitted[n].1)
                || taken.iter().any(|other| *other == fitted[n].1);
            if !lost {
                taken.push(fitted[n].1.clone());
                continue;
            }
            let recut = ordinal(n);
            if recut.is_empty() {
                // Not even the number fits; keep whatever the fit left.
                continue;
            }
            let mut text = recut;
            if taken.contains(&text) {
                let digits = (tab + 1).to_string();
                let holders = taken.iter().filter(|other| **other == digits).count();
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

    /// Whether this label names anything or is only elision marks.
    fn label_says_nothing(label: &str) -> bool {
        label
            .trim_matches(|c: char| c == '…' || c.is_whitespace())
            .is_empty()
    }

    /// [`ordinal_chip_label`] in the pixel domain.
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
            let composed = head + &tail;
            if !label_says_nothing(&tail) && measure(&composed) <= span_px {
                return composed;
            }
        }
        digits
    }

    /// Shrink `label` (already display-cell-truncated) until it measures inside `span_px` under
    /// `measure`.
    fn fit_label(label: String, span_px: f32, measure: impl Fn(&str) -> f32) -> String {
        if measure(&label) <= span_px {
            return label;
        }
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

    /// Where a tail cut's mark sits in `label` ([`fit_label`]): first (`…sub dir`), or right behind
    /// an ordinal (`12 · …sub dir`).
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

    /// A command cut's head and tail ([`command_cut`]).
    fn middle_cut_parts(label: &str) -> Option<(&str, &str)> {
        if label.starts_with('…') || label.ends_with('…') {
            return None;
        }
        label
            .rsplit_once(MIDDLE_MARK)
            .filter(|(head, tail)| !head.trim().is_empty() && !tail.trim().is_empty())
    }

    /// Whether a label's cut keeps its end ([`tail_cut_mark`], [`middle_cut_parts`]).
    fn cut_keeps_its_end(label: &str) -> bool {
        tail_cut_mark(label).is_some() || middle_cut_parts(label).is_some()
    }

    /// One glyph of a [`ShapedRun`].
    struct ShapedGlyph {
        pen: f32,
        xmin: i32,
        w: usize,
        h: usize,
        ymin: i32,
        cov: Vec<u8>,
    }

    impl ShapedGlyph {
        /// The fontdue pen's placement, term for term: pen + bearing, truncated to the device
        /// column.
        fn x(&self) -> i32 {
            (self.pen + self.xmin as f32).floor() as i32
        }
    }

    /// Where one char of a [`ShapedRun`] ended: the pen after its advance and the glyph count so
    /// far.
    #[derive(Clone, Copy)]
    struct CharCut {
        ch: char,
        pen: f32,
        glyphs: usize,
    }

    /// A label shaped through the variable semibold instance.
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

    /// The variable semibold ready to draw: the instance parsed once with its coords applied,
    /// beside the regular's cmap/kern.
    struct VariablePen<'a> {
        vf: &'a crate::tray_raster::UiVariableSemibold,
        face: aterm_render::variation::VariedFace<'a>,
    }

    impl<'a> VariablePen<'a> {
        /// `None` when the retained bytes no longer parse.
        fn new(vf: &'a crate::tray_raster::UiVariableSemibold) -> Option<Self> {
            let face = aterm_render::variation::VariedFace::parse(&vf.bytes, vf.index, &vf.coords)?;
            Some(Self { vf, face })
        }

        /// Shape `text` at `px` through the variable semibold.
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

        /// [`fit_label`] for an already-shaped run.
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

    /// Shape `text` at `px` through the variable semibold — [`VariablePen::shape`] over a face
    /// parsed for this one call.
    #[cfg(test)]
    fn shape_variable_label(
        vf: &crate::tray_raster::UiVariableSemibold,
        text: &str,
        px: f32,
    ) -> Option<ShapedRun> {
        VariablePen::new(vf)?.shape(text, px)
    }

    /// Lay a [`ShapedRun`] into a straight-alpha RGBA overlay `height` px tall, at most `span_px`
    /// wide.
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

    /// The advance of a colour emoji in a `label_px` label: its square and a hair of air before the
    /// next letter.
    fn emoji_advance_px(label_px: f32) -> f32 {
        emoji_box_px(label_px) + (label_px * 0.12).round()
    }

    /// `title` as pieces the band draws when the UI stack cannot draw every char of it.
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

    /// Where a label with colour emoji is drawn ([`draw_emoji_label`]).
    struct EmojiLabelAt {
        x0: f32,
        span_px: f32,
        baseline: f32,
        cy: f32,
        label_px: f32,
        height: usize,
    }

    /// Draw `pieces` ([`emoji_label`]) as one label in its span.
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

    /// `emoji` fitted into a `side`-px square whose left edge is `x` and whose centre is at `cy`,
    /// as an overlay the canvas's height.
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

    /// Map one 16-unit icon ir box ([`TabIconPrimitive`]) into `DrawPrim`s centred at `(cx, cy)`
    /// with side `side` px.
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

        /// [`ink_bbox`] for the opaque band canvas.
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

        /// Install the host UI faces for this thread.
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

        /// A two-tab strip at `cols` with the version label measured at this module's fixture
        /// metrics, and the label's segment.
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

        /// The slot is the label's measured width rounded up to cells.
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

        /// The band draws the label: it covers the slot's columns and inks them.
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

        /// The window buttons (`crate::client_frame`, lw.5).
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
                let got = pixel(&rgba, w, cx - 5, h / 2);
                assert!(near(got, want), "{hovered:?}: {got:?} vs {want:?}");
                let mark = pixel(&rgba, w, cx, h / 2);
                assert!(!near(mark, want), "{hovered:?}: no mark at the centre");
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
            let lift = BAND_TOP;
            assert_eq!((w, h), (80 * CELL_W, CELL_H + lift), "canvas vs footprint");
            assert_eq!((first.cols, first.rows, first.z_index), (80, 1, 0));
            assert_eq!(usize::from(first.band_lift_px), lift);
            assert!(
                (0..w * h).all(|i| rgba[i * 4 + 3] == 255),
                "an opaque bar has no see-through pixels"
            );

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

        /// A CJK title stays in the pixel band.
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

        /// A colour-emoji title is a band tab ([`emoji_label`]).
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

        /// A CJK title no face maps takes the cell lane.
        #[test]
        fn a_cjk_title_no_face_maps_falls_to_the_cell_lane() {
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

        /// Each label's east asian faces are its own weight's.
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

        /// The real window's band (2 px pad, a 13 px head, one 23 px row, the 6 px foot) at `cols`.
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

        /// The first frame of A CJK title is a band tab.
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

        /// A title the cell lane draws keeps its hairlines.
        #[test]
        fn quiet_neighbours_of_a_cell_lane_title_keep_their_hairlines() {
            if !SELECTED_TAB_IS_GROUND {
                return;
            }
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
            let before = start * CELL_W - 1;
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

        /// Characters missing from the variable UI font (`✓`, Nerd Font PUA icons) render on the
        /// active chip via fallback.
        #[test]
        fn active_label_the_variable_face_cannot_shape_draws_whole_through_the_cascade() {
            if !with_ui_faces() {
                aterm_spec::host::could_not_run("no system UI face here for the strip band");
            }
            let Some(vf) = crate::tray_raster::strip_band_variable_semibold() else {
                crate::tray_raster::clear_ui_fonts_for_test();
                return;
            };
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

        /// The glyph-level fit ([`VariablePen::fit`]) places every glyph where shaping
        /// [`fit_label`]'s string would, across nothing fits.
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

        /// A tab's want ([`tab_wants`]) seats its whole title, laid out and labelled.
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
            let spun: Vec<String> = ["\u{2733}", "\u{2736}", "\u{b7}", "\u{280b}"]
                .iter()
                .map(|frame| format!("{frame} Fix the strip floor under splits"))
                .filter(|title| crate::tray_raster::strip_band_run_coverable(title))
                .collect();
            assert!(spun.len() >= 2, "CONTROL: two frames to compare");
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

        /// The selected title through the variable semibold pen reads at one weight in a dark and a
        /// light theme.
        #[test]
        fn the_selected_title_keeps_its_weight_on_light_and_dark_tabs() {
            if !with_ui_faces() {
                aterm_spec::host::could_not_run("no system UI face here for the strip band");
            }
            if crate::tray_raster::strip_band_variable_semibold().is_none() {
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

        /// The band's title window and span for one segment, measured as [`resolve_band_labels`]
        /// does.
        fn window(seg: &TabSegment) -> (u16, f32) {
            let layout = band_content_layout(seg, None, seg.end_col);
            let avail = layout.title_end.saturating_sub(layout.title_start);
            (avail, f32::from(avail) * CELL_W as f32 - 1.0)
        }

        /// A UI face whose every char out-measures the mono cell by half.
        fn wide_face(_tab: usize, s: &str) -> f32 {
            s.chars().count() as f32 * (CELL_W as f32 * 1.5)
        }

        /// The pixel fit must not collapse labels the cell pass ([`distinct_chip_labels`]) made
        /// distinct.
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

        /// Each repair [`fit_labels_distinctly`] owns, from the string the shipped fit left.
        #[test]
        fn the_pixel_fit_repairs_every_label_that_names_nothing_or_another_tab() {
            let squeeze = wide_face(0, "……");
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

        /// Titles are cut for the UI face on Windows, not for terminal cells (Linux keeps its cell
        /// cut).
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

        /// Each tab is cut at its own widest step ([`widest_fitting_cuts`], font-free).
        #[test]
        fn each_tab_is_cut_at_its_own_widest_step() {
            let wide = "W".repeat(40);
            let titles = [wide.as_str(), "Claude Code: reviewing the strip"];
            let labels_at = |widen: f32| -> Vec<Option<String>> {
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
            let width = |s: &str| {
                s.chars()
                    .map(|c| if c == 'W' { 14.0 } else { 7.0 })
                    .sum::<f32>()
            };
            let overflows = |_: usize, s: &str, span: f32| width(s) > span;
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
            assert_eq!(
                labels_at(1.0)[1].as_deref(),
                Some("Claude Code: review…"),
                "CONTROL"
            );
            let middle = widest_fitting_cuts(&[(1, 180.0)], 1.5, labels_at, overflows);
            assert_eq!(
                middle[1].as_deref(),
                Some("Claude Code: reviewing …"),
                "{middle:?}"
            );
            assert_eq!(middle[0], None);
        }

        /// A neighbour's title costs a tab no characters (with the UI faces).
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
            assert_eq!(
                label_of(r"C:\Users\m6-an\aterm", 0),
                label_of(r"C:\Users\m6-an\aterm", 2)
            );
            crate::tray_raster::clear_ui_fonts_for_test();
        }

        /// Band label plumbing end to end, no fonts needed.
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

        /// A cut that keeps its end keeps it through the pixel fit ([`fit_label`], font-free).
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

        const WIN_BANDS: [(usize, usize, usize); 4] =
            [(19, 13, 10), (17, 15, 12), (13, 19, 15), (9, 23, 18)];

        /// The seam's canvas row for a [`WIN_BANDS`] band, as the splice resolves it
        /// ([`BandGeometry::seam_top_px`]).
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

        /// At every Windows band, the label run sits inside its card and its descenders clear the
        /// seam.
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
                assert!(
                    y1 < seam_top,
                    "{win:?}: descenders reach {y1}, seam rule at {seam_top}"
                );
                if d.tabs {
                    assert_eq!(d.card_bot, seam_top as f32, "{win:?}: on the seam");
                    let (above, below) = (y0 as f32 - d.card_top, d.card_bot - 1.0 - y1 as f32);
                    assert!(
                        above >= 1.0 && below >= 1.0 && (above - below).abs() <= 3.0,
                        "{win:?}: ink {y0}..={y1} in tab {}..{}: {above} above, {below} below",
                        d.card_top,
                        d.card_bot
                    );
                } else {
                    let above = d.card_top;
                    let below = seam_top as f32 - d.card_bot;
                    assert!(
                        (above - below).abs() <= 1.0,
                        "{win:?}: card {}..{} is not centred over the seam at {seam_top}",
                        d.card_top,
                        d.card_bot
                    );
                }
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

        /// The windows tab row (`SELECTED_TAB_IS_GROUND`).
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

        /// The selected tab is one surface with what lies under the seam ([`StripFloor`]), on the
        /// real window's band.
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

        /// A selected tab on a light band wears the seam ([`selected_tab_rim`]).
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

        /// A compact band (the headless 25 px band: a 2 px pad over one 23 px row) keeps a title's
        /// descenders a pixel inside its tab at 100%.
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

    /// Segments for a one-tab window with the solo title band forced on, so the solo painter is
    /// tested on every platform.
    fn forced_solo_segments(cols: u16, metadata: &[TabStripMetadata]) -> Vec<TabSegment> {
        let mut segments = layout_segments_with_metadata(cols, 1, metadata, 0, false);
        if let Some(band) = segments.first_mut() {
            band.solo = true;
            band.close_col = None;
        }
        segments
    }

    /// A lone tab on macOS is the window title (no close column, `solo`, full band).
    #[test]
    fn single_tab_is_a_full_width_title_band_only_where_it_is_the_window_title() {
        let segs = layout_segments(80, 1, 0, false);
        assert_eq!(segs.len(), 2, "one tab + the new-tab affordance");
        assert_eq!(segs[0].kind, TabHit::Select(0));
        assert_eq!(segs[0].start_col, 0, "the lone tab always LEADS");
        match SOLO_CHIP_MAX_COLS {
            None => {
                assert_eq!(segs[0].end_col, 80 - NEW_TAB_W);
                assert!(segs[0].solo, "a lone tab is the window title");
                assert_eq!(
                    segs[0].close_col, None,
                    "a title band has no close affordance; the window's own already exists"
                );
                assert_eq!(segs[1].start_col, 80 - NEW_TAB_W);
            }
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
                assert_eq!(segs[1].start_col, cap);
            }
        }
        assert_eq!(segs[1].kind, TabHit::NewTab);
        assert!(!segs[1].solo);
        assert_eq!(segs[1].end_col, segs[1].start_col + NEW_TAB_W);

        let segs = layout_segments(20, 1, 0, false);
        assert_eq!(segs[0].end_col, 20 - NEW_TAB_W);
        assert_eq!(segs[1].start_col, 20 - NEW_TAB_W);
    }

    /// The inline rename field replaces the chip's title span with the find bar's well.
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

    /// While editing a narrow chip, icon and status canvas are suppressed so the field gets the
    /// whole interior and scrolls rather than truncating.
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

    /// The `✕` is hover-revealed on quiet tabs and, on a chip-card band, resident on the selected
    /// card.
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
        assert_eq!(
            layout_segments_with_metadata(60, 2, &metadata, 1, false),
            segments,
            "selection does not move the close columns either"
        );
    }

    /// The solo band spends its width on the title.
    #[test]
    fn solo_band_compresses_the_description_before_the_title() {
        let (title, subtitle) = solo_band_text("aterm", Some("~/very/long/path/here"), 40);
        assert_eq!(title, "aterm", "a title that fits is never cut");
        assert_eq!(
            subtitle.as_deref(),
            Some("~/very/long/path/here"),
            "and a description that fits is not cut either"
        );

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

    /// Two or more tabs split the band into equal shares, each at most [`CHIP_MAX_COLS`] (no
    /// maximum on macOS), and none is `solo`.
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

    /// One cap for every count (Windows).
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

    /// Each platform's chips at 200 columns.
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

    /// The windows tab cap is in dips ([`TabSizing::at`]).
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

    /// A windows tab grows toward its title while the strip has room ([`TabSizing`],
    /// [`layout_segments_with_version_sized`]).
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

    /// The band's foot (Windows).
    #[test]
    fn a_sized_band_keeps_a_foot_of_ground_under_its_seam() {
        if !STRIP_WINDOWS_PAINT {
            assert_eq!(strip_foot_px(38, 32, 1.0), 0, "no foot off Windows");
            return;
        }
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

    /// `show_update` prepends a leading `↻` segment at col 0, shifting the tabs right.
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
        let first_tab = segs
            .iter()
            .find(|s| matches!(s.kind, TabHit::Select(_)))
            .unwrap();
        assert_eq!(
            first_tab.start_col, UPDATE_W,
            "tabs shifted right by the icon width"
        );
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
        assert_eq!(
            hit_test(&canonical, base[1].close_col.unwrap()),
            Some(TabHit::Select(1)),
            "the painted status cell selects its tab"
        );
    }

    /// Every reachable [`IconKey`] with `conn: None` matches a fresh rasterization byte for byte.
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

    /// The solo band's subtitle comes from the composed session chrome, prefers authored prose over
    /// observed status.
    #[test]
    fn solo_subtitle_prefers_authored_prose_and_never_echoes_the_title() {
        for (title, chrome, expected) in [
            (
                "aterm",
                Some("aterm\ndescription: the release cutter\nactivity: Building\ncwd: ~/aterm"),
                Some("the release cutter"),
            ),
            (
                "aterm",
                Some("aterm\ncwd: ~/aterm\nactivity: Building"),
                Some("~/aterm"),
            ),
            ("aterm", Some("aterm\nactivity: Building"), Some("Building")),
            ("aterm", Some("aterm\nstate: exited"), None),
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

    /// The load meter: a tenth of a core shows even on a machine of hundreds, quiet earns none.
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

    /// The tab's marks are tellable apart: every attention level draws its own shape.
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
            assert_eq!(hit_test(&segs, t.start_col), Some(t.kind));
        }
        assert!(
            saw_narrow,
            "the case is still reachable — a segment too narrow for a close x: {tabs:?}"
        );
    }

    /// Title truncation from either end.
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

    /// A command line keeps its file ([`command_cut`]).
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

    /// A title the SHELL already cut keeps its end cut ([`command_cut`]).
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
            ("C:\\Windows\\System32", 8, Some("System32")),
            ("~\\aterm\\crates\\aterm-cli\\src", 3, Some("src")),
            ("C:\\Windows\\System32", 7, None),
            ("~/a/verylongname", 8, None),
        ] {
            assert_eq!(
                path_tail(subject, room, &[]).as_deref(),
                expected,
                "{subject:?} in {room} cells"
            );
        }

        let gui = "~\\aterm\\crates\\aterm-gui\\src";
        let cli = "~\\aterm\\crates\\aterm-cli\\src";
        let sys = "C:\\Windows\\System32";
        assert_eq!(common_suffix_bytes(gui, cli), "i\\src".len(), "fixture");
        for (room, expected) in [
            (15, Some("…aterm-gui\\src")),
            (14, Some("…aterm-gui\\src")),
            (13, Some("…gui\\src")),
            (8, Some("…gui\\src")),
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

        for (tab, subject, room, kin, expected) in [
            (6, sys, 7, &[][..], "System…"),
            (6, sys, 4, &[], "Sys…"),
            (6, sys, 7, &[gui, cli, "C:\\Windows\\Temp"], "System…"),
            (6, sys, 3, &[], "7"),
            (6, sys, 7, &["C:\\Windows\\SystemApps"], "7"),
            (4, gui, 3, &[cli], "5"),
            (11, sys, 1, &[], "…"),
            (11, sys, 0, &[], ""),
        ] {
            assert_eq!(
                path_fallback(tab, subject, room, kin),
                expected,
                "{subject:?} in {room} cells beside {kin:?}"
            );
        }

        let subjects = ["~\\aterm", gui, cli, sys, gui, "cargo test -p crates"];
        assert_eq!(path_chip_label(1, &subjects, 7), "…gui");
        assert_eq!(path_chip_label(2, &subjects, 7), "…cli");
        assert_eq!(path_chip_label(4, &subjects, 7), "…gui");
        assert_eq!(path_chip_label(3, &subjects, 7), "System…");
        assert_eq!(path_chip_label(1, &["~\\aterm", gui, sys], 7), "…src");
        assert_eq!(
            path_chip_label(0, &["~\\aterm\\crates", "cargo test -p crates"], 7),
            "…crates"
        );
        assert_eq!(path_ordinal_label(4, &subjects, 7), "5");
        assert_eq!(path_ordinal_label(4, &subjects, 8), "5 · …gui");
        assert_eq!(path_ordinal_label(4, &subjects, 12), "5 · …gui\\src");
        assert_eq!(path_ordinal_label(11, &subjects, 1), "…");

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

    /// Whether `label` is an honest cut of the path `subject` on tab `tab`: the subject whole.
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

    /// A windowed strip of fifteen tabs, over every width and selection.
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

    /// One label pin for both band geometries.
    fn on_band<'a>(chip_card: &'a str, padless: &'a str) -> &'a str {
        if STRIP_CHIP_CARDS { chip_card } else { padless }
    }

    /// `(tab, label)` for every chip the strip seats, in strip order.
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

    /// Deep sibling titles past the chrome cap stay distinguishable.
    #[test]
    fn deep_siblings_past_the_chrome_cap_stay_tellable_apart() {
        use crate::chrome_label::{ChromeSurface, capped_label, present};
        use aterm_grapheme::GraphemeClusters as _;
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
        assert_eq!(resolved[0], "…~/aterm");
        assert_eq!(resolved[1], "…$HOME/trust");
        assert_eq!(resolved[2], "…~");
        assert_eq!(
            resolved[3],
            on_band("…/aterm/crates", "…~/aterm/crates"),
            "chip-card: a remainder one cell too wide falls back to the plain \
             tail cut; padless: the exact fit keeps the word-boundary cut, \
             `~` included"
        );

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

        let segments = layout_segments(130, suffixed.len(), 0, false);
        let labels = distinct_chip_labels(&segments, &suffixed, None, 0, None);
        let resolved: Vec<String> = labels.into_iter().map(Option::unwrap).collect();
        assert_eq!(
            resolved,
            ["…~/aterm", "…$HOME/trust", "…~", "…~/aterm/crates"],
            "every member of the shared-prompt family speaks the tail dialect"
        );

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

    /// Ten to twenty identical shells across every column range the strip lays out.
    #[test]
    fn every_strip_width_labels_a_twin_honestly() {
        let mut marks = 0usize;
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

    /// The ordinal's tail follows the survivor rule ([`furniture_survivor_recut`]).
    #[cfg(not(target_os = "macos"))]
    #[test]
    fn an_ordinal_tail_is_never_the_shared_furniture() {
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

    /// A tail one chip outside a cluster happens to share is not furniture.
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

    /// A twin family's shed ending is whole words.
    #[test]
    fn a_shed_ending_is_whole_words() {
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

    /// Whether `painted` is context for the path `subject` in [`twin_tail`]'s sense.
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

    /// Three shells in one directory with identical titles, over every title window a chip can get.
    #[test]
    fn twins_keep_whole_components_or_the_head_at_every_width() {
        let subject = "~\\aterm";
        let title = subject;
        let furniture = shared_furniture(title, &[title, title], 1);
        let expected: [(usize, &str, &str); 12] = [
            (0, "", ""),
            (1, "2", "…"),
            (2, "2", "~…"),
            (3, "2", "~\\…"),
            (4, "2", "~\\a…"),
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
        assert_eq!(family_tail(subject, "", 5), "…term");
        assert_eq!(family_tail(subject, "", 3), "…rm");
        assert_eq!(twin_tail("~\\work\\api", "", 10), "~\\work\\api");
        assert_eq!(twin_tail("~\\work\\api", "", 9), "…work\\api");
        assert_eq!(twin_tail("~\\work\\api", "", 8), "…api");
        assert_eq!(twin_tail("~\\work\\api", "", 5), "…\\api");
        assert_eq!(twin_tail("~/work/api", "", 4), "…api");
        assert_eq!(twin_tail("~/work/api", "", 3), "~/…");
        assert_eq!(twin_tail("Settings.toml", "", 6), "….toml");
        assert_eq!(twin_tail("same shell", "shell", 4), "…ell");
        let core = "user@m17-tower: ~/aterm";
        assert_eq!(twin_tail(core, "~/aterm", 10), "…~/aterm");
        assert_eq!(twin_tail(core, "~/aterm", 8), "…~/aterm");
        assert_eq!(twin_tail(core, "~/aterm", 7), "…/aterm");
        assert_eq!(twin_tail(core, "~/aterm", 6), "…aterm");
        assert_eq!(twin_tail(core, "~/aterm", 5), "ayat…");

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
                        // Distinct branch.
                        continue;
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
        assert!(cut > 50, "vacuous: only {cut} chips were cut");
    }

    /// Twins in a roomy strip both name their place.
    #[test]
    fn roomy_twins_name_their_directory_whole() {
        let dir = r"C:\Users\me\AppData\Local\Temp\wp-shells\work\sub dir".to_string();
        let titles = vec![dir.clone(), dir];
        let segments = layout_segments(100, 2, 0, false);
        let seats = seated(&segments, &titles, 0);
        assert!(seats[0].1.ends_with(r"work\sub dir"), "{seats:?}");
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

    /// [`twin_tail`]'s reach on a pressure strip.
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

    /// An out-of-range `active` is clamped by the label pass as by `layout_segments`, so the
    /// widened last chip keeps its title.
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

    /// Titles for the one-dialect test (once a cluster on a pressure strip flips to tail cuts, the
    /// cut loners flip too; a roomy strip keeps distinct heads' head cuts).
    fn one_dialect_titles() -> [&'static str; 10] {
        [
            "Settings.toml",
            "Settings.toml",
            "Settings.toml",
            "README.md",
            "Settings.toml",
            "cargo build",
            "Settings.toml",
            "Setup.sh",
            "Settings.toml",
            "Settings.toml.bak",
        ]
    }

    #[test]
    fn a_pressure_strip_speaks_one_truncation_dialect() {
        let titles: Vec<String> = one_dialect_titles().iter().map(|t| t.to_string()).collect();
        let segments = layout_segments(70, titles.len(), 9, false);
        let resolved = seated_labels(&segments, &titles, 9);
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

    /// `paint_strip` distinguishes the active tab (raised bg, full-strength fg, accent underline)
    /// from inactive ones (dimmed fg that recedes).
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
        let card0 = t0.start_col as usize + usize::from(STRIP_CHIP_CARDS);
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

    /// A wide char in a tab title paints as a real lead + continuation pair, never a `·`
    /// placeholder or an overlapping narrow cell.
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
            assert!(col <= layout.title_end);

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

    /// A wide title on a narrow segment drops a straddling wide char whole rather than bleeding
    /// over the close `✕` or the neighbouring chip.
    #[test]
    fn a_wide_char_never_straddles_the_title_boundary() {
        let theme = Theme::default();
        let segs = layout_segments(24, 2, 0, false);
        let titles = ["日本語シェルの長い名前".to_string(), "第二".to_string()];
        let mut row = vec![blank_cell(theme); 24];
        paint_strip(&mut row, &segs, &titles, None, 0, theme);
        for seg in segs.iter().filter(|s| matches!(s.kind, TabHit::Select(_))) {
            let boundary = match seg.close_col {
                Some(cx) => cx.saturating_sub(1),
                None => seg.end_col.saturating_sub(1),
            };
            assert!(
                !row[boundary as usize].wide || boundary == 0,
                "a continuation leaked onto the pad at {boundary}"
            );
        }
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

    /// Three realistic strips.
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

    /// A painted label with its furniture (a leading ordinal and the elision marks) taken off.
    fn painted_text(label: &str) -> &str {
        label
            .trim_start_matches(|c: char| c.is_ascii_digit())
            .trim_start_matches(' ')
            .trim_start_matches('\u{00b7}')
            .trim_start_matches(' ')
            .trim_matches('…')
    }

    /// Whether this chip says something of its own tab.
    fn says_its_own(label: &str, title: &str) -> bool {
        let painted = painted_text(label);
        if let Some((head, tail)) = painted.rsplit_once(MIDDLE_MARK) {
            return title.starts_with(head) && title.ends_with(tail);
        }
        !painted.is_empty() && title.contains(painted)
    }

    /// Sweep of 1 to 20 tabs, 40 to 300 columns, the selection at each end and in the middle, with
    /// and without the leading `↻`.
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
                        for pair in seated.windows(2) {
                            assert_eq!(
                                pair[1],
                                pair[0] + 1,
                                "{where_}: the window has a hole: {seated:?}"
                            );
                        }
                        if seated.len() < tabs && seated.len() > STRIP_MIN_SEATS {
                            for seg in &chips {
                                assert!(
                                    seg.end_col - seg.start_col >= STRIP_SEATS_FLOOR,
                                    "{where_}: a windowed strip seated a chip below \
                                     the floor: {chips:?}"
                                );
                            }
                        }
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

    /// The selected chip names its tab at every width: never empty, never a bare elision mark,
    /// always text of its own title.
    #[test]
    fn the_selected_chip_names_its_tab_at_every_width() {
        for (set, titles) in realistic_strips() {
            for cols in (40..=300u16).step_by(1) {
                for active in [0, titles.len() / 2, titles.len() - 1] {
                    let segs = layout_segments(cols, titles.len(), active, false);
                    let labels = distinct_chip_labels(&segs, &titles, None, active, None);
                    let Some(label) = labels[active].as_deref() else {
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

    /// The overflow mark is painted where the layout reserved it, on a column the hit map calls
    /// bare band.
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

    /// More tabs than the band can hold at a cell apiece still lay out inside the strip without
    /// panicking.
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

    /// `strip_char` blanks only controls.
    #[test]
    fn strip_char_sanitizes() {
        assert_eq!(strip_char('a'), 'a');
        assert_eq!(strip_char('\t'), ' ');
        assert_eq!(strip_char('世'), '世');
        assert_eq!(strip_char_cells('世'), 2);
        assert_eq!(strip_char('\u{1F680}'), '\u{1F680}');
        assert_eq!(strip_char_cells('\u{1F680}'), 2);
        assert_eq!(strip_char('\u{2705}'), '\u{2705}');
        assert_eq!(strip_char_cells('\u{2705}'), 2);
        assert_eq!(strip_char('\u{F0154}'), '\u{F0154}');
        assert_eq!(strip_char_cells('\u{F0154}'), 1);
        assert_eq!(strip_char_cells('\u{FE0F}'), 1);
        assert_eq!(
            strip_display_cells("日本語"),
            6,
            "display width, not char count"
        );
    }

    /// Every built-in theme keeps the strip's text above its 3.0:1 legibility floor
    /// ([`STRIP_LEGIBILITY_FLOOR`], deliberately below aa).
    #[test]
    fn strip_inks_clear_the_legibility_floor() {
        use aterm_types::Rgb;
        let rgb = |c: [u8; 3]| Rgb::new(c[0], c[1], c[2]);
        let rgb_of = crate::settings::u32_rgb;
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
                let floor = |what: &str, ink: [u8; 3], surface: [u8; 3]| {
                    let ratio = rgb(ink).contrast(rgb(surface));
                    assert!(ratio >= 3.0, "{at}: {what} contrast {ratio:.2} < 3.0:1");
                };
                floor("active-tab text", c.active_fg, c.active_bg);
                floor("inactive-tab label", c.inactive_fg, c.band_bg);
                if STRIP_IS_CHROME_BAND {
                    // likeliest to slip under the floor on a muted scheme.
                    floor("hovered-tab label", c.hover_fg, c.hover_bg);
                }
                if STRIP_CHIP_CARDS {
                    floor("quiet-chip label", c.chip_fg, c.chip_bg);
                    floor("'+' affordance", c.chip_button_fg, c.chip_bg);
                }
                assert_ne!(c.active_bg, c.band_bg, "{at}: active card is the body");
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
                floor("update rule on its card", c.update_rule, c.update_bg);
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
                    floor("hovered '+'", c.button_hover_fg, c.hover_bg);
                    assert_ne!(c.hover_bg, c.chip_bg, "{at}: hover is the quiet chip");
                    assert_ne!(c.hover_bg, c.active_bg, "{at}: hover is the selection");
                }
                let fg = [
                    ((theme.fg >> 16) & 0xff) as u8,
                    ((theme.fg >> 8) & 0xff) as u8,
                    (theme.fg & 0xff) as u8,
                ];
                assert_eq!(c.fg, fg, "{at}: the BAND is too heavy for its own ink");
            }
        }
    }

    /// Catppuccin Latte's cursor (a deepened rosewater) clears 3:1 on its background but not on the
    /// raised card.
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
        let surfaces = [c.active_bg, c.band_bg, c.hover_bg];
        if !SELECTED_TAB_IS_GROUND {
            assert!(
                contrast(raw, c.raise_bg) < 3.0,
                "negative control: Latte's raw cursor already clears 3:1 on the card"
            );
        }
        assert!(surfaces.iter().all(|on| contrast(c.accent, *on) >= 3.0));
        assert!(contrast(c.update_rule, c.update_bg) >= 3.0);
    }

    /// The High-Contrast twin of [`strip_inks_clear_the_legibility_floor`].
    #[test]
    fn strip_contrast_holds_under_forced_high_contrast_palettes() {
        use aterm_types::Rgb;
        let rgb = |c: [u8; 3]| Rgb::new(c[0], c[1], c[2]);
        let hostile = Theme {
            fg: 0x0080_8080,
            bg: 0x0080_8080,
            cursor: 0x0080_8080,
            selection: 0x0080_8080,
        };
        for (name, palette) in crate::chrome_band::hc_fixtures::STOCK {
            crate::chrome_band::hc_fixtures::with_forced(palette, || {
                let c = strip_colors(hostile);
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
                    ("version label", rest_ink, rest_on),
                    ("hovered version label", hover_ink, hover_on),
                    ("version label under attention", cta_ink, cta_on),
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
                assert_eq!(c.band_bg, palette.btn_face, "{name}: band is BTNFACE");
                assert_eq!(c.active_bg, palette.highlight, "{name}: chip is HIGHLIGHT");
                assert_eq!(
                    c.seam,
                    Some(palette.window_text),
                    "{name}: the seam is a WINDOWTEXT border, not a blend"
                );
                assert_ne!(
                    c.raise_bg, palette.highlight,
                    "{name}: the '+' must not borrow the selection surface"
                );
                assert_ne!(
                    c.hover_bg, palette.highlight,
                    "{name}: hover must not impersonate the selection"
                );
                assert_ne!(
                    c.update_bg, palette.highlight,
                    "{name}: the update CTA must not borrow the selection surface"
                );
                assert_ne!(
                    c.update_bg, c.active_bg,
                    "{name}: the update CTA must not be a copy of the selected tab"
                );
                let overridden = strip_colors_with_active(hostile, Some([0xFF, 0x00, 0xFF]));
                assert_eq!(
                    overridden.active_bg, palette.highlight,
                    "{name}: active_tab_color must not override an HC palette"
                );
            });
        }
    }

    /// The automatic selected-tab colour is what the strip paints.
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

    /// `strip_colors` is appearance-aware.
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
            assert!(
                luma(dark.active_bg) > luma(dark.band_bg),
                "dark theme active card should be brighter than the body"
            );
            assert!(
                luma(light.active_bg) < luma(light.band_bg),
                "light theme active card should be darker than the body"
            );
        }
    }

    /// Every selected tab of a light band is one shape ([`selected_tab_rim`]).
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

    const STRIP_GRID_COLS: [u16; 9] = [40, 60, 80, 100, 120, 140, 200, 240, 300];
    const STRIP_GRID_TABS: [usize; 4] = [1, 2, 8, 20];

    /// A tab wearing `busy` and nothing else, as `claude` shells mid-command do.
    fn busy_tab() -> TabStripMetadata {
        TabStripMetadata {
            busy: true,
            ..PLAIN_TAB
        }
    }

    /// The four strips rules below are asserted over.
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

    /// Selecting a chip the strip already shows produces the identical strip (same chips, columns,
    /// close columns, marks) over the whole grid.
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

    /// [`strip_window`] is a partition, over every seat and tab count.
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

    /// The floor is measured on the title, not the chip: a busy chip spends three more cells on its
    /// status canvas than an idle one.
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

    /// No seated chip is ever a bare ordinal, over the whole grid and all four strips: the layout
    /// seats fewer chips instead.
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

    /// Every strip the sweeps below lay out.
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

    /// Everything left of the label is, to the column, the label-free layout of the `cols - w`
    /// columns left of it.
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

    /// The label yields before any chip does.
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

    /// A lone chip never shrinks for the label.
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

    /// The label steps down, then hides.
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

    /// The cell lane's label (before the UI face is installed, and wherever the pixel band does not
    /// reach).
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

    /// The window buttons sit flush right and hit as themselves (lw.5).
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

    /// The desktop's button layout places them (sctk-adwaita's `button-layout`,
    /// [`crate::client_frame::ButtonLayout`]).
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

    /// A narrow header keeps its close button: minimize goes first, then maximize, and the tabs
    /// keep [`CAPTION_STRIP_FLOOR`] cells.
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

    /// The cell lane's window buttons — the frame before the UI face lands.
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

/// The in-grid needs pill.
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

    /// The strip with `pill` laid out and painted, at `cols` columns, three plain tabs.
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

    /// The columns whose painted characters spell `text`, starting at the first that does.
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

    /// Every painted piece resolves to its own act.
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
        let pill_cols: Vec<_> = segments
            .iter()
            .filter(|s| matches!(s.kind, TabHit::Pill(_)))
            .collect();
        let (from, to) = (pill_cols[0].start_col, pill_cols.last().unwrap().end_col);
        assert!((from..to).all(|col| matches!(hit_test(&segments, col), Some(TabHit::Pill(_)))));
        assert!(to - from <= 120 * 2 / 5, "{from}..{to}");
        assert!(
            segments
                .iter()
                .filter(|s| !matches!(s.kind, TabHit::Pill(_)))
                .all(|s| s.end_col <= from)
        );
    }

    /// The pill's age moves no tab.
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
        let drawn = |w: &PillWords| PillWords {
            room: w.words.clone(),
            ..w.clone()
        };
        assert_ne!(lay(&drawn(&young)), lay(&drawn(&old)));
    }

    /// The share clips the words first, then the label, never the glyph.
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

/// Who drives: the strip's team marks.
#[cfg(test)]
mod team_title_tests {
    use super::*;

    /// A worker's title opens with `↳`, a manager's ends with `⇉N`, a tab in both wears both.
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

    /// A stuck need's mark is the stop ink and a decide need's the wait ink (the border's two
    /// tones, as the native chip draws them).
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
