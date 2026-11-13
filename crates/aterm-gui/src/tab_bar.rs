// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Clickable top-of-window tab strip layout, hit testing, and pixel-band rendering.

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
/// The closed tab application and recognized program icons.
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

/// A tab's team marks indicating worker status and managed worker count.
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
    pub(crate) const fn from_presentation(
        presentation: &crate::tab_model::TabPresentation,
    ) -> Self {
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

    /// Whether this tab shows an application rather than a terminal ([`TabIconKind::surface`]).
    #[must_use]
    pub(crate) const fn is_app_tab(self) -> bool {
        matches!(
            self.icon,
            Some(kind) if matches!(kind.surface(), crate::tab_model::TabSurface::App)
        )
    }

    #[must_use]
    pub(crate) const fn has_status(self) -> bool {
        self.dirty || self.busy || matches!(self.attention, ChipLevel::Yours) || self.conn.is_some()
    }

    #[must_use]
    pub(crate) const fn status_count(self) -> usize {
        self.dirty as usize
            + self.busy as usize
            + matches!(self.attention, ChipLevel::Yours) as usize
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
            TabStatusKind::Attention => matches!(self.attention, ChipLevel::Yours),
            TabStatusKind::Connection => self.conn.is_some(),
        }
    }
}

/// Horizontal inset of a chip's load meter from each end of the pill, in points.
pub(crate) const CHIP_LOAD_INSET_PT: f32 = 10.0;
/// Distance above the pill's bottom for the load meter, in points.
pub(crate) const CHIP_LOAD_LIFT_PT: f32 = 2.5;
/// The meter's thickness, in points.
pub(crate) const CHIP_LOAD_H_PT: f32 = 1.5;

/// Compute a tab's load step from its cores-worth and the machine's capacity in cores.
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
pub(crate) const fn tab_status_mark_scale(count: usize) -> f32 {
    // At 0.75 the widest shapes (the diamond and the outbound triangle, 2.5 from
    // centre at full size) fit the 4-unit pitch, only the diamond's stroke reaching
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

const SYSTEM_STATE_ICON: &[TabIconPrimitive] = &[
    TabIconPrimitive::Line {
        from: [3.0, 13.0],
        to: [3.0, 9.5],
        width: 2.25,
    },
    TabIconPrimitive::Line {
        from: [8.0, 13.0],
        to: [8.0, 6.0],
        width: 2.25,
    },
    TabIconPrimitive::Line {
        from: [13.0, 13.0],
        to: [13.0, 3.0],
        width: 2.25,
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

const ABOUT_ICON: &[TabIconPrimitive] = &[
    TabIconPrimitive::RoundedRect {
        rect: [1.0, 1.0, 14.0, 14.0],
        radius: 7.0,
        width: 1.2,
    },
    TabIconPrimitive::Dot {
        center: [8.0, 4.75],
        radius: 0.85,
    },
    TabIconPrimitive::Line {
        from: [8.0, 7.0],
        to: [8.0, 11.5],
        width: 1.4,
    },
    TabIconPrimitive::Line {
        from: [6.75, 11.5],
        to: [9.25, 11.5],
        width: 1.2,
    },
];

const CODEX_ICON: &[TabIconPrimitive] = &[
    TabIconPrimitive::RoundedRect {
        rect: [1.5, 2.0, 13.0, 12.0],
        radius: 2.0,
        width: 1.2,
    },
    TabIconPrimitive::Line {
        from: [4.0, 5.0],
        to: [7.0, 8.0],
        width: 1.5,
    },
    TabIconPrimitive::Line {
        from: [7.0, 8.0],
        to: [4.0, 11.0],
        width: 1.5,
    },
    TabIconPrimitive::Line {
        from: [8.5, 11.0],
        to: [12.0, 11.0],
        width: 1.5,
    },
];

const WORKERS_ICON: &[TabIconPrimitive] = &[
    TabIconPrimitive::Dot {
        center: [3.0, 4.0],
        radius: 1.25,
    },
    TabIconPrimitive::Line {
        from: [6.5, 4.0],
        to: [14.0, 4.0],
        width: 1.25,
    },
    TabIconPrimitive::Dot {
        center: [3.0, 8.0],
        radius: 1.25,
    },
    TabIconPrimitive::Line {
        from: [6.5, 8.0],
        to: [14.0, 8.0],
        width: 1.25,
    },
    TabIconPrimitive::Dot {
        center: [3.0, 12.0],
        radius: 1.25,
    },
    TabIconPrimitive::Line {
        from: [6.5, 12.0],
        to: [14.0, 12.0],
        width: 1.25,
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
        TabIconKind::Mail => &[
            TabIconPrimitive::RoundedRect {
                rect: [1.5, 3.0, 13.0, 10.0],
                radius: 1.5,
                width: 1.25,
            },
            TabIconPrimitive::Line {
                from: [2.0, 4.0],
                to: [8.0, 8.0],
                width: 1.25,
            },
            TabIconPrimitive::Line {
                from: [8.0, 8.0],
                to: [14.0, 4.0],
                width: 1.25,
            },
        ],
        TabIconKind::Codex => CODEX_ICON,
        TabIconKind::About => ABOUT_ICON,
        TabIconKind::Settings => SETTINGS_ICON,
        TabIconKind::SystemState => SYSTEM_STATE_ICON,
        TabIconKind::Markdown => MARKDOWN_ICON,
        TabIconKind::Editor => EDITOR_ICON,
        TabIconKind::Recovery => RECOVERY_ICON,
        TabIconKind::Workers => WORKERS_ICON,
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

/// Whether the strip draws Windows 11 tabs rather than chips.
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

/// Tab width bounds and per-tab desired widths for the Windows tab row.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct TabSizing<'a> {
    /// The equal width in columns ([`TAB_MAX_DIP`] at the window's metrics).
    pub(crate) cap: u16,
    /// The most a tab grows to, in columns ([`TAB_GROW_DIP`]).
    pub(crate) grow: u16,
    /// Each tab's wanted width in columns, title and chrome (`pixel_band::tab_wants`).
    pub(crate) wants: &'a [u16],
}

impl<'a> TabSizing<'a> {
    /// Equal tabs at [`MAX_CHIP_COLS`] that never grow.
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

    /// Tab `i`'s wanted width clamped between `cap` and `grow`.
    fn wanted(&self, i: usize) -> u16 {
        self.wants
            .get(i)
            .copied()
            .unwrap_or(0)
            .clamp(self.cap, self.grow.max(self.cap))
    }
}

/// Compute widths for `count` tabs from `first` growing from `base` within `band` columns.
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

/// Count how many title cells a `width`-column chip wearing `chrome` keeps.
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

/// The narrowest chip this strip can seat across all of its tabs.
fn strip_seat_cols(tab_count: usize, metadata: &[TabStripMetadata]) -> u16 {
    (0..tab_count)
        .map(|i| chip_seat_cols(metadata.get(i).copied().unwrap_or(PLAIN_TAB)))
        .max()
        .unwrap_or(PREFERRED_MIN_TAB_COLS)
}

/// Lay out the tab strip across `cols` columns for `tab_count` tabs.
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

/// `layout_segments` with the seat width (`seat`, from [`strip_seat_cols`]).
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

/// Column widths for each rung of the strip's version label at this window's metrics.
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

/// Lay out the strip with fixed-width tabs.
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
    let seat = strip_seat_cols(tab_count, metadata);
    let lone = if STRIP_WINDOWS_PAINT {
        sizing.cap
    } else {
        LONE_CHIP_COLS
    };
    let version = version_rung_that_fits(cols, tab_count, seat, show_update, slot, lone);
    let laid_cols = version.map_or(cols, |(_, w)| cols - w);
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
    if let Some((rung, w)) = version {
        segments.push(TabSegment {
            start_col: laid_cols,
            end_col: laid_cols + w,
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

const CAPTION_STRIP_FLOOR: u16 = NEW_TAB_W + 3;

/// [`layout_segments_with_caption_sized`] with equal tabs.
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
        caption,
        TabSizing::FIXED,
    )
}

#[allow(
    clippy::too_many_arguments,
    reason = "the strip's layout inputs, each independent: width, tabs, chrome, \
              selection, the update alert, the version label, the window buttons \
              and the tabs' sizing"
)]
#[must_use]
pub(crate) fn layout_segments_with_caption_sized(
    cols: u16,
    tab_count: usize,
    metadata: &[TabStripMetadata],
    active: usize,
    show_update: bool,
    slot: VersionSlotCols,
    caption: CaptionSlot,
    sizing: TabSizing<'_>,
) -> Vec<TabSegment> {
    use crate::client_frame::Side;
    let caption = caption.fit(cols, CAPTION_STRIP_FLOOR);
    let lead = caption.side_cols(Side::Left);
    let inner = cols.saturating_sub(caption.cols());
    let mut segments = layout_segments_with_version_sized(
        inner,
        tab_count,
        metadata,
        active,
        show_update,
        slot,
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

/// Select the widest version-label rung that fits beside `tab_count` tabs on a `cols`-wide strip.
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

/// Map a strip click at column `col` to its [`TabHit`], or `None` for bare background.
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

/// Resolve the columns where the `‹` / `›` overflow marks paint.
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

    /// Yield the active overflow mark columns in strip order.
    pub(crate) fn columns(self) -> impl Iterator<Item = (u16, char)> {
        [
            self.left.map(|col| (col, STRIP_OVERFLOW_LEFT)),
            self.right.map(|col| (col, STRIP_OVERFLOW_RIGHT)),
        ]
        .into_iter()
        .flatten()
    }
}

/// Visual role of a strip cell in [`strip_cell`].
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

/// Whether the in-grid strip paints itself as a distinct chrome band.
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

/// Whether the strip resolves every chip's label in one pass over all tabs.
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
    app_active: AppTile,
    app_quiet: AppTile,
    app_hover: AppTile,
    app_solo: AppTile,
}

/// The tile under an application tab's glyph: what says "application" while the chip's own marks go on saying "selected" (`docs/DESIGN-app-tabs-2026-11-02.md`).
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub(crate) struct AppTile {
    pub(crate) fill: [u8; 3],
    pub(crate) glyph: [u8; 3],
}

/// The tiles a macOS strip paints, which has no [`StripColors`] of its own.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) struct AppTileInks {
    pub(crate) active: AppTile,
    pub(crate) solo: AppTile,
    pub(crate) quiet: AppTile,
}

/// How far the tile's source leans from the foreground toward the theme's hue.
pub(crate) const APP_TILE_HUE: f32 = 0.35;
/// A tile against the chip it sits on, and its glyph against the tile (WCAG 1.4.11).
pub(crate) const APP_TILE_FLOOR: f64 = 3.0;
/// A quiet tile against its ground: visible as a shape, without competing with a selected chip.
pub(crate) const APP_TILE_QUIET_FLOOR: f64 = 1.30;
/// The tile's side as a share of what it stands on: a glyph takes this much of the tile.
pub(crate) const APP_TILE_GLYPH_SHARE: f32 = 0.70;
/// The tile's corner radius as a share of its side.
pub(crate) const APP_TILE_RADIUS_SHARE: f32 = 0.25;
/// The leading mark's width and height (points) on a chip too narrow for a tile.
pub(crate) const APP_SPINE_W_PT: f32 = 2.0;
pub(crate) const APP_SPINE_H_PT: f32 = 12.0;

const QUIET_TILE_STEPS: std::ops::RangeInclusive<u8> = 11..=25;

/// The theme's near-neutral tile colour on `ground`.
fn app_tile_source(fg: [u8; 3], tint: [u8; 3], ground: [u8; 3]) -> [u8; 3] {
    crate::chrome_band::ensure_contrast(
        crate::chrome_band::mix3(fg, tint, APP_TILE_HUE),
        ground,
        APP_TILE_FLOOR,
    )
}

/// A selected or solo tile: filled, its glyph cut in the ground's own tone.
pub(crate) fn app_tile_solid(fg: [u8; 3], tint: [u8; 3], ground: [u8; 3]) -> AppTile {
    let fill = app_tile_source(fg, tint, ground);
    AppTile {
        fill,
        glyph: crate::chrome_band::ensure_contrast(ground, fill, APP_TILE_FLOOR),
    }
}

/// An unselected tile: a quiet wash of the same colour, its glyph in that colour.
pub(crate) fn app_tile_tonal(fg: [u8; 3], tint: [u8; 3], ground: [u8; 3]) -> AppTile {
    use crate::chrome_band::{contrast, ensure_contrast, mix3};
    let source = app_tile_source(fg, tint, ground);
    let at = |step: u8| mix3(ground, source, f32::from(step) * 0.02);
    let fill = QUIET_TILE_STEPS
        .map(at)
        .find(|fill| contrast(*fill, ground) >= APP_TILE_QUIET_FLOOR)
        .unwrap_or_else(|| at(*QUIET_TILE_STEPS.end()));
    AppTile {
        fill,
        glyph: ensure_contrast(source, fill, APP_TILE_FLOOR),
    }
}

/// A tile under an OS-forced palette: system colours only, ink on `ground`.
fn forced_app_tile(hc: crate::chrome_band::ForcedChrome, ground: [u8; 3]) -> AppTile {
    let fill = crate::chrome_band::forced_ink(hc.window_text, ground);
    AppTile {
        fill,
        glyph: ground,
    }
}

/// The inks of the macOS strip for `palette`, with `active_ground` under a selected chip.
pub(crate) fn native_app_tiles(
    palette: impl Into<ChromePalette>,
    active_ground: [u8; 3],
) -> AppTileInks {
    let palette = palette.into();
    let tokens = palette.tokens();
    let (fg, tint) = (tokens.fg, tokens.theme_tint);
    let bg = tokens.bg;
    AppTileInks {
        active: app_tile_solid(fg, tint, active_ground),
        solo: app_tile_solid(fg, tint, bg),
        quiet: app_tile_tonal(fg, tint, bg),
    }
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

/// Strip tones under an OS-forced chrome palette.
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
        app_active: forced_app_tile(hc, hc.highlight),
        app_quiet: forced_app_tile(hc, hc.btn_face),
        app_hover: forced_app_tile(hc, hc.window),
        app_solo: forced_app_tile(hc, hc.btn_face),
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
    const fn tone(self, ground: [u8; 3]) -> [u8; 3] {
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
        app_active: app_tile_solid(fg, tokens.theme_tint, active_bg),
        app_quiet: app_tile_tonal(fg, tokens.theme_tint, chip_bg),
        app_hover: app_tile_tonal(fg, tokens.theme_tint, hover_bg),
        app_solo: app_tile_solid(fg, tokens.theme_tint, band_bg),
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

/// Hold `mark` to at least `floor` contrast on both `card` and `band`.
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
    let tokens = p.tokens();
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
        colors.app_active = app_tile_solid(tokens.fg, tokens.theme_tint, active_bg);
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

/// Return `true` when the packed-RGB theme background is dark.
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

/// A bare strip-background [`RenderCell`], to pre-fill strip rows before `paint_strip`.
#[must_use]
pub(crate) fn blank_cell(p: impl Into<ChromePalette>) -> RenderCell {
    strip_cell(' ', &strip_colors(p), StripRole::Inactive)
}

/// Logical pixel size for chrome labels.
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

/// Logical pixel height of the band's foot under the seam on Windows.
pub(crate) const STRIP_FOOT_LOGICAL_PX: f32 = if STRIP_WINDOWS_PAINT { 6.0 } else { 0.0 };

/// Bottom foot height in device px for a `canvas_px`-tall strip targeting `target_px`.
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

/// Seal the bottom of the strip's last row against the terminal content with a hairline seam.
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

impl StripColors {
    /// The tile a chip wears in this state.
    /// A narrow chip's spine: it stands in the gutter beside the chip, so on the band.
    fn app_spine(&self, active: bool, hovered: bool) -> [u8; 3] {
        crate::chrome_band::ensure_contrast(
            app_spine_color(self.app_tile(active, hovered), active),
            self.band_bg,
            APP_TILE_FLOOR,
        )
    }

    const fn app_tile(&self, active: bool, hovered: bool) -> AppTile {
        if active {
            self.app_active
        } else if hovered {
            self.app_hover
        } else {
            self.app_quiet
        }
    }
}

/// What stands for the tile on a chip too narrow to hold one: a tile's fill is too faint at that size when quiet, so its glyph's colour.
#[must_use]
pub(crate) const fn app_spine_color(tile: AppTile, active: bool) -> [u8; 3] {
    if active { tile.fill } else { tile.glyph }
}

/// Set an app tab's title bold across the cells `from..to`, in every selection state.
fn embolden_title(row: &mut [RenderCell], from: u16, to: u16) {
    let end = usize::from(to).min(row.len());
    for cell in row.get_mut(usize::from(from)..end).unwrap_or_default() {
        cell.bold = true;
    }
}

/// Build the [`RenderCell`] for a strip cell from [`StripColors`] and [`StripRole`].
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

/// Paint the laid-out `segments` into `row`.
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

/// Per-frame hover, subtitle, and inline-rename state for painting the strip.
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

/// Inline rename editor state for the in-grid strip painter.
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

/// Return the IME cursor cell `(row, col)` for the inline rename field.
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
                            strip_cell(' ', &colors, band_role).bg,
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
                let item = metadata.and_then(|items| items.get(i)).copied();
                if item.is_some_and(TabStripMetadata::is_app_tab) {
                    embolden_title(row, start, col);
                }
                if let Some(subtitle) = subtitle {
                    col = col.saturating_add(SOLO_GAP_COLS as u16);
                    put_text(row, col, seg.end_col, &subtitle, band_role);
                }
                if let (Some(kind), true) = (item.and_then(|item| item.icon), span > 4) {
                    let tile = item
                        .is_some_and(TabStripMetadata::is_app_tab)
                        .then_some(colors.app_solo);
                    append_icon_images(&mut images, seg.start_col + 1, kind, colors.fg, tile);
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
                    let tile = item
                        .is_some_and(TabStripMetadata::is_app_tab)
                        .then_some(colors.app_tile(is_active, is_hovered));
                    append_icon_images(&mut images, icon_start, kind, color, tile);
                } else if let Some(col) = item
                    .filter(|item| item.is_app_tab() && layout.icon_start.is_none())
                    .and_then(|_| layout.title_start.checked_sub(1))
                {
                    append_spine_image(&mut images, col, colors.app_spine(is_active, is_hovered));
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
                        strip_cell(' ', &colors, tab_role).bg,
                        is_active.then_some(colors.accent),
                    );
                } else {
                    let label = labels.get_mut(i).and_then(Option::take).unwrap_or_else(|| {
                        let avail = layout.title_end.saturating_sub(layout.title_start);
                        chip_cut(raw, avail as usize)
                    });
                    let drawn_to =
                        put_text(row, layout.title_start, layout.title_end, &label, tab_role);
                    if item.is_some_and(TabStripMetadata::is_app_tab) {
                        embolden_title(row, layout.title_start, drawn_to);
                    }
                }
                if let Some(cx) = seg.close_col
                    && (paint.hovered == Some(i) || (STRIP_CHIP_CARDS && is_active))
                {
                    put(row, cx, '✕', tab_role);
                    if STRIP_CHIP_CARDS && let Some(slot) = row.get_mut(cx as usize) {
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
            TabHit::Close(_) => {}
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

/// Resolve the ink colour for a chip's status marks.
fn status_mark_ink(
    item: TabStripMetadata,
    colors: &StripColors,
    _palette: ChromePalette,
    ink: [u8; 3],
) -> [u8; 3] {
    if item.dirty { colors.accent } else { ink }
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
                primitives.push(TabIconPrimitive::RoundedRect {
                    rect: [x - 2.2 * s, 8.0 - 2.2 * s, 4.4 * s, 4.4 * s],
                    radius: 0.8 * s,
                    width: 1.15 * s,
                });
            }
            TabStatusKind::Connection => {
                let Some(role) = metadata.conn else {
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
    rasterize_glyph(primitives, color, None)
}

/// The glyph's design coordinate for a tile's: the glyph shrinks about the centre, by [`APP_TILE_GLYPH_SHARE`].
fn into_tile(design: f32) -> f32 {
    let centre = TAB_ICON_DESIGN_SIZE * 0.5;
    (design - centre) / APP_TILE_GLYPH_SHARE + centre
}

/// `primitives` in `color`, on `tile`'s fill where one is given.
fn rasterize_glyph(
    primitives: &[TabIconPrimitive],
    color: [u8; 3],
    tile: Option<[u8; 3]>,
) -> Vec<u8> {
    let side = usize::from(ICON_RASTER_SIZE);
    let samples = u32::from(ICON_SUPERSAMPLE).pow(2);
    let mut rgba = vec![0u8; side * side * 4];
    for py in 0..side {
        for px in 0..side {
            let mut covered = 0u32;
            let mut filled = 0u32;
            for sy in 0..ICON_SUPERSAMPLE {
                for sx in 0..ICON_SUPERSAMPLE {
                    let fx = (px as f32 + (f32::from(sx) + 0.5) / f32::from(ICON_SUPERSAMPLE))
                        * TAB_ICON_DESIGN_SIZE
                        / f32::from(ICON_RASTER_SIZE);
                    let fy = (py as f32 + (f32::from(sy) + 0.5) / f32::from(ICON_SUPERSAMPLE))
                        * TAB_ICON_DESIGN_SIZE
                        / f32::from(ICON_RASTER_SIZE);
                    let (gx, gy) = if tile.is_some() {
                        (into_tile(fx), into_tile(fy))
                    } else {
                        (fx, fy)
                    };
                    covered += u32::from(
                        primitives
                            .iter()
                            .any(|primitive| primitive_covers(*primitive, gx, gy)),
                    );
                    filled += u32::from(
                        rounded_rect_signed_distance(
                            fx,
                            fy,
                            [0.0, 0.0, TAB_ICON_DESIGN_SIZE, TAB_ICON_DESIGN_SIZE],
                            TAB_ICON_DESIGN_SIZE * APP_TILE_RADIUS_SHARE,
                        ) <= 0.0,
                    );
                }
            }
            let offset = (py * side + px) * 4;
            let ink = f32::from(((covered * 255 + samples / 2) / samples) as u8) / 255.0;
            let pixel = match tile {
                None => [color[0], color[1], color[2], (ink * 255.0).round() as u8],
                Some(fill) => {
                    let over = crate::chrome_band::mix3(fill, color, ink);
                    let alpha = ((filled * 255 + samples / 2) / samples) as u8;
                    [over[0], over[1], over[2], alpha]
                }
            };
            rgba[offset..offset + 4].copy_from_slice(&pixel);
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
    bytes_image(rasterize_icon(primitives, color), cols)
}

fn bytes_image(bytes: Vec<u8>, cols: u16) -> Arc<ImageData> {
    Arc::new(ImageData {
        bytes,
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
    AppTile(TabIconKind, AppTile),
    Spine([u8; 3]),
    Status {
        dirty: bool,
        busy: bool,
        attention: ChipLevel,
        color: [u8; 3],
    },
}

const ICON_MEMO_CAP: usize = 64;

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
    tile: Option<AppTile>,
) {
    let image = match tile {
        None => memoized_image(IconKey::Icon(kind, color), || {
            image_data(tab_icon_primitives(kind), color, ICON_COLS)
        }),
        Some(tile) => memoized_image(IconKey::AppTile(kind, tile), || {
            bytes_image(
                rasterize_glyph(tab_icon_primitives(kind), tile.glyph, Some(tile.fill)),
                ICON_COLS,
            )
        }),
    };
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

fn append_spine_image(images: &mut Vec<(usize, ImageRef)>, col: u16, color: [u8; 3]) {
    const SPINE: &[TabIconPrimitive] = &[TabIconPrimitive::Line {
        from: [12.0, 3.0],
        to: [12.0, 13.0],
        width: 3.0,
    }];
    let image = memoized_image(IconKey::Spine(color), || image_data(SPINE, color, 1));
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

/// Paint the inline session-rename field across `field` (a column range of `row`), inset in a
/// strip cell of `ground`: underlined in `underline` (the active tab's accent), else, where the
/// well's fill stands under 3:1 off `ground` and so does not show where the field is, in the
/// edge every faint text field draws ([`aterm_messages::ink::well_edge`]).
#[allow(
    clippy::too_many_arguments,
    reason = "the field's geometry, contents and the two tones it is drawn between"
)]
fn paint_rename_field(
    row: &mut [RenderCell],
    field: std::ops::Range<usize>,
    text: &str,
    cursor: usize,
    placeholder: &str,
    palette: ChromePalette,
    ground: [u8; 3],
    underline: Option<[u8; 3]>,
) -> Option<usize> {
    let width = field.len();
    if width == 0 || field.end > row.len() {
        return None;
    }
    let c = crate::chrome_band::band_colors(palette);
    let edge = underline.or_else(|| aterm_messages::ink::well_edge(c.field_bg, ground, c.value));
    let well = |ch: char, fg: [u8; 3], bg: [u8; 3]| {
        let mut cell = crate::chrome_band::cell(ch, fg, bg, false, false);
        if let Some(ink) = edge {
            cell.underline = UnderlineStyle::Single;
            cell.underline_color = Some(ink);
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

/// Elide the middle of a command line so its last file or path argument survives.
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
        .and_then(fits)
    else {
        return max
            .checked_sub(1)
            .and_then(fits)
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

/// Keep the tail of `title` within `max` display cells, with a leading `…` when cut.
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

/// Resolve every chip's label with each title budget scaled by `widen`.
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
    const fn find(parent: &mut [usize], mut x: usize) -> usize {
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
                label
            } else {
                ordinal_chip_label(i, &titles[i], core, remainder, avail, furniture, tail_cut)
            }
        } else if path_cut[i] {
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

/// Byte length of the longest suffix `title` shares with at least `quorum` of `others`.
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

/// Cut a twin family's subject into `room` cells without splitting path components.
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

/// Whether the tail cut `cut` (`…<text>`) of `core` starts inside a path component or word.
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

/// Whether `text` is a non-empty prefix cut of `core` ending in `…`.
fn is_head_cut(core: &str, text: &str) -> bool {
    text != core
        && text.strip_suffix('…').is_some_and(|head| {
            !head.is_empty() && !head.starts_with('…') && core.starts_with(head)
        })
}

/// Strip `shared` trailing bytes from `title` unless `shared` covers the whole title.
fn cut_core(title: &str, shared: usize) -> &str {
    let shed = if shared >= title.len() { 0 } else { shared };
    &title[..title.len() - shed]
}

/// The characters a label may be cut after without splitting a name.
const fn is_token_boundary(c: char) -> bool {
    matches!(c, '/' | '\\' | '-' | '_' | '.') || c.is_whitespace()
}

/// Narrow `shared` (a common suffix of `title`, in bytes) to start at a token boundary.
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
const fn is_path_subject(subject: &str) -> bool {
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

/// Whether `text` ends `name` at a token boundary.
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

    /// Device-pixel geometry of the strip band for one window.
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
        /// Return the selected tab index clamped into range.
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
                    let label_bold = is_active || item.is_some_and(TabStripMetadata::is_app_tab);
                    let label_face = if label_bold && design.active_semibold {
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
                    let tile = item
                        .filter(|it| it.is_app_tab())
                        .map(|_| colors.app_tile(is_active, is_hovered));
                    if let (Some(icon_start), Some(kind)) =
                        (layout.icon_start, item.and_then(|it| it.icon))
                    {
                        let centre = (f32::from(icon_start) + f32::from(ICON_COLS) * 0.5) * cw;
                        let side = ((2.0 * cw).min(band_h) * 0.85).max(6.0);
                        let (glyph_side, glyph_ink) = match tile {
                            Some(tile) => {
                                prims.push(DrawPrim::Panel {
                                    x: centre - side * 0.5,
                                    y: cy - side * 0.5,
                                    w: side,
                                    h: side,
                                    radius: side * APP_TILE_RADIUS_SHARE,
                                    fill: rgba(tile.fill, 255),
                                });
                                (side * APP_TILE_GLYPH_SHARE, tile.glyph)
                            }
                            None => (side, ink),
                        };
                        draw_icon_prims(
                            &mut prims,
                            tab_icon_primitives(kind),
                            centre,
                            cy,
                            glyph_side,
                            glyph_ink,
                        );
                    } else if tile.is_some() {
                        let h = APP_SPINE_H_PT * scale;
                        let w = (APP_SPINE_W_PT * scale).max(1.0);
                        prims.push(DrawPrim::Panel {
                            x: (f32::from(layout.title_start) * cw - w - 2.0 * scale).round(),
                            y: (cy - h * 0.5).round(),
                            w,
                            h,
                            radius: w * 0.5,
                            fill: rgba(colors.app_spine(is_active, is_hovered), 255),
                        });
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
                    let label_ground = if is_active {
                        colors.active_bg
                    } else if is_hovered {
                        colors.hover_bg
                    } else if design.quiet_cards {
                        colors.chip_bg
                    } else {
                        colors.band_bg
                    };
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
                                    .filter(|_| label_bold && wants_semibold),
                                ink,
                                label_ground,
                            );
                        }
                    } else if span_px > 1.0 && !label.trim().is_empty() {
                        let varied = if label_bold && wants_semibold {
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
                                    label_ground,
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

    /// Average title characters per terminal cell column (`cell_w` px) at `label_px`.
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

    /// Compute each tab's wanted width in columns on a `band_h`-px band.
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

    /// Label size in pixels for a `band_h`-px optical band at `scale`.
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
        /// Whether the selected label and every app tab's label are semibold.
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

    /// Resolve the content geometry of one band segment.
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
            let app = input
                .metadata
                .get(tab)
                .is_some_and(|item| item.is_app_tab());
            if (tab == active || app) && wants_semibold {
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

    /// Choose each seated tab's cell-pass label at the widest step that fits its span.
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

    /// Cell-pass resolved label and pixel span for one chip entering pixel fitting.
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

    /// Shrink `label` until it measures inside `span_px` under `measure`.
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

    /// Byte offset of a leading or post-ordinal tail-cut ellipsis in `label`.
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
        /// Device column placement (`pen + xmin`, floored).
        fn x(&self) -> i32 {
            (self.pen + self.xmin as f32).floor() as i32
        }
    }

    /// Pen position and glyph count after one char of a [`ShapedRun`].
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

    /// Parsed variable semibold instance paired with regular cmap and kerning.
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

    /// Rasterize a [`ShapedRun`] into a straight-alpha RGBA overlay.
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

    /// Advance in device px of a colour emoji in a `label_px` label.
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

    /// Fit `emoji` into a `side`-px square at `(x, cy)` as a canvas-height overlay.
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

    /// Map 16-unit icon primitives into `DrawPrim`s centred at `(cx, cy)` with side `side` px.
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
    mod tests;
}

#[cfg(test)]
#[path = "tab_bar_tests.rs"]
mod tests;

/// Who drives: the strip's team marks.
#[cfg(test)]
#[path = "tab_bar_team_title_tests.rs"]
mod team_title_tests;

/// Diagnostics do not change a tab's ink.
#[cfg(test)]
#[path = "tab_bar_status_mark_ink_tests.rs"]
mod status_mark_ink_tests;
