// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Native macOS unified titlebar tab strip, NeedsPill, and inline tab rename editor.

#[cfg(target_os = "macos")]
pub(crate) use macos::native_strip_container;
#[cfg(target_os = "macos")]
pub(crate) use macos::{
    ToolbarHandle, begin_tab_rename, can_present_tab_rename, end_tab_rename,
    install_window_toolbar, native_selected_pill_rgb, read_tab_chrome, read_tab_menus,
    reflow_window_tabs, rename_editor_edit, rename_editor_text, set_active_tab_color,
    set_chip_loads, set_strip_accent, set_strip_dark, set_window_pill, set_window_tabs,
};

#[cfg(not(target_os = "macos"))]
pub(crate) use non_macos::{
    ToolbarHandle, install_window_toolbar, native_selected_pill_rgb, read_tab_chrome,
    read_tab_menus, set_active_tab_color, set_chip_loads, set_strip_accent, set_strip_dark,
    set_window_tabs,
};

use crate::tab_bar::TabStripMetadata;

/// Whether `next` differs from `current` only by the frame of a leading busy spinner.
#[must_use]
pub(crate) fn busy_spinner_phase_only_change(current: &str, next: &str) -> bool {
    if current == next {
        return false;
    }
    matches!(
        (busy_spinner_title_parts(current), busy_spinner_title_parts(next)),
        (Some((a, left)), Some((b, right))) if a != b && left == right
    )
}

pub(crate) fn busy_spinner_title_parts(title: &str) -> Option<(char, &str)> {
    let mut chars = title.chars();
    let frame = chars.next()?;
    let suffix = chars.as_str();
    (is_busy_spinner_frame(frame) && suffix.starts_with(' ')).then_some((frame, suffix))
}

#[must_use]
const fn is_busy_spinner_frame(ch: char) -> bool {
    matches!(
        ch,
        '\u{280b}'
            | '\u{2819}'
            | '\u{2839}'
            | '\u{2838}'
            | '\u{283c}'
            | '\u{2834}'
            | '\u{2826}'
            | '\u{2827}'
            | '\u{2807}'
            | '\u{280f}'
            | '\u{25d0}'..='\u{25d5}'
    )
}

/// Format the `toolbar-tabs` introspection line.
#[must_use]
pub(crate) fn format_tab_chrome(
    titles: &[String],
    metadata: &[TabStripMetadata],
    tooltips: &[Option<String>],
    active: usize,
) -> Option<String> {
    let count = titles.len();
    if count == 0 {
        return None;
    }
    let selected = active.min(count - 1) as isize;
    let states = (0..count)
        .map(|index| {
            let mut state = Vec::with_capacity(6);
            if index == selected as usize {
                state.push("selected");
            }
            if let Some(metadata) = metadata.get(index) {
                if metadata.dirty {
                    state.push("dirty");
                }
                if metadata.busy {
                    state.push("busy");
                }
                state.extend(metadata.attention.chrome_states());
                if let Some(conn) = metadata.conn {
                    state.extend_from_slice(conn.chrome_states());
                }
            }
            state
        })
        .collect::<Vec<_>>();
    let tooltips = (0..count)
        .map(|index| tooltips.get(index).and_then(Option::as_deref))
        .collect::<Vec<_>>();
    let icons = (0..count)
        .map(|index| {
            metadata
                .get(index)
                .and_then(|item| item.icon)
                .map(crate::tab_bar::TabIconKind::semantic_name)
        })
        .collect::<Vec<_>>();
    Some(format!(
        "toolbar-tabs count={count} selected={selected} labels={titles:?} icons={icons:?} states={states:?} tooltips={tooltips:?}"
    ))
}

/// Route-aware hover/help text, augmented with state that must never be visual-only.
#[must_use]
#[cfg(any(target_os = "macos", test))]
fn tab_help(
    title: &str,
    tooltip: Option<&str>,
    metadata: TabStripMetadata,
    index: usize,
) -> String {
    let mut parts = vec![tooltip.unwrap_or(title).to_string()];
    if let Some(icon) = metadata.icon {
        let name = icon.display_name();
        if !parts[0].to_lowercase().contains(&name.to_lowercase()) {
            parts.push(name.to_string());
        }
    }
    if metadata.dirty {
        parts.push("Unsaved changes".to_string());
    }
    if metadata.busy {
        parts.push("Working".to_string());
    }
    let reason = tooltip.and_then(|tip| {
        tip.lines().find(|line| {
            line.starts_with(crate::session_chrome::NEEDS_ATTENTION_PREFIX)
                || line.starts_with(crate::session_chrome::NEEDS_YOU_PREFIX)
        })
    });
    let reason_told = match metadata.attention {
        crate::tab_bar::ChipLevel::Wait => reason.is_some(),
        crate::tab_bar::ChipLevel::Stop(aterm_messages::presence::StopCause::Frozen) => {
            reason.is_some_and(|r| r.contains("not reading input"))
        }
        crate::tab_bar::ChipLevel::Stop(aterm_messages::presence::StopCause::Suspended) => {
            reason.is_some_and(|r| r.contains("stopped with input queued"))
        }
        _ => false,
    };
    if let Some(help) = metadata.attention.help()
        && !reason_told
    {
        parts.push(help.to_string());
    }
    match metadata.conn {
        Some(crate::tab_bar::TabConnRole::Outbound) => {
            parts.push("Connects into a peer session".to_string());
        }
        Some(crate::tab_bar::TabConnRole::Inbound) => {
            parts.push("A peer session connects in".to_string());
        }
        Some(crate::tab_bar::TabConnRole::Both) => {
            parts.push("Connected both ways with peers".to_string());
        }
        None => {}
    }
    if index < 9 {
        parts.push(format!("⌘{}", index + 1));
    }
    parts.join(" · ")
}

#[must_use]
#[cfg(any(target_os = "macos", test))]
fn tab_display_label(title: &str, index: usize, available_width: f64) -> String {
    let title_width = title.chars().count() as f64 * 7.0;
    let shortcut_width = 34.0;
    if index < 9 && available_width >= title_width + shortcut_width {
        format!("{title}  \u{2318}{}", index + 1)
    } else {
        title.to_string()
    }
}

#[must_use]
#[cfg(target_os = "macos")]
fn tab_close_accessibility_label(title: &str) -> String {
    format!("Close {title} Tab")
}

#[cfg(any(target_os = "macos", test))]
const PREFERRED_MIN_TAB_WIDTH: f64 = 64.0;
#[cfg(any(target_os = "macos", test))]
const TAB_CELL_GUTTER: f64 = 1.0;

/// Lay out a native tab band without ever overlapping the trailing New Tab action.
#[must_use]
#[cfg(any(target_os = "macos", test))]
fn native_tab_cells(band_width: f64, count: usize, active: usize) -> Vec<(f64, f64)> {
    if count == 0 {
        return Vec::new();
    }
    let band_width = band_width.max(1.0);
    let ideal = band_width / count as f64;
    let active = active.min(count - 1);
    let widths = if ideal >= PREFERRED_MIN_TAB_WIDTH {
        vec![ideal; count]
    } else {
        let selected = (band_width * 0.6).min(96.0).max(ideal);
        let inactive = if count > 1 {
            (band_width - selected) / (count - 1) as f64
        } else {
            selected
        };
        (0..count)
            .map(|index| if index == active { selected } else { inactive })
            .collect()
    };
    let mut cells = Vec::with_capacity(count);
    let mut x = 0.0;
    for width in widths {
        let gutter = TAB_CELL_GUTTER.min(width * 0.2);
        cells.push((x, (width - gutter).max(f64::EPSILON)));
        x += width;
    }
    cells
}

/// Maximum width share (40%) of `band_w` that the needs pill may occupy.
#[cfg(any(target_os = "macos", test))]
fn pill_share(band_w: f64) -> f64 {
    (band_w * 0.4).floor().max(0.0)
}

/// Gap and horizontal padding (points) for the macOS titlebar pill.
#[cfg(any(target_os = "macos", test))]
const PILL_GAP: f64 = 4.0;
#[cfg(any(target_os = "macos", test))]
const PILL_PAD: f64 = 8.0;

/// Lay out the titlebar pill in at most `share` points, returning its layout and total width.
#[cfg(any(target_os = "macos", test))]
fn titlebar_pill_layout(
    words: &crate::presence::PillWords,
    share: f64,
    measure: impl Fn(&str) -> f64,
) -> (aterm_messages::presence::PillLayout, f64) {
    let inner = (share - 2.0 * PILL_PAD).max(0.0);
    let layout = aterm_messages::presence::pill_layout(words, inner, PILL_GAP, measure);
    let width = layout.width + 2.0 * PILL_PAD;
    (layout, width)
}

#[cfg(test)]
mod shared_tests {
    use super::*;

    #[test]
    fn a_pills_age_moves_no_titlebar_tab_within_its_unit() {
        use crate::presence::{NeedSeverity, PillKind, PillWords};
        let t0 = std::time::Instant::now();
        let pill = aterm_messages::presence::Pill {
            kind: PillKind::Need(NeedSeverity::Decide),
            session: 1,
            label: "fmt".into(),
            tab: Some(2),
            what: "deploy to prod?".into(),
            since: Some(t0),
            act: None,
            more: 0,
        };
        let at = |secs: u64| {
            crate::presence::pill_words(&pill, t0 + std::time::Duration::from_secs(secs))
        };
        let measure = |text: &str| text.chars().count() as f64 * 7.0;
        let band = 900.0;
        let cells = |words: &PillWords| {
            let (_, width) = titlebar_pill_layout(words, pill_share(band), measure);
            (width, native_tab_cells(band - width, 3, 0))
        };
        let (young, old) = (at(3), at(47));
        assert_ne!(young.words, old.words);
        assert_eq!(cells(&young), cells(&old), "the tabs hold still");
        let drawn = |w: &PillWords| PillWords {
            room: w.words.clone(),
            ..w.clone()
        };
        assert_ne!(cells(&drawn(&young)).0, cells(&drawn(&old)).0);
    }

    #[test]
    fn single_settings_tab_keeps_route_and_all_independent_states() {
        let settings_metadata = TabStripMetadata {
            icon: Some(crate::tab_bar::TabIconKind::Settings),
            dirty: true,
            busy: true,
            attention: crate::tab_bar::ChipLevel::Wait,
            conn: Some(crate::tab_bar::TabConnRole::Both),
            closable: true,
            team: crate::tab_bar::TabTeam::NONE,
            load: 0,
        };
        let line = format_tab_chrome(
            &["Settings".to_string()],
            &[settings_metadata],
            &[Some("Settings · Cursor & Motion".to_string())],
            0,
        )
        .expect("single tab remains title chrome");
        assert!(line.contains("count=1 selected=0"));
        assert!(line.contains(r#"labels=["Settings"]"#));
        assert!(line.contains(r#"icons=[Some("settings")]"#));
        assert!(
            line.contains(r#"["selected", "dirty", "busy", "attention", "conn-out", "conn-in"]"#)
        );
        assert!(line.contains("Settings · Cursor & Motion"));

        let help = tab_help(
            "Settings",
            Some("Settings · Cursor & Motion"),
            settings_metadata,
            1,
        );
        assert!(
            help.starts_with("Settings · Cursor & Motion · ") && help.ends_with(" · ⌘2"),
            "{help}"
        );
        assert_eq!(
            help.split(" · ").count(),
            7,
            "the tooltip, one clause per state (dirty, busy, attention, both \
             directions as one), and the shortcut: {help}"
        );
    }

    #[test]
    fn the_hover_names_the_reason_once() {
        let metadata = TabStripMetadata {
            icon: None,
            dirty: false,
            busy: false,
            attention: crate::tab_bar::ChipLevel::Wait,
            conn: None,
            closable: true,
            team: crate::tab_bar::TabTeam::NONE,
            load: 0,
        };
        let stop = |cause| TabStripMetadata {
            attention: crate::tab_bar::ChipLevel::Stop(cause),
            ..metadata
        };
        let reason = "build\nNeeds attention: upgrade stalled";
        let frozen = "build\nNeeds attention: claude is frozen: not reading input since 14:02";
        let stopped = "build\nNeeds attention: vim is stopped with input queued since 14:02";
        let harness = "build\nNeeds you: sign 0.48.0 (+1 more)\nClaude Code carries on meanwhile";
        use aterm_messages::presence::StopCause::{Frozen, Suspended, Wall};
        for (tooltip, metadata, clauses) in [
            (reason, metadata, 1),
            (harness, metadata, 1),
            ("build\ncwd: ~/src", metadata, 2),
            (frozen, stop(Frozen), 1),
            (stopped, stop(Suspended), 1),
            (reason, stop(Frozen), 2),
            (reason, stop(Wall), 2),
        ] {
            let help = tab_help("build", Some(tooltip), metadata, 0);
            assert!(help.starts_with(tooltip), "{help}");
            assert_eq!(
                help.split(" · ").count(),
                clauses + 1,
                "{tooltip:?} with {metadata:?}: {help}"
            );
        }
    }

    #[test]
    fn the_hover_names_the_app_once() {
        let app = |icon| TabStripMetadata {
            icon: Some(icon),
            dirty: false,
            busy: false,
            attention: crate::tab_bar::ChipLevel::Off,
            conn: None,
            closable: true,
            team: crate::tab_bar::TabTeam::NONE,
            load: 0,
        };
        let clauses = |title, icon| tab_help(title, None, app(icon), 0).split(" · ").count();
        assert_eq!(
            clauses("README.md", crate::tab_bar::TabIconKind::Markdown),
            3
        );
        assert_eq!(clauses("main.rs", crate::tab_bar::TabIconKind::Editor), 3);
        assert_eq!(
            tab_help(
                "Settings",
                None,
                app(crate::tab_bar::TabIconKind::Settings),
                0
            ),
            "Settings · ⌘1",
            "the title already names the app"
        );
    }

    #[test]
    fn connection_roles_state_their_direction_in_chrome_and_help() {
        let conn_only = |conn| TabStripMetadata {
            icon: None,
            dirty: false,
            busy: false,
            attention: crate::tab_bar::ChipLevel::Off,
            conn,
            closable: true,
            team: crate::tab_bar::TabTeam::NONE,
            load: 0,
        };
        let line = format_tab_chrome(
            &["push-src".to_string(), "push-dst".to_string()],
            &[
                conn_only(Some(crate::tab_bar::TabConnRole::Outbound)),
                conn_only(Some(crate::tab_bar::TabConnRole::Inbound)),
            ],
            &[None, None],
            0,
        )
        .expect("two tabs report chrome");
        assert!(line.contains(r#"["selected", "conn-out"], ["conn-in"]"#));
        let help = |role| tab_help("peer", None, conn_only(role), 0);
        let (out, inbound) = (
            help(Some(crate::tab_bar::TabConnRole::Outbound)),
            help(Some(crate::tab_bar::TabConnRole::Inbound)),
        );
        assert_eq!(out.split(" · ").count(), 3, "{out}");
        assert_eq!(inbound.split(" · ").count(), 3, "{inbound}");
        assert_ne!(out, inbound, "each direction is its own words");
        assert_eq!(help(None), "peer · ⌘1", "no connection adds nothing");
    }

    #[test]
    fn terminal_help_and_chrome_have_no_phantom_app_icon() {
        let terminal = TabStripMetadata::from_presentation(
            &crate::tab_model::TabPresentation::terminal("build-server"),
        );
        assert_eq!(terminal.icon, None);
        assert_eq!(
            tab_help("build-server", None, terminal, 0),
            "build-server · ⌘1"
        );
        let line = format_tab_chrome(&["build-server".to_string()], &[terminal], &[None], 0)
            .expect("single terminal is explicit title chrome");
        assert!(line.contains("icons=[None]"));
    }

    #[test]
    fn visible_tab_label_spends_narrow_space_on_identity() {
        assert_eq!(tab_display_label("Settings", 1, 146.0), "Settings  ⌘2");
        assert_eq!(tab_display_label("Settings", 1, 51.0), "Settings");
        assert_eq!(tab_display_label("build-server", 0, 74.0), "build-server");
    }

    #[test]
    fn busy_spinner_phase_dedup_preserves_semantic_title_changes() {
        let frames = [
            "⠋ aterm",
            "⠙ aterm",
            "⠹ aterm",
            "⠸ aterm",
            "⠼ aterm",
            "⠴ aterm",
            "⠦ aterm",
            "⠧ aterm",
            "⠇ aterm",
            "⠏ aterm",
        ];
        for pair in frames.windows(2) {
            assert!(busy_spinner_phase_only_change(pair[0], pair[1]));
        }

        let mut chrome = "aterm".to_string();
        let mut updates = 0;
        for index in 0..100 {
            let next = frames[index % frames.len()];
            if chrome != next && !busy_spinner_phase_only_change(&chrome, next) {
                chrome.clear();
                chrome.push_str(next);
                updates += 1;
            }
        }
        assert_eq!(updates, 1);
        assert_eq!(chrome, "⠋ aterm");

        assert!(!busy_spinner_phase_only_change("⠋ aterm", "⠋ aterm"));
        assert!(!busy_spinner_phase_only_change("⠋ aterm", "⠙ project"));
        assert!(!busy_spinner_phase_only_change("⠋ aterm", "aterm"));
        assert!(!busy_spinner_phase_only_change("aterm", "⠋ aterm"));
        assert!(!busy_spinner_phase_only_change("⠋aterm", "⠙aterm"));
        assert!(!busy_spinner_phase_only_change("⣿ aterm", "⠋ aterm"));
        assert!(!busy_spinner_phase_only_change("prefix ⠋", "prefix ⠙"));

        let moons = [
            "◐ Claude Code",
            "◓ Claude Code",
            "◑ Claude Code",
            "◒ Claude Code",
        ];
        for pair in moons.windows(2) {
            assert!(busy_spinner_phase_only_change(pair[0], pair[1]));
        }
        assert!(busy_spinner_phase_only_change("◔ task", "◕ task"));
        assert!(busy_spinner_phase_only_change("⠋ aterm", "◐ aterm"));
        assert!(!busy_spinner_phase_only_change(
            "◑ Claude Code",
            "✳ Claude Code"
        ));
        assert!(!busy_spinner_phase_only_change("◐ aterm", "◑ project"));
        assert!(!busy_spinner_phase_only_change("◐aterm", "◑aterm"));
    }

    #[test]
    fn native_overflow_cells_never_overlap_or_escape_the_band() {
        let band = 180.0;
        let cells = native_tab_cells(band, 12, 7);
        assert_eq!(cells.len(), 12);
        for pair in cells.windows(2) {
            assert!(pair[0].0 + pair[0].1 <= pair[1].0);
        }
        let last = cells.last().unwrap();
        assert!(last.0 + last.1 <= band);
        assert!(cells.iter().all(|(_, width)| *width > 0.0));

        assert!(
            cells[7].1 > cells[6].1,
            "active title gets overflow priority"
        );

        let phone_pair = native_tab_cells(126.0, 2, 1);
        assert!(
            phone_pair[1].1 > phone_pair[0].1,
            "a selected Settings tab wins space even in the common two-tab case"
        );
        let settings_layout =
            crate::tab_bar::native_tab_content_layout(phone_pair[1].1, 14.0, true, false);
        assert!(
            settings_layout.label[2] >= 55.0,
            "the selected phone tab preserves AppKit's measured Settings title"
        );

        let extreme = native_tab_cells(12.0, 40, 39);
        let last = extreme.last().unwrap();
        assert!(last.0 + last.1 <= 12.0 + f64::EPSILON);
    }

    #[test]
    fn wide_bands_are_split_evenly_and_spent_to_the_last_point() {
        for count in 1..=8usize {
            let band = 1180.0;
            let cells = native_tab_cells(band, count, 0);
            assert_eq!(cells.len(), count);
            let share = band / count as f64;
            for (index, &(x, width)) in cells.iter().enumerate() {
                assert!(
                    (x - share * index as f64).abs() < 1e-9,
                    "cell {index} of {count} starts on its equal share"
                );
                assert!(
                    (width - (share - TAB_CELL_GUTTER)).abs() < 1e-9,
                    "cell {index} of {count} is one equal share wide"
                );
            }
            let (last_x, last_w) = *cells.last().unwrap();
            assert!(
                band - (last_x + last_w) <= TAB_CELL_GUTTER,
                "nothing but the gutter is left over at {count} tabs"
            );
        }

        assert_eq!(native_tab_cells(1180.0, 1, 0)[0].0, 0.0);
        assert!(native_tab_cells(1180.0, 1, 0)[0].1 >= 1180.0 - TAB_CELL_GUTTER);
    }
}

/// Non-macOS in-memory tab-chrome seam serving the `chrome` verb.
#[cfg(not(target_os = "macos"))]
mod non_macos {
    use std::cell::RefCell;

    use aterm_window::event_loop::EventLoopProxy;
    use aterm_window::window::Window;

    use super::format_tab_chrome;
    use crate::session_chrome::TabChromeExt;
    use crate::{Wake, WindowId, tab_bar::TabStripMetadata};

    /// Non-macOS in-memory tab-chrome model for a window.
    #[derive(Default)]
    pub(super) struct TabChrome {
        /// The raw session titles the caller syncs, in tab order.
        titles: Vec<String>,
        /// Canonical icon/status/close metadata paired with `titles`.
        metadata: Vec<TabStripMetadata>,
        /// Full route/document context paired with `titles`, shown on hover and
        /// exposed to accessibility/introspection without bloating the paint metadata.
        tooltips: Vec<Option<String>>,
        /// Composed per-tab chrome extension (tooltip and context-menu model), paired
        /// with `titles`; [`read_tab_menus`] serves the `chrome` mirror from it so the
        /// lines match macOS.
        ext: Vec<TabChromeExt>,
        /// The 0-based index of the active tab (clamped into range on read/format).
        active: usize,
    }

    /// Non-macOS toolbar handle wrapping [`TabChrome`].
    pub(crate) struct ToolbarHandle {
        /// Updated by [`set_window_tabs`], read by [`read_tab_chrome`].
        chrome: RefCell<TabChrome>,
    }

    /// Create an empty non-macOS [`ToolbarHandle`].
    pub(crate) fn install_window_toolbar(
        window: &Window,
        _proxy: &EventLoopProxy<Wake>,
        _wid: WindowId,
    ) -> Option<ToolbarHandle> {
        crate::elevated_caption::set_title(window, "aterm");
        Some(ToolbarHandle {
            chrome: RefCell::new(TabChrome::default()),
        })
    }

    /// Update the non-macOS [`ToolbarHandle`] with the current tab set.
    #[allow(
        clippy::too_many_arguments,
        reason = "the macOS strip's signature: every per-tab vector, the active index and the window's pill"
    )]
    pub(crate) fn set_window_tabs(
        handle: &ToolbarHandle,
        titles: &[String],
        _ids: &[crate::tab_model::TabId],
        metadata: &[TabStripMetadata],
        tooltips: &[Option<String>],
        ext: &[TabChromeExt],
        active: usize,
        _pill: Option<&crate::app_presence::NeedsPillModel>,
    ) {
        let mut chrome = handle.chrome.borrow_mut();
        chrome.titles.clear();
        chrome.titles.extend_from_slice(titles);
        chrome.metadata.clear();
        chrome.metadata.extend_from_slice(metadata);
        chrome.tooltips.clear();
        chrome.tooltips.extend_from_slice(tooltips);
        chrome.ext.clear();
        chrome.ext.extend_from_slice(ext);
        chrome.active = active;
    }

    /// Format the `toolbar-tabs` introspection line from `handle`.
    #[must_use]
    pub(crate) fn read_tab_chrome(handle: &ToolbarHandle) -> Option<String> {
        let chrome = handle.chrome.borrow();
        format_tab_chrome(
            &chrome.titles,
            &chrome.metadata,
            &chrome.tooltips,
            chrome.active,
        )
    }

    /// Format per-tab context-menu introspection lines from `handle`.
    #[must_use]
    pub(crate) fn read_tab_menus(handle: &ToolbarHandle) -> Vec<String> {
        handle
            .chrome
            .borrow()
            .ext
            .iter()
            .enumerate()
            .map(|(i, e)| {
                crate::session_chrome::tab_menu_chrome_line(
                    i,
                    &e.menu,
                    crate::turn_ledger::now_ms(),
                )
            })
            .collect()
    }

    /// No-op off macOS.
    pub(crate) fn set_strip_dark(_handle: &ToolbarHandle, _dark: bool) {}

    /// Always `None` off macOS.
    pub(crate) fn native_selected_pill_rgb(_handle: &ToolbarHandle) -> Option<[u8; 3]> {
        None
    }

    /// No-op off macOS.
    pub(crate) fn set_active_tab_color(_handle: &ToolbarHandle, _color: Option<[u8; 3]>) {}

    /// No-op off macOS.
    pub(crate) fn set_strip_accent(_handle: &ToolbarHandle, _accent: Option<[u8; 3]>) {}

    /// No-op off macOS.
    pub(crate) fn set_chip_loads(_handle: &ToolbarHandle, _loads: &[u8], _inks: [[u8; 3]; 2]) {}
}

#[cfg(all(test, not(target_os = "macos")))]
mod non_macos_tests {
    use super::format_tab_chrome;
    use crate::tab_bar::TabStripMetadata;

    fn metadata(dirty: bool, busy: bool, attention: bool) -> TabStripMetadata {
        TabStripMetadata {
            icon: None,
            dirty,
            busy,
            attention: if attention {
                crate::tab_bar::ChipLevel::Wait
            } else {
                crate::tab_bar::ChipLevel::Off
            },
            conn: None,
            closable: true,
            team: crate::tab_bar::TabTeam::NONE,
            load: 0,
        }
    }

    #[test]
    fn chrome_keeps_single_tab_identity() {
        assert_eq!(format_tab_chrome(&[], &[], &[], 0), None);
        assert_eq!(
            format_tab_chrome(
                &["Settings".to_string()],
                &[metadata(false, false, false)],
                &[Some("Settings · Cursor & Motion".to_string())],
                0,
            )
            .as_deref(),
            Some(
                r#"toolbar-tabs count=1 selected=0 labels=["Settings"] icons=[None] states=[["selected"]] tooltips=[Some("Settings · Cursor & Motion")]"#
            )
        );
    }

    #[test]
    fn chrome_line_matches_macos_shape() {
        let titles = vec!["zsh".to_string(), "vim".to_string(), "htop".to_string()];
        let metadata = vec![
            metadata(false, false, false),
            metadata(true, true, true),
            metadata(false, false, false),
        ];
        assert_eq!(
            format_tab_chrome(&titles, &metadata, &vec![None; 3], 1).as_deref(),
            Some(
                r#"toolbar-tabs count=3 selected=1 labels=["zsh", "vim", "htop"] icons=[None, None, None] states=[[], ["selected", "dirty", "busy", "attention"], []] tooltips=[None, None, None]"#
            )
        );
    }

    #[test]
    fn chrome_clamps_out_of_range_active() {
        let titles = vec!["a".to_string(), "b".to_string()];
        assert_eq!(
            format_tab_chrome(
                &titles,
                &[metadata(false, false, false); 2],
                &[None, None],
                9,
            )
            .as_deref(),
            Some(
                r#"toolbar-tabs count=2 selected=1 labels=["a", "b"] icons=[None, None] states=[[], ["selected"]] tooltips=[None, None]"#
            )
        );
    }
}

#[cfg(target_os = "macos")]
mod macos {
    use std::cell::{Cell, RefCell};

    use aterm_objc::{CGPoint, CGRect, CGSize, Id, MainThread, Obj, Retained, class, sel};
    use aterm_window::event_loop::EventLoopProxy;

    use super::{
        PILL_PAD, format_tab_chrome, native_tab_cells, pill_share, tab_close_accessibility_label,
        tab_display_label, tab_help, titlebar_pill_layout,
    };
    use crate::appkit::{self, consts};
    use crate::menu::MenuAction;
    use crate::session_chrome::{ConnVerb, TabChromeExt, TabMenuEntry};
    use crate::tab_bar::{
        TAB_ICON_DESIGN_SIZE, TAB_ICON_NATIVE_SIZE, TAB_STATUS_KINDS, TabConnRole,
        TabIconPrimitive, TabStatusKind, TabStripMetadata, native_tab_content_layout,
        tab_icon_primitives, tab_status_center, tab_status_mark_scale,
    };
    use crate::tab_model::TabId;
    use crate::{TabAction, Wake, WindowId};

    /// Height (points) of the single chrome row.
    const STRIP_HEIGHT: f64 = 28.0;
    const STRIP_LEADING_PAD: f64 = 4.0;
    const LIGHTS_CLEARANCE: f64 = 10.0;
    const PLUS_WIDTH: f64 = 28.0;
    const TRAILING_PAD: f64 = 8.0;
    const TAB_GAP: f64 = 6.0;
    const BTN_INSET_X: f64 = 2.0;
    const BTN_PILL_HEIGHT: f64 = 20.0;
    const BTN_RADIUS: f64 = 6.0;
    const PLUS_ARM: f64 = 5.5;
    const PLUS_STROKE: f64 = 1.6;
    const STRIP_MIN_WIDTH: f64 = 120.0;
    const STRIP_MAX_WIDTH: f64 = 100_000.0;
    const TAB_PILL_INSET_X: f64 = 2.0;
    const TAB_PILL_HEIGHT: f64 = 22.0;
    const TAB_PILL_RADIUS: f64 = 7.0;
    const TAB_RULE_HEIGHT: f64 = 14.0;
    const SOLO_EDITOR_WIDTH: f64 = 280.0;
    const SOLO_GAP: f64 = 10.0;
    const SOLO_EDGE_PAD: f64 = 12.0;
    const SOLO_LABEL_HEIGHT: f64 = 18.0;
    const SOLO_STATUS_SIZE: f64 = 16.0;

    /// Resolved placement of a tab chip in the strip.
    #[derive(Clone, Copy, PartialEq, Debug)]
    struct TabGeometry {
        /// The measured content centre line, in the chip's own coordinates.
        center_y: f64,
        /// Exactly one tab in this window: render as the window title, not a chip.
        solo: bool,
        /// The window's horizontal centre, in the chip's own coordinates (solo only).
        solo_center_x: f64,
        /// Draw the leading-edge hairline. False for the first chip and for chips
        /// touching the selected pill, which draws its own edge.
        separator: bool,
    }

    impl TabGeometry {
        /// Whether the chip at `index` draws a leading divider when `active` is selected.
        #[must_use]
        const fn separates(index: usize, active: usize) -> bool {
            index > 0 && index != active && index != active + 1
        }
    }

    /// `[x, y, w, h]` (the point-space layout arrays) as an AppKit rect.
    const fn rect_of([x, y, w, h]: [f64; 4]) -> CGRect {
        rect(x, y, w.max(0.0), h.max(0.0))
    }

    /// `CGRect` from four scalars — the shape `NSMakeRect` makes.
    const fn rect(x: f64, y: f64, w: f64, h: f64) -> CGRect {
        CGRect {
            origin: CGPoint { x, y },
            size: CGSize {
                width: w,
                height: h,
            },
        }
    }

    /// `CGPoint` from two scalars — the shape `NSMakePoint` makes.
    const fn point(x: f64, y: f64) -> CGPoint {
        CGPoint { x, y }
    }

    /// Call a nullary `NSColor` class factory selector.
    /// # Safety
    /// `name` must be a no-argument `+(NSColor *)` class method of `NSColor`.
    unsafe fn ns_color(name: aterm_objc::Sel) -> Id {
        // SAFETY: the caller pins the selector to a nullary class factory.
        unsafe { appkit::send_id(class(c"NSColor").as_id(), name) }
    }

    /// Construct an sRGB `NSColor`.
    /// # Safety
    /// Must be called on the main thread.
    unsafe fn srgb(r: f64, g: f64, b: f64, a: f64) -> Id {
        // SAFETY: the documented non-raising `+(NSColor *)(CGFloat × 4)`.
        unsafe {
            appkit::send_id_f64_f64_f64_f64(
                class(c"NSColor").as_id(),
                sel!(colorWithSRGBRed:green:blue:alpha:),
                r,
                g,
                b,
                a,
            )
        }
    }

    /// Return `color` with alpha component `alpha`.
    /// # Safety
    /// `color` must be a live `NSColor`.
    unsafe fn with_alpha(color: Id, alpha: f64) -> Id {
        // SAFETY: `-(NSColor *)(CGFloat)`, non-raising.
        unsafe { appkit::send_id_f64(color, sel!(colorWithAlphaComponent:), alpha) }
    }

    /// Construct an empty `NSBezierPath`.
    /// # Safety
    /// Must be called on the main thread inside a draw cycle.
    unsafe fn bezier_path() -> Id {
        // SAFETY: the documented non-raising `+(NSBezierPath *)` factory.
        unsafe { appkit::send_id(class(c"NSBezierPath").as_id(), sel!(bezierPath)) }
    }

    /// Construct a rounded-rect `NSBezierPath`.
    /// # Safety
    /// Must be called on the main thread inside a draw cycle.
    unsafe fn rounded_rect(r: CGRect, radius: f64) -> Id {
        // SAFETY: `+(NSBezierPath *)(NSRect, CGFloat, CGFloat)`, non-raising.
        unsafe {
            appkit::send_id_rect_f64_f64(
                class(c"NSBezierPath").as_id(),
                sel!(bezierPathWithRoundedRect:xRadius:yRadius:),
                r,
                radius,
                radius,
            )
        }
    }

    /// Construct a circular `NSBezierPath` centered at `(x, y)`.
    /// # Safety
    /// Must be called on the main thread inside a draw cycle.
    unsafe fn circle(x: f64, y: f64, radius: f64) -> Id {
        // SAFETY: `+(NSBezierPath *)(NSRect)`, non-raising.
        unsafe {
            appkit::send_id_rect(
                class(c"NSBezierPath").as_id(),
                sel!(bezierPathWithOvalInRect:),
                rect(x - radius, y - radius, radius * 2.0, radius * 2.0),
            )
        }
    }

    /// Construct a closed polygon `NSBezierPath` through `points`.
    /// # Safety
    /// Must be called on the main thread inside a draw cycle.
    unsafe fn polygon(points: &[CGPoint]) -> Id {
        // SAFETY: a fresh path, then `-moveToPoint:`/`-lineToPoint:` (`-(void)(NSPoint)`) and `-closePath` (`-(void)`), none of which raise.
        unsafe {
            let path = bezier_path();
            for (i, &p) in points.iter().enumerate() {
                let verb = if i == 0 {
                    sel!(moveToPoint:)
                } else {
                    sel!(lineToPoint:)
                };
                appkit::send_v_point(path, verb, p);
            }
            appkit::send_v(path, sel!(closePath));
            path
        }
    }

    /// Return the regular or bold system `NSFont` of `size`.
    /// # Safety
    /// Must be called on the main thread.
    unsafe fn system_font(size: f64, bold: bool) -> Id {
        let name = if bold {
            sel!(boldSystemFontOfSize:)
        } else {
            sel!(systemFontOfSize:)
        };
        // SAFETY: both are `+(NSFont *)(CGFloat)`, non-raising.
        unsafe { appkit::send_id_f64(class(c"NSFont").as_id(), name, size) }
    }

    /// Return the regular-weight monospaced-digit system `NSFont` of `size`.
    /// # Safety
    /// Must be called on the main thread.
    unsafe fn monospaced_digit_font(size: f64) -> Id {
        const REGULAR: f64 = 0.0;
        // SAFETY: `+(NSFont *)(CGFloat, NSFontWeight)`, non-raising; `NSFontWeight` is a `CGFloat`.
        unsafe {
            appkit::send_id_f64_f64(
                class(c"NSFont").as_id(),
                sel!(monospacedDigitSystemFontOfSize:weight:),
                size,
                REGULAR,
            )
        }
    }

    /// Set tail-truncation line-break mode on `field`'s cell.
    /// # Safety
    /// `field` must be a live `NSTextField`.
    unsafe fn truncate_tail(field: Id) {
        // SAFETY: `-cell` is a main-thread `-(id)` getter; every `NSTextField` cell is
        // an `NSCell`, which responds to `-setLineBreakMode:` (`NSUInteger`). A nil
        // cell is inert.
        unsafe {
            let cell = appkit::send_id(field, sel!(cell));
            if !cell.is_null() {
                appkit::send_v_usize(
                    cell,
                    sel!(setLineBreakMode:),
                    consts::NS_LINE_BREAK_BY_TRUNCATING_TAIL,
                );
            }
        }
    }

    /// Strip geometry measured from the live window in container coordinates.
    #[derive(Clone, Copy, Debug)]
    struct StripMetrics {
        /// Vertical centre of the strip's content: the traffic lights' centre line in
        /// container coordinates, or the container's midpoint when unreadable.
        center_y: f64,
        /// Trailing edge of the rightmost visible traffic light in container
        /// coordinates (usually negative); `f64::NEG_INFINITY` when unmeasured, so the
        /// consuming `max` falls back to the plain leading pad.
        lights_right: f64,
        /// Horizontal centre of the window in container coordinates; the solo title
        /// centres on it rather than on the band.
        window_center_x: f64,
    }

    /// [`plan_strip`]'s output: chip cells and strip measurements.
    struct StripPlan {
        cells: Vec<(f64, f64)>,
        metrics: StripMetrics,
        strip_h: f64,
    }

    impl StripMetrics {
        /// Content geometry of chip `index` of `n` starting at `cell_x`.
        fn geometry(&self, index: usize, n: usize, active: usize, cell_x: f64) -> TabGeometry {
            TabGeometry {
                center_y: self.center_y,
                solo: n == 1,
                solo_center_x: self.window_center_x - cell_x,
                separator: TabGeometry::separates(index, active),
            }
        }

        /// Fallback geometry when `container` has no reachable window.
        /// # Safety
        /// `container` must be a live `NSView`.
        unsafe fn unmeasured(container: Id) -> Self {
            // SAFETY: `-bounds` is a side-effect-free `-(NSRect)` getter.
            let bounds = unsafe { appkit::send_rect(container, sel!(bounds)) };
            Self {
                center_y: bounds.origin.y + bounds.size.height * 0.5,
                lights_right: f64::NEG_INFINITY,
                window_center_x: bounds.origin.x + bounds.size.width * 0.5,
            }
        }
    }

    /// Measure the window's traffic lights in `container` coordinates.
    /// # Safety
    /// `container` must be a live `NSView`.
    unsafe fn strip_metrics(container: Id) -> StripMetrics {
        // SAFETY: the caller guarantees a live view.
        let mut metrics = unsafe { StripMetrics::unmeasured(container) };
        // SAFETY: side-effect-free main-thread AppKit getters on live objects (`-window`, `-superview`, `-contentView`, `-standardWindowButton:`, `-isHidden`, `-frame`, `-bounds`, `-convertRect:fromView:`). Each nil is checked; a nil source view means window coordinates.
        unsafe {
            let window = appkit::send_id(container, sel!(window));
            if window.is_null() {
                return metrics;
            }
            for button in [
                consts::NS_WINDOW_CLOSE_BUTTON,
                consts::NS_WINDOW_MINIATURIZE_BUTTON,
                consts::NS_WINDOW_ZOOM_BUTTON,
            ] {
                let light = appkit::send_id_usize(window, sel!(standardWindowButton:), button);
                if light.is_null() || appkit::send_bool(light, sel!(isHidden)) {
                    continue;
                }
                let superview = appkit::send_id(light, sel!(superview));
                let rect = appkit::send_rect_rect_id(
                    container,
                    sel!(convertRect:fromView:),
                    appkit::send_rect(light, sel!(frame)),
                    superview,
                );
                if !(rect.size.width.is_finite() && rect.size.height > 0.0) {
                    continue;
                }
                if button == consts::NS_WINDOW_CLOSE_BUTTON {
                    metrics.center_y = rect.origin.y + rect.size.height * 0.5;
                }
                metrics.lights_right = metrics.lights_right.max(rect.origin.x + rect.size.width);
            }
            let content = appkit::send_id(window, sel!(contentView));
            if !content.is_null() {
                let rect = appkit::send_rect_rect_id(
                    container,
                    sel!(convertRect:fromView:),
                    appkit::send_rect(content, sel!(bounds)),
                    content,
                );
                if rect.size.width.is_finite() && rect.size.width > 1.0 {
                    metrics.window_center_x = rect.origin.x + rect.size.width * 0.5;
                }
            }
        }
        // SAFETY: as above.
        let bounds = unsafe { appkit::send_rect(container, sel!(bounds)) };
        let lo = bounds.origin.y + TAB_PILL_HEIGHT * 0.5;
        let hi = bounds.origin.y + bounds.size.height - TAB_PILL_HEIGHT * 0.5;
        if lo <= hi {
            metrics.center_y = metrics.center_y.clamp(lo, hi);
        }
        metrics
    }

    /// Retained AppKit objects backing a window's native titlebar tab strip.
    pub(crate) struct ToolbarHandle {
        /// The `NSToolbarDelegate` that vends the strip's single custom-view item.
        _delegate: aterm_objc::Retained<ToolbarDelegate>,
        /// The `NSToolbar` hosting the strip item, always visible. Retain-only (the
        /// window owns its live toolbar strongly).
        _toolbar: Obj,
        /// The container `NSView` holding the [`TabView`]s and the "+" [`ChromeButton`],
        /// retained for [`set_window_tabs`] rebuilds and [`read_tab_chrome`].
        container: Obj,
        /// Proxy and window id for building each rebuilt [`TabView`]'s relays.
        proxy: EventLoopProxy<Wake>,
        window: WindowId,
        /// The live [`TabView`]s in tab order: the source for [`read_tab_chrome`] and
        /// the set [`set_window_tabs`] replaces on rebuild.
        tabs: RefCell<Vec<aterm_objc::Retained<TabView>>>,
        /// The trailing "+" New Tab [`ChromeButton`], re-pinned to the live width and
        /// measured centre line by every [`set_window_tabs`].
        plus: aterm_objc::Retained<ChromeButton>,
        /// winit's content `NSView`, the permanent first responder. [`end_tab_rename`]
        /// hands focus back to it; asking the container's window later is wrong in
        /// native fullscreen, where AppKit re-hosts the toolbar in a separate window.
        content_view: Obj,
        /// The live inline session-rename editor, or `None`. Owned by the handle rather
        /// than a chip because a rebuild (tab count or width change) destroys every
        /// chip; the field is repositioned across a rebuild instead.
        rename: RefCell<Option<RenameEditor>>,
        /// The NEEDS PILL ([`NeedsPill`], §5): built on the first lit pill and
        /// kept, hidden while the window's border is unlit.
        pill: RefCell<Option<Retained<NeedsPill>>>,
        /// The pill's laid-out width (points, its pads included) at the last
        /// [`plan_strip`]: what it takes from the tab band.
        pill_w: Cell<f64>,
        /// Where the last [`plan_strip`] laid the pill out (its share, its
        /// right edge, the strip's height and centre line): what a pill-only
        /// push ([`set_window_pill`]) lays it out at again.
        pill_slot: Cell<Option<PillSlot>>,
    }

    /// Where the strip lays its pill out ([`ToolbarHandle::pill_slot`]).
    #[derive(Clone, Copy, Debug)]
    struct PillSlot {
        share: f64,
        right: f64,
        strip_h: f64,
        center_y: f64,
    }

    /// Live inline rename editor overlaying one tab chip.
    struct RenameEditor {
        /// The editable field. Its `stringValue` is the only home of the in-progress
        /// text; nothing else needs saving across a strip rebuild.
        field: Obj,
        /// The delegate/relay. Retained here because AppKit holds a delegate weakly.
        target: aterm_objc::Retained<TabRenameTarget>,
        /// Stable id of the tab the editor is painted over, for positioning only. When
        /// it stops resolving the edit is cancelled.
        tab: TabId,
    }

    /// Return the retained custom-view container placed in the unified titlebar.
    pub(crate) fn native_strip_container(handle: &ToolbarHandle) -> Obj {
        handle.container.clone_retained()
    }

    /// Per-button state for [`ChromeButton`].
    pub(crate) struct ChromeIvars {
        /// The `Wake` channel a click relays through.
        proxy: EventLoopProxy<Wake>,
        /// The [`MenuAction`] this button fires (File ▸ New Tab for "+"), the same
        /// action the menu item and keybinding use.
        action: MenuAction,
        /// Whether the pointer is inside the button (drives the hover highlight).
        hovered: Cell<bool>,
        /// Whether the mouse is held down inside the button (drives the pressed state).
        pressed: Cell<bool>,
        /// The strip's measured content centre line in this button's coordinates, so
        /// the "+" sits on the traffic lights' optical row.
        center_y: Cell<f64>,
        /// The retained tracking area, so `updateTrackingAreas` can swap it on a frame
        /// change without leaking the old one.
        tracking: RefCell<Option<Obj>>,
    }

    aterm_objc::declare_class! {
        /// Custom `NSView` drawing the titlebar "+" New Tab button.
        pub(crate) struct ChromeButton: NSView {
            const NAME: &str = "ATermChromeButton";
            type Ivars = ChromeIvars;
            protocols: [NSObject];
            /// Paint the affordance, then the "+": nothing at rest, a translucent
            /// rounded pill on hover, a stronger one while pressed.
            @sel(drawRect:)
            fn drawRect(&self, _dirty: CGRect) {
                // SAFETY: `-bounds` is a side-effect-free `-(NSRect)` getter on this live view, inside AppKit's own draw cycle.
                let bounds = unsafe { appkit::send_rect(self.as_id(), sel!(bounds)) };
                let ivars = self.ivars();
                // SAFETY: AppKit drawing primitives on the main thread inside a draw cycle; colors
                // are autoreleased (+0) and used in this frame; none raise.
                unsafe {
                    if ivars.pressed.get() || ivars.hovered.get() {
                        let pill = rect(
                            bounds.origin.x + BTN_INSET_X,
                            ivars.center_y.get() - BTN_PILL_HEIGHT * 0.5,
                            (bounds.size.width - 2.0 * BTN_INSET_X).max(0.0),
                            BTN_PILL_HEIGHT,
                        );
                        let a = if ivars.pressed.get() { 0.16 } else { 0.09 };
                        appkit::send_v(srgb(1.0, 1.0, 1.0, a), sel!(set));
                        appkit::send_v(rounded_rect(pill, BTN_RADIUS), sel!(fill));
                    }
                    let ink = if ivars.hovered.get() {
                        sel!(labelColor)
                    } else {
                        sel!(secondaryLabelColor)
                    };
                    appkit::send_v(ns_color(ink), sel!(set));
                    let cx = bounds.origin.x + bounds.size.width * 0.5;
                    let cy = ivars.center_y.get();
                    let arm = PLUS_ARM;
                    let mark = bezier_path();
                    appkit::send_v_f64(mark, sel!(setLineWidth:), PLUS_STROKE);
                    appkit::send_v_usize(mark, sel!(setLineCapStyle:), consts::NS_LINE_CAP_STYLE_ROUND);
                    appkit::send_v_point(mark, sel!(moveToPoint:), point(cx - arm, cy));
                    appkit::send_v_point(mark, sel!(lineToPoint:), point(cx + arm, cy));
                    appkit::send_v_point(mark, sel!(moveToPoint:), point(cx, cy - arm));
                    appkit::send_v_point(mark, sel!(lineToPoint:), point(cx, cy + arm));
                    appkit::send_v(mark, sel!(stroke));
                }
            }

            /// Press begins: latch the pressed state and repaint (shows the pressed fill).
            @sel(mouseDown:)
            fn mouseDown(&self, _event: Id) {
                self.ivars().pressed.set(true);
                mark_dirty(self.as_id());
            }

            /// Drag while pressed: the pressed highlight holds only while the pointer
            /// stays inside (dragging off cancels the press, as native buttons do).
            @sel(mouseDragged:)
            fn mouseDragged(&self, event: Id) {
                let inside = self.point_inside(event);
                if self.ivars().pressed.get() != inside {
                    self.ivars().pressed.set(inside);
                    mark_dirty(self.as_id());
                }
            }

            /// Release: fire the action only if the press began and ended inside the
            /// button, then clear the pressed state.
            @sel(mouseUp:)
            fn mouseUp(&self, event: Id) {
                let was_pressed = self.ivars().pressed.get();
                self.ivars().pressed.set(false);
                mark_dirty(self.as_id());
                if was_pressed && self.point_inside(event) {
                    let action = self.ivars().action;
                    let _ = crate::tab_spawn::post_native_menu_action(&self.ivars().proxy, action);
                }
            }

            /// Pointer entered — show the hover highlight and brighten the "+" mark.
            @sel(mouseEntered:)
            fn mouseEntered(&self, _event: Id) {
                self.ivars().hovered.set(true);
                mark_dirty(self.as_id());
            }

            /// Pointer left — drop the hover highlight and dim the "+" mark back.
            @sel(mouseExited:)
            fn mouseExited(&self, _event: Id) {
                self.ivars().hovered.set(false);
                mark_dirty(self.as_id());
            }

            /// Rebuild the tracking area whenever geometry changes (the "+" is re-pinned on
            /// resize), so hover detection follows.
            @sel(updateTrackingAreas)
            fn updateTrackingAreas(&self) {
                update_tracking_areas(self.super_receiver(), &self.ivars().tracking);
            }

            /// Accept the first click in a non-key window, so the "+" works in one
            /// click.
            @sel(acceptsFirstMouse:) @abort_on_exception
            fn acceptsFirstMouse(&self, _event: Id) -> aterm_objc::Bool {
                aterm_objc::Bool::YES
            }

            /// VoiceOver's press action dispatches the exact same typed menu command as
            /// a pointer click.
            @sel(accessibilityPerformPress)
            fn accessibilityPerformPress(&self) -> aterm_objc::Bool {
                let _ = crate::tab_spawn::post_native_menu_action(
                    &self.ivars().proxy,
                    self.ivars().action,
                );
                aterm_objc::Bool::YES
            }
        }

    }

    impl ChromeButton {
        /// Build a titlebar "+" button of `frame`, relaying `action` through `proxy`.
        fn build(
            mtm: MainThread,
            proxy: EventLoopProxy<Wake>,
            action: MenuAction,
            accessibility_label: &str,
            frame: CGRect,
        ) -> Option<Retained<Self>> {
            let ivars = ChromeIvars {
                proxy,
                action,
                hovered: Cell::new(false),
                pressed: Cell::new(false),
                center_y: Cell::new(frame.size.height * 0.5),
                tracking: RefCell::new(None),
            };
            // SAFETY: `alloc_ivars` returns a +1 instance owing exactly one designated
            // initializer, which is the next send. `-initWithFrame:` is `-(id)(NSRect)`,
            // consumes that +1 and returns the initialised +1 that `Retained::from_owned` adopts.
            let this: Retained<Self> = unsafe {
                let raw = Self::alloc_ivars(mtm, ivars);
                if raw.is_null() {
                    return None;
                }
                Retained::from_owned(appkit::send_id_rect(raw, sel!(initWithFrame:), frame))?
            };

            // SAFETY: `-setAccessibilityLabel:` is a plain `-(void)(NSString *)` main-thread metadata write on the fresh view.
            unsafe {
                let name = appkit::nsstring(accessibility_label)?;
                make_accessibility_element(this.as_id(), "AXButton")?;
                appkit::send_v_id(this.as_id(), sel!(setAccessibilityLabel:), name.id());
            }
            install_tracking_area(this.as_id(), &this.ivars().tracking);
            Some(this)
        }

        /// Move this button onto the strip's measured content line.
        fn set_center_y(&self, center_y: f64) {
            if self.ivars().center_y.get() == center_y {
                return;
            }
            self.ivars().center_y.set(center_y);
            mark_dirty(self.as_id());
        }

        /// Whether `event`'s location is within the button's bounds.
        fn point_inside(&self, event: Id) -> bool {
            if event.is_null() {
                return false;
            }
            // SAFETY: `event` is the live NSEvent AppKit passed; `-bounds` is a
            // side-effect-free `-(NSRect)` getter on this live view.
            let (p, b) = unsafe {
                (
                    point_in_view(self.as_id(), event),
                    appkit::send_rect(self.as_id(), sel!(bounds)),
                )
            };
            p.x >= 0.0 && p.x <= b.size.width && p.y >= 0.0 && p.y <= b.size.height
        }
    }

    /// Per-view state for [`NeedsPill`].
    pub(crate) struct PillIvars {
        /// The `Wake` channel a click relays through.
        proxy: EventLoopProxy<Wake>,
        /// The window the pill belongs to.
        window: WindowId,
        /// The session the drawn pill names: a click carries it, so a click
        /// on a pill that has since moved acts on nothing (`App::on_pill_act`).
        session: Cell<u64>,
        /// What lit it, for its tone; `None` while hidden.
        kind: Cell<Option<crate::presence::PillKind>>,
        /// The drawn words, for the next layout and the accessibility label.
        words: RefCell<Option<crate::presence::PillWords>>,
        /// The pieces as laid out (`pill_layout`, in points from the pad).
        layout: Cell<Option<aterm_messages::presence::PillLayout>>,
        /// The mouse is held down inside the pill.
        pressed: Cell<bool>,
        /// The glyph, label, words, act and `+N` labels, in that order.
        fields: RefCell<Vec<Obj>>,
    }

    aterm_objc::declare_class! {
        /// Titlebar view displaying the window's projected pill.
        pub(crate) struct NeedsPill: NSView {
            const NAME: &str = "ATermNeedsPill";
            type Ivars = PillIvars;
            protocols: [NSObject];
            /// The fill: the tone at a low alpha, a stronger one while pressed.
            @sel(drawRect:)
            fn drawRect(&self, _dirty: CGRect) {
                let ivars = self.ivars();
                let Some(kind) = ivars.kind.get() else {
                    return;
                };
                // SAFETY: `-bounds` is a side-effect-free `-(NSRect)` getter on this live view, inside AppKit's own draw cycle; the colours are autoreleased (+0) and used in this frame; none raise.
                unsafe {
                    let bounds = appkit::send_rect(self.as_id(), sel!(bounds));
                    let pill = rect(
                        bounds.origin.x,
                        bounds.origin.y + (bounds.size.height - TAB_PILL_HEIGHT) * 0.5,
                        bounds.size.width,
                        TAB_PILL_HEIGHT,
                    );
                    let alpha = if ivars.pressed.get() { 0.30 } else { 0.16 };
                    appkit::send_v(with_alpha(ns_color(tone_of(kind)), alpha), sel!(set));
                    appkit::send_v(rounded_rect(pill, TAB_PILL_RADIUS), sel!(fill));
                }
            }

            /// Press begins: latch the pressed fill.
            @sel(mouseDown:)
            fn mouseDown(&self, _event: Id) {
                self.ivars().pressed.set(true);
                mark_dirty(self.as_id());
            }

            /// Release inside the pill: resolve the region through the drawn
            /// layout and post its act.
            @sel(mouseUp:)
            fn mouseUp(&self, event: Id) {
                let was_pressed = self.ivars().pressed.get();
                self.ivars().pressed.set(false);
                mark_dirty(self.as_id());
                if !was_pressed || event.is_null() {
                    return;
                }
                // SAFETY: `event` is the live NSEvent AppKit passed.
                let p = unsafe { point_in_view(self.as_id(), event) };
                let hit = self
                    .ivars()
                    .layout
                    .get()
                    .and_then(|layout| layout.hit(p.x - PILL_PAD));
                if let Some(hit) = hit {
                    self.post(hit);
                }
            }

            /// Accept the first click in a non-key window, so the pill works
            /// in one click from a window behind.
            @sel(acceptsFirstMouse:) @abort_on_exception
            fn acceptsFirstMouse(&self, _event: Id) -> aterm_objc::Bool {
                aterm_objc::Bool::YES
            }

            /// VoiceOver's press goes to the tab, the body's act.
            @sel(accessibilityPerformPress)
            fn accessibilityPerformPress(&self) -> aterm_objc::Bool {
                self.post(crate::presence::PillHit::Go);
                aterm_objc::Bool::YES
            }
        }
    }

    /// Return the `NSColor` selector for `kind`.
    fn tone_of(kind: crate::presence::PillKind) -> aterm_objc::Sel {
        match kind {
            crate::presence::PillKind::Need(crate::presence::NeedSeverity::Stuck) => {
                sel!(systemRedColor)
            }
            crate::presence::PillKind::Need(crate::presence::NeedSeverity::Decide) => {
                sel!(systemOrangeColor)
            }
            crate::presence::PillKind::Yours => sel!(secondaryLabelColor),
        }
    }

    impl NeedsPill {
        /// Build a hidden pill for `window` with its five labels.
        fn build(
            mtm: MainThread,
            proxy: EventLoopProxy<Wake>,
            window: WindowId,
        ) -> Option<Retained<Self>> {
            let ivars = PillIvars {
                proxy,
                window,
                session: Cell::new(0),
                kind: Cell::new(None),
                words: RefCell::new(None),
                layout: Cell::new(None),
                pressed: Cell::new(false),
                fields: RefCell::new(Vec::new()),
            };
            // SAFETY: as `ChromeButton::build`: `alloc_ivars` returns a +1 instance owing one designated initializer, the next send.
            let this: Retained<Self> = unsafe {
                let raw = Self::alloc_ivars(mtm, ivars);
                if raw.is_null() {
                    return None;
                }
                Retained::from_owned(appkit::send_id_rect(
                    raw,
                    sel!(initWithFrame:),
                    rect(0.0, 0.0, 0.0, STRIP_HEIGHT),
                ))?
            };
            let mut fields = Vec::with_capacity(5);
            for (index, bold) in [true, false, false, true, false].into_iter().enumerate() {
                // SAFETY: `labelWithString:` is the documented non-raising
                // factory (+0, retained here); plain setters follow, on the
                // main thread; the labels pass the mouse through to the pill.
                let field = unsafe {
                    let field = Obj::retain(appkit::send_id_id(
                        class(c"NSTextField").as_id(),
                        sel!(labelWithString:),
                        appkit::nsstring("")?.id(),
                    ))?;
                    appkit::send_v_bool(field.id(), sel!(setDrawsBackground:), false);
                    appkit::send_v_bool(field.id(), sel!(setBezeled:), false);
                    appkit::send_v_bool(field.id(), sel!(setEditable:), false);
                    appkit::send_v_bool(field.id(), sel!(setSelectable:), false);
                    appkit::send_v_bool(field.id(), sel!(setUsesSingleLineMode:), true);
                    let font = if index == 2 {
                        monospaced_digit_font(12.0)
                    } else {
                        system_font(12.0, bold)
                    };
                    appkit::send_v_id(field.id(), sel!(setFont:), font);
                    truncate_tail(field.id());
                    appkit::send_v_id(this.as_id(), sel!(addSubview:), field.id());
                    field
                };
                fields.push(field);
            }
            *this.ivars().fields.borrow_mut() = fields;
            // SAFETY: plain main-thread metadata writes on the fresh view.
            unsafe {
                make_accessibility_element(this.as_id(), "AXButton")?;
                appkit::send_v_bool(this.as_id(), sel!(setHidden:), true);
            }
            Some(this)
        }

        /// Post the click's act for the session the drawn pill names.
        fn post(&self, hit: crate::presence::PillHit) {
            let ivars = self.ivars();
            if ivars.kind.get().is_none() {
                return;
            }
            let _ = ivars.proxy.send_event(Wake::PillAct {
                window: ivars.window,
                session: ivars.session.get(),
                hit,
            });
        }

        /// Show `model` or hide on `None`.
        fn set_model(&self, model: Option<&crate::app_presence::NeedsPillModel>) {
            let ivars = self.ivars();
            let Some(model) = model else {
                ivars.kind.set(None);
                *ivars.words.borrow_mut() = None;
                ivars.layout.set(None);
                // SAFETY: plain main-thread setters on the live view;
                // `-setToolTip:` takes nil to clear the tip.
                unsafe {
                    appkit::send_v_id(self.as_id(), sel!(setToolTip:), Id::NIL);
                    appkit::send_v_bool(self.as_id(), sel!(setHidden:), true);
                }
                return;
            };
            ivars.kind.set(Some(model.kind));
            ivars.session.set(model.session);
            *ivars.words.borrow_mut() = Some(model.words.clone());
            let words = &model.words;
            let mut glyph = [0u8; 4];
            let texts: [&str; 5] = [
                words.glyph.encode_utf8(&mut glyph),
                &words.label,
                &words.words,
                words.act.map_or("", |a| a.word()),
                words.more.as_deref().unwrap_or(""),
            ];
            let fields = ivars.fields.borrow();
            // SAFETY: main-thread setters on the live retained labels; the
            // colours are +0 and retained by `-setTextColor:`.
            unsafe {
                let inks = [
                    ns_color(tone_of(model.kind)),
                    ns_color(sel!(labelColor)),
                    ns_color(sel!(labelColor)),
                    ns_color(sel!(controlAccentColor)),
                    ns_color(sel!(secondaryLabelColor)),
                ];
                for ((field, text), ink) in fields.iter().zip(texts).zip(inks) {
                    if let Some(ns) = appkit::nsstring(text) {
                        appkit::send_v_id(field.id(), sel!(setStringValue:), ns.id());
                    }
                    appkit::send_v_id(field.id(), sel!(setTextColor:), ink);
                }
                if let Some(label) = appkit::nsstring(&words.text()) {
                    appkit::send_v_id(self.as_id(), sel!(setAccessibilityLabel:), label.id());
                    appkit::send_v_id(self.as_id(), sel!(setToolTip:), label.id());
                }
                appkit::send_v_bool(self.as_id(), sel!(setHidden:), false);
            }
            mark_dirty(self.as_id());
        }

        /// Lay out the shown pill in at most `max_w` points with its right edge at `right`.
        fn lay_out(&self, max_w: f64, right: f64, strip_h: f64, center_y: f64) -> f64 {
            let ivars = self.ivars();
            let words = ivars.words.borrow();
            let Some(words) = words.as_ref() else {
                return 0.0;
            };
            let fields = ivars.fields.borrow();
            // SAFETY: `-fittingSize` is a side-effect-free `-(NSSize)` getter.
            let natural: Vec<f64> = fields
                .iter()
                .map(|field| unsafe { appkit::send_size(field.id(), sel!(fittingSize)).width })
                .collect();
            let mut glyph = [0u8; 4];
            let pieces: [&str; 5] = [
                words.glyph.encode_utf8(&mut glyph),
                &words.label,
                &words.words,
                words.act.map_or("", |a| a.word()),
                words.more.as_deref().unwrap_or(""),
            ];
            let room_w = if words.room == words.words {
                natural.get(2).copied().unwrap_or(0.0)
            } else {
                fields.get(2).map_or(0.0, |field| {
                    // SAFETY: main-thread setters and a side-effect-free `-fittingSize` getter on the live retained label; its text is put back before the frame is drawn.
                    unsafe {
                        let set = |text: &str| {
                            if let Some(ns) = appkit::nsstring(text) {
                                appkit::send_v_id(field.id(), sel!(setStringValue:), ns.id());
                            }
                        };
                        set(&words.room);
                        let w = appkit::send_size(field.id(), sel!(fittingSize)).width;
                        set(&words.words);
                        w
                    }
                })
            };
            let measure = |text: &str| {
                if text == words.room {
                    return room_w;
                }
                pieces
                    .iter()
                    .position(|piece| *piece == text)
                    .and_then(|i| natural.get(i).copied())
                    .unwrap_or(0.0)
            };
            let (layout, width) = titlebar_pill_layout(words, max_w, measure);
            ivars.layout.set(Some(layout));
            let spans = [
                Some(layout.glyph),
                Some(layout.label),
                Some(layout.words),
                layout.act.map(|(_, span)| span),
                layout.more,
            ];
            let field_h = 16.0;
            let y = center_y - field_h * 0.5;
            // SAFETY: main-thread geometry setters on the live retained view and labels.
            unsafe {
                appkit::send_v_rect(
                    self.as_id(),
                    sel!(setFrame:),
                    rect(right - width, 0.0, width, strip_h),
                );
                for (field, span) in fields.iter().zip(spans) {
                    let span = span.filter(|span| span.w > 0.0);
                    appkit::send_v_bool(field.id(), sel!(setHidden:), span.is_none());
                    if let Some(span) = span {
                        appkit::send_v_rect(
                            field.id(),
                            sel!(setFrame:),
                            rect(PILL_PAD + span.x, y, span.w, field_h),
                        );
                    }
                }
            }
            mark_dirty(self.as_id());
            width
        }
    }

    /// Sync the window's pill model onto its view.
    fn sync_pill(
        handle: &ToolbarHandle,
        mtm: MainThread,
        model: Option<&crate::app_presence::NeedsPillModel>,
    ) {
        if model.is_none() && handle.pill.borrow().is_none() {
            return;
        }
        if handle.pill.borrow().is_none() {
            let Some(pill) = NeedsPill::build(mtm, handle.proxy.clone(), handle.window) else {
                return;
            };
            // SAFETY: `addSubview:` on the live container, main thread.
            unsafe {
                appkit::send_v_id(handle.container.id(), sel!(addSubview:), pill.as_id());
            }
            *handle.pill.borrow_mut() = Some(pill);
        }
        if let Some(pill) = handle.pill.borrow().as_ref() {
            pill.set_model(model);
        }
    }

    /// Lay out the window's pill at `slot`, returning its width.
    fn place_pill(handle: &ToolbarHandle, slot: PillSlot) -> f64 {
        handle.pill.borrow().as_ref().map_or(0.0, |pill| {
            pill.lay_out(slot.share, slot.right, slot.strip_h, slot.center_y)
        })
    }

    /// Update the window's [`NeedsPill`] and re-layout if its width changed.
    pub(crate) fn set_window_pill(
        handle: &ToolbarHandle,
        pill: Option<&crate::app_presence::NeedsPillModel>,
    ) -> bool {
        let Some(mtm) = MainThread::new() else {
            return true;
        };
        let Some(slot) = handle.pill_slot.get() else {
            return false;
        };
        let was = handle.pill_w.get();
        if pill.is_some() != (was > 0.0) {
            return false;
        }
        sync_pill(handle, mtm, pill);
        place_pill(handle, slot) == was
    }

    /// Request a repaint of the whole view (after a hover / press / state change).
    fn mark_dirty(view: Id) {
        // SAFETY: side-effect-free invalidation request on the live view, main thread.
        unsafe { appkit::send_v_bool(view, sel!(setNeedsDisplay:), true) };
    }

    /// Configure `view` as an enabled accessibility element with `role`.
    /// # Safety
    /// `view` must be a live `NSView` on the main thread.
    unsafe fn make_accessibility_element(view: Id, role: &str) -> Option<()> {
        let role = appkit::nsstring(role)?;
        // SAFETY: NSView implements the NSAccessibility setters on every supported
        // macOS version; `-setAccessibilityElement:`/`-setAccessibilityEnabled:` are
        // `-(void)(BOOL)` and `-setAccessibilityRole:` is `-(void)(NSString *)`.
        unsafe {
            appkit::send_v_bool(view, sel!(setAccessibilityElement:), true);
            appkit::send_v_id(view, sel!(setAccessibilityRole:), role.id());
            appkit::send_v_bool(view, sel!(setAccessibilityEnabled:), true);
        }
        Some(())
    }

    /// Forward `updateTrackingAreas` to `super` and reinstall the hover tracking area.
    fn update_tracking_areas(sup: aterm_objc::ObjcSuper, tracking: &RefCell<Option<Obj>>) {
        // SAFETY: `[super updateTrackingAreas]` is `-(void)`; `super_receiver()` pairs
        // the instance with the declaring class, so lookup starts at `NSView`.
        unsafe {
            let f: unsafe extern "C-unwind" fn(*const aterm_objc::ObjcSuper, aterm_objc::Sel) =
                aterm_objc::msg_super();
            f(&raw const sup, sel!(updateTrackingAreas));
        }
        install_tracking_area(sup.receiver, tracking);
    }

    /// Reinstall a `mouseEnteredAndExited` tracking area covering `view`.
    fn install_tracking_area(view: Id, tracking: &RefCell<Option<Obj>>) {
        // SAFETY: main-thread AppKit calls: `-removeTrackingArea:`/`-addTrackingArea:` are `-(void)(id)`; `-initWithRect:options:owner:userInfo:` consumes the +1 from `+alloc`, which `Obj::from_owned` adopts. AppKit holds the area's owner weakly, so the ivar retains the area.
        unsafe {
            if let Some(old) = tracking.borrow_mut().take() {
                appkit::send_v_id(view, sel!(removeTrackingArea:), old.id());
            }
            let area = Obj::from_owned(appkit::send_id_rect_usize_id_id(
                appkit::alloc(class(c"NSTrackingArea")),
                sel!(initWithRect:options:owner:userInfo:),
                appkit::send_rect(view, sel!(bounds)),
                consts::NS_TRACKING_HOVER_IN_VISIBLE_RECT,
                view,
                Id::NIL,
            ));
            if let Some(area) = area {
                appkit::send_v_id(view, sel!(addTrackingArea:), area.id());
                *tracking.borrow_mut() = Some(area);
            }
        }
    }

    /// Convert `event`'s window location into `view`'s coordinate space.
    /// # Safety
    /// `view` and `event` must be live AppKit objects on the main thread.
    unsafe fn point_in_view(view: Id, event: Id) -> CGPoint {
        // SAFETY: `-locationInWindow` is a side-effect-free `-(NSPoint)`; a nil
        // source view to `-convertPoint:fromView:` means window coordinates.
        unsafe {
            let win_pt = appkit::send_point(event, sel!(locationInWindow));
            appkit::send_point_point_id(view, sel!(convertPoint:fromView:), win_pt, Id::NIL)
        }
    }

    /// Per-tab state for [`TabView`].
    pub(crate) struct TabIvars {
        /// The `Wake` channel clicks and drags relay through.
        proxy: EventLoopProxy<Wake>,
        /// The window this tab belongs to, so a relayed `Wake` addresses the right
        /// window (a click on a non-frontmost window's strip acts on that window).
        window: WindowId,
        /// This tab's 0-based index for the select/close/move relays, set at build.
        index: Cell<usize>,
        /// This tab's stable [`crate::tab_model::TabId`]. The diff path re-stamps it
        /// per position after a reorder; `TabView::show_context_menu` snapshots it
        /// into `menu_tab`.
        tab_id: Cell<TabId>,
        /// The identity captured when the context menu popped, which
        /// `tabMenuAction:` posts.
        menu_tab: Cell<Option<TabId>>,
        /// Total tab count at build time, so a drag can clamp the destination index.
        count: Cell<usize>,
        /// Whether this is the active tab: drives the accent in `drawRect:`.
        active: Cell<bool>,
        /// The canonical icon/status/close facts, pushed whole by `set_metadata`:
        /// `icon` (`None` paints no icon); status marks `dirty` (accent dot), `busy`
        /// (hollow ring), the `attention` level (orange diamond hollow to wait, filled
        /// at a stop, violet dot for a story) and `conn` (design §4: ▲ outbound,
        /// ▽ inbound, hourglass both); `closable` (a non-closable tab keeps the close
        /// geometry but never reveals or dispatches it).
        metadata: Cell<TabStripMetadata>,
        /// Whether the pointer is inside this tab, which reveals its ×. Toggled by
        /// the tracking-area enter/exit.
        hovered: Cell<bool>,
        /// The mouse-down location in the tab's own coordinates, kept so `mouseDragged:`
        /// can measure horizontal travel and decide a reorder direction.
        press_x: Cell<f64>,
        /// Whether the current gesture has already fired a reorder: at most one
        /// `Move` per press, so a long drag does not stutter through swaps.
        dragged: Cell<bool>,
        /// Solo mode: this is the window's only tab, drawn as the window title (no
        /// pill, keyline or ✕; description beside the title).
        solo: Cell<bool>,
        /// The strip's measured content centre line in this view's coordinates; every
        /// vertical slot derives from it.
        center_y: Cell<f64>,
        /// Solo mode: the horizontal centre the title group aligns to (the window's
        /// centre, not the band's), in view coordinates.
        solo_center_x: Cell<f64>,
        /// Draw the leading-edge divider ([`TabGeometry::separates`]), which tells
        /// the eye that equal full-width titles are separate tabs.
        separator: Cell<bool>,
        /// Whether the geometry reserves a close slot
        /// ([`crate::tab_bar::NativeTabContentLayout::close_available`]), cached by
        /// `TabView::relayout` so a hover reveal is a single `setHidden:`.
        close_available: Cell<bool>,
        /// Where `TabView::paint_identity` draws the app icon and status canvas, set
        /// by `TabView::relayout`.
        icon_rect: Cell<Option<[f64; 4]>>,
        status_rect: Cell<Option<[f64; 4]>>,
        /// The retained close `NSButton` and title `NSTextField`, so the view can
        /// show/hide the × on hover and so the handle keeps them alive.
        close_btn: RefCell<Option<Obj>>,
        label: RefCell<Option<Obj>>,
        /// Solo mode: the description `NSTextField` beside the title (hidden when
        /// tabbed), showing [`crate::tab_bar::solo_subtitle`] of the session chrome.
        desc_label: RefCell<Option<Obj>>,
        /// The retained tracking area, so `updateTrackingAreas` can swap it on a frame
        /// change without leaking the old one.
        tracking: RefCell<Option<Obj>>,
        /// Canonical undecorated tab title for accessibility and introspection.
        title: RefCell<String>,
        /// The tab's hover: the strip's own tooltip vector (`App::tab_tooltips`, the
        /// composed session chrome plus its presence lines: the need, who drives it,
        /// its agent's state, who last typed into it), set by [`TabView::set_context`]
        /// and at build.
        tooltip: RefCell<Option<String>>,
        /// The composed context-menu model (`session_chrome::compose_tab_menu`),
        /// rendered as an `NSMenu` per pop and read back by [`read_tab_menus`] for the
        /// `chrome` mirror.
        menu_entries: RefCell<Vec<TabMenuEntry>>,
        /// The connection rows of the open menu's pop-time snapshot, in order; a
        /// `tabMenuConnection:` click decodes its `tag` as an index into this.
        menu_conn: RefCell<Vec<(aterm_session::SessionId, ConnVerb)>>,
        /// The agent-upgrade rows of the same pop-time snapshot (session, build, word);
        /// a `tabMenuUpgrade:` click decodes its `tag` against it.
        menu_upgrade: RefCell<Vec<(String, String, aterm_messages::UpgradeWord)>>,
    }

    aterm_objc::declare_class! {
        /// Custom `NSView` rendering a single tab chip or solo window title.
        pub(crate) struct TabView: NSView {
            const NAME: &str = "ATermTabView";
            type Ivars = TabIvars;
            protocols: [NSObject];
            /// Paint the tab background, border, accent keyline, and identity marks.
            @sel(drawRect:)
            fn drawRect(&self, _dirty: CGRect) {
                // SAFETY: `-bounds` is a side-effect-free `-(NSRect)` getter on this live view, inside AppKit's own draw cycle.
                let bounds = unsafe { appkit::send_rect(self.as_id(), sel!(bounds)) };
                let ivars = self.ivars();
                if ivars.solo.get() {
                    self.paint_identity();
                    return;
                }
                let pill = rect(
                    bounds.origin.x + TAB_PILL_INSET_X,
                    ivars.center_y.get() - TAB_PILL_HEIGHT * 0.5,
                    (bounds.size.width - 2.0 * TAB_PILL_INSET_X).max(0.0),
                    TAB_PILL_HEIGHT,
                );
                // SAFETY: AppKit drawing primitives on the main thread inside a draw
                // cycle; colors are autoreleased (+0) and used in this frame; none raise.
                unsafe {
                    if ivars.active.get() {
                        let panel = match active_tab_color_override() {
                            Some([r, g, b]) => srgb(
                                f64::from(r) / 255.0,
                                f64::from(g) / 255.0,
                                f64::from(b) / 255.0,
                                1.0,
                            ),
                            None => with_alpha(
                                ns_color(sel!(selectedContentBackgroundColor)),
                                f64::from(crate::tab_bar::NATIVE_PILL_AUTO_ALPHA),
                            ),
                        };
                        appkit::send_v(panel, sel!(set));
                        let path = rounded_rect(pill, TAB_PILL_RADIUS);
                        appkit::send_v(path, sel!(fill));
                        appkit::send_v(ns_color(sel!(separatorColor)), sel!(set));
                        appkit::send_v_f64(path, sel!(setLineWidth:), 0.75);
                        appkit::send_v(path, sel!(stroke));

                        let keyline = rect(
                            pill.origin.x + 8.0,
                            pill.origin.y,
                            (pill.size.width - 16.0).max(0.0),
                            2.0,
                        );
                        appkit::send_v(strip_accent_color(), sel!(set));
                        appkit::send_v(rounded_rect(keyline, 1.0), sel!(fill));
                    } else if ivars.hovered.get() {
                        appkit::send_v(with_alpha(ns_color(sel!(labelColor)), 0.07), sel!(set));
                        appkit::send_v(rounded_rect(pill, TAB_PILL_RADIUS), sel!(fill));
                    } else if ivars.separator.get() {
                        appkit::send_v(ns_color(sel!(separatorColor)), sel!(set));
                        let rule = rect(
                            bounds.origin.x,
                            ivars.center_y.get() - TAB_RULE_HEIGHT * 0.5,
                            1.0,
                            TAB_RULE_HEIGHT,
                        );
                        appkit::send_v(
                            appkit::send_id_rect(
                                class(c"NSBezierPath").as_id(),
                                sel!(bezierPathWithRect:),
                                rule,
                            ),
                            sel!(fill),
                        );
                    }
                }
                if let Some(len) = crate::tab_bar::chip_load_len_pt(
                    pill.size.width as f32,
                    ivars.metadata.get().load,
                ) {
                    let h = f64::from(crate::tab_bar::CHIP_LOAD_H_PT);
                    let bar = rect(
                        pill.origin.x + f64::from(crate::tab_bar::CHIP_LOAD_INSET_PT),
                        pill.origin.y + f64::from(crate::tab_bar::CHIP_LOAD_LIFT_PT),
                        f64::from(len),
                        h,
                    );
                    // SAFETY: AppKit drawing primitives on the main thread inside
                    // this draw cycle; the colour is +0 and used in this frame.
                    unsafe {
                        appkit::send_v(strip_load_color(ivars.active.get()), sel!(set));
                        appkit::send_v(rounded_rect(bar, h * 0.5), sel!(fill));
                    }
                }
                self.paint_identity();
            }

            /// Select the tab, open the context menu, or begin rename/drag on press.
            @sel(mouseDown:)
            fn mouseDown(&self, event: Id) {
                // SAFETY: `-modifierFlags` is a side-effect-free `NSUInteger` getter on the live event, on the main thread.
                let ctrl = unsafe { appkit::send_usize(event, sel!(modifierFlags)) }
                    & consts::NS_EVENT_MODIFIER_FLAG_CONTROL
                    != 0;
                if ctrl {
                    self.show_context_menu(event);
                    return;
                }
                // SAFETY: `clickCount` is a side-effect-free getter on the live event, on the main thread.
                if unsafe { appkit::send_isize(event, sel!(clickCount)) } == 2 {
                    let ivars = self.ivars();
                    ivars.dragged.set(true);
                    let _ = ivars.proxy.send_event(Wake::BeginSessionRename {
                        window: ivars.window,
                        tab: ivars.tab_id.get(),
                    });
                    return;
                }
                let ivars = self.ivars();
                // SAFETY: `event` is the live event AppKit passed; a nil source view
                // means window coordinates.
                let p = unsafe { point_in_view(self.as_id(), event) };
                ivars.press_x.set(p.x);
                ivars.dragged.set(false);
                let _ = ivars.proxy.send_event(Wake::SelectTab {
                    window: ivars.window,
                    index: ivars.index.get(),
                });
            }

            /// Pop the session context menu at the pointer on right-click.
            @sel(rightMouseDown:)
            fn rightMouseDown(&self, event: Id) {
                self.show_context_menu(event);
            }

            /// Post a tab-move event once per press when dragged past half a tab width.
            @sel(mouseDragged:)
            fn mouseDragged(&self, event: Id) {
                let ivars = self.ivars();
                if ivars.dragged.get() {
                    return;
                }
                // SAFETY: side-effect-free geometry getters, main thread.
                let (p, width) = unsafe {
                    (
                        point_in_view(self.as_id(), event),
                        appkit::send_rect(self.as_id(), sel!(bounds))
                            .size
                            .width
                            .max(1.0),
                    )
                };
                let dx = p.x - ivars.press_x.get();
                if dx.abs() < width * 0.5 {
                    return;
                }
                let from = ivars.index.get();
                let count = ivars.count.get();
                let to = if dx > 0.0 {
                    (from + 1).min(count.saturating_sub(1))
                } else {
                    from.saturating_sub(1)
                };
                if to == from {
                    return;
                }
                ivars.dragged.set(true);
                let (tx, _rx) = std::sync::mpsc::channel();
                let _ = ivars.proxy.send_event(Wake::TabCmd {
                    action: TabAction::Move { from, to },
                    by: crate::session_store::ExitActor::Human,
                    reply: tx,
                });
            }

            /// Consume mouse-up without forwarding up the responder chain.
            @sel(mouseUp:)
            fn mouseUp(&self, _event: Id) {}

            /// Reveal the close button and repaint the hover highlight on enter.
            @sel(mouseEntered:)
            fn mouseEntered(&self, _event: Id) {
                self.ivars().hovered.set(true);
                self.refresh_close_visibility();
                mark_dirty(self.as_id());
            }

            /// Hide the close button and repaint on pointer exit.
            @sel(mouseExited:)
            fn mouseExited(&self, _event: Id) {
                self.ivars().hovered.set(false);
                self.refresh_close_visibility();
                mark_dirty(self.as_id());
            }

            /// Rebuild the tracking area when the view's geometry changes.
            @sel(updateTrackingAreas)
            fn updateTrackingAreas(&self) {
                update_tracking_areas(self.super_receiver(), &self.ivars().tracking);
            }

            /// Accept the first click in a non-key window.
            @sel(acceptsFirstMouse:) @abort_on_exception
            fn acceptsFirstMouse(&self, _event: Id) -> aterm_objc::Bool {
                aterm_objc::Bool::YES
            }

            /// Select this tab on VoiceOver press.
            @sel(accessibilityPerformPress)
            fn accessibilityPerformPress(&self) -> aterm_objc::Bool {
                let ivars = self.ivars();
                let _ = ivars.proxy.send_event(Wake::SelectTab {
                    window: ivars.window,
                    index: ivars.index.get(),
                });
                aterm_objc::Bool::YES
            }

            /// Post `Wake::CloseTab` when the close button is clicked.
            @sel(closeTab:)
            fn close_tab(&self, _sender: Id) {
                let ivars = self.ivars();
                if !ivars.metadata.get().closable {
                    return;
                }
                let _ = ivars.proxy.send_event(Wake::CloseTab {
                    window: ivars.window,
                    index: ivars.index.get(),
                });
            }

            /// Dispatch a context-menu action decoded from the sender's tag.
            @sel(tabMenuAction:)
            fn tab_menu_action(&self, sender: Id) {
                if sender.is_null() {
                    return;
                }
                // SAFETY: `sender` is the live NSMenuItem AppKit passed as the action sender;
                // `-tag` is `-(NSInteger)`, side-effect free.
                let tag = unsafe { appkit::send_isize(sender, sel!(tag)) };
                if let Some(action) = MenuAction::from_tag(tag) {
                    let ivars = self.ivars();
                    let Some(tab) = ivars.menu_tab.get() else { return };
                    let _ = ivars.proxy.send_event(Wake::TabMenuAction {
                        window: ivars.window,
                        tab,
                        action,
                    });
                }
            }

            /// Dispatch a per-peer connection action indexed by the sender's tag.
            @sel(tabMenuConnection:)
            fn tab_menu_connection(&self, sender: Id) {
                if sender.is_null() {
                    return;
                }
                // SAFETY: `sender` is the live NSMenuItem AppKit passed as the
                // action sender; `-tag` is `-(NSInteger)`, side-effect free.
                let tag = unsafe { appkit::send_isize(sender, sel!(tag)) };
                let ivars = self.ivars();
                let Ok(i) = usize::try_from(tag) else { return };
                let row = ivars.menu_conn.borrow().get(i).cloned();
                let Some((peer_sid, verb)) = row else { return };
                let Some(tab) = ivars.menu_tab.get() else { return };
                let _ = ivars.proxy.send_event(Wake::TabMenuConnection {
                    window: ivars.window,
                    tab,
                    peer_sid,
                    verb,
                });
            }

            /// Dispatch an agent-upgrade action indexed by the sender's tag.
            @sel(tabMenuUpgrade:)
            fn tab_menu_upgrade(&self, sender: Id) {
                if sender.is_null() {
                    return;
                }
                // SAFETY: `sender` is the live NSMenuItem AppKit passed as the action sender;
                // `-tag` is `-(NSInteger)`, side-effect free.
                let tag = unsafe { appkit::send_isize(sender, sel!(tag)) };
                let ivars = self.ivars();
                let Ok(i) = usize::try_from(tag) else { return };
                let row = ivars.menu_upgrade.borrow().get(i).cloned();
                let Some((sid, to, word)) = row else { return };
                let Some(tab) = ivars.menu_tab.get() else { return };
                let _ = ivars.proxy.send_event(Wake::TabMenuUpgrade {
                    window: ivars.window,
                    tab,
                    sid,
                    to,
                    word,
                });
            }
        }
    }

    impl TabView {
        /// Paint the shared icon IR and status marks.
        fn paint_identity(&self) {
            let ivars = self.ivars();
            let metadata = self.metadata();
            let layout_icon = ivars.icon_rect.get();
            let layout_status = ivars.status_rect.get();
            // SAFETY: all operations are standard AppKit drawing calls made from `drawRect:` on the main thread with an active graphics context.
            unsafe {
                let ink = if ivars.active.get() || ivars.solo.get() {
                    ns_color(sel!(labelColor))
                } else {
                    ns_color(sel!(secondaryLabelColor))
                };
                appkit::send_v(ink, sel!(set));
                if let (Some(kind), Some(icon)) = (metadata.icon, layout_icon) {
                    let scale = (icon[2] / f64::from(TAB_ICON_DESIGN_SIZE))
                        .min(icon[3] / f64::from(TAB_ICON_DESIGN_SIZE));
                    let ox = icon[0] + (icon[2] - f64::from(TAB_ICON_DESIGN_SIZE) * scale) * 0.5;
                    let oy = icon[1] + (icon[3] - f64::from(TAB_ICON_DESIGN_SIZE) * scale) * 0.5;
                    let at = |p: [f32; 2]| {
                        point(
                            ox + f64::from(p[0]) * scale,
                            oy + (f64::from(TAB_ICON_DESIGN_SIZE) - f64::from(p[1])) * scale,
                        )
                    };
                    for primitive in tab_icon_primitives(kind) {
                        match *primitive {
                            TabIconPrimitive::Line { from, to, width } => {
                                let path = bezier_path();
                                appkit::send_v_f64(
                                    path,
                                    sel!(setLineWidth:),
                                    f64::from(width) * scale,
                                );
                                appkit::send_v_usize(
                                    path,
                                    sel!(setLineCapStyle:),
                                    consts::NS_LINE_CAP_STYLE_ROUND,
                                );
                                appkit::send_v_point(path, sel!(moveToPoint:), at(from));
                                appkit::send_v_point(path, sel!(lineToPoint:), at(to));
                                appkit::send_v(path, sel!(stroke));
                            }
                            TabIconPrimitive::RoundedRect {
                                rect: rect_,
                                radius,
                                width,
                            } => {
                                let frame = rect(
                                    ox + f64::from(rect_[0]) * scale,
                                    oy + (f64::from(TAB_ICON_DESIGN_SIZE)
                                        - f64::from(rect_[1] + rect_[3]))
                                        * scale,
                                    f64::from(rect_[2]) * scale,
                                    f64::from(rect_[3]) * scale,
                                );
                                let path = rounded_rect(frame, f64::from(radius) * scale);
                                appkit::send_v_f64(
                                    path,
                                    sel!(setLineWidth:),
                                    f64::from(width) * scale,
                                );
                                appkit::send_v(path, sel!(stroke));
                            }
                            TabIconPrimitive::Dot { center, radius } => {
                                let centre = at(center);
                                let radius = f64::from(radius) * scale;
                                appkit::send_v(circle(centre.x, centre.y, radius), sel!(fill));
                            }
                            TabIconPrimitive::Triangle { points } => {
                                appkit::send_v(polygon(&points.map(at)), sel!(fill));
                            }
                        }
                    }
                }
                if let Some(status) = layout_status {
                    let count = metadata.status_count();
                    let mut ordinal = 0usize;
                    let scale = status[2] / f64::from(TAB_ICON_DESIGN_SIZE)
                        * f64::from(tab_status_mark_scale(count));
                    for kind in TAB_STATUS_KINDS {
                        if !metadata.has_status_kind(kind) {
                            continue;
                        }
                        let x = status[0]
                            + f64::from(tab_status_center(ordinal, count))
                                * (status[2] / f64::from(TAB_ICON_DESIGN_SIZE));
                        let y = status[1] + status[3] * 0.5;
                        ordinal += 1;
                        match kind {
                            TabStatusKind::Dirty => {
                                appkit::send_v(strip_accent_color(), sel!(set));
                                appkit::send_v(circle(x, y, 1.75 * scale), sel!(fill));
                            }
                            TabStatusKind::Busy => {
                                appkit::send_v(ink, sel!(set));
                                let ring = circle(x, y, 2.0 * scale);
                                appkit::send_v_f64(ring, sel!(setLineWidth:), 1.25 * scale);
                                appkit::send_v(ring, sel!(stroke));
                            }
                            TabStatusKind::Attention => {
                                let level = metadata.attention;
                                if level == crate::tab_bar::ChipLevel::Done {
                                    appkit::send_v(ns_color(sel!(secondaryLabelColor)), sel!(set));
                                    appkit::send_v(circle(x, y, 1.5 * scale), sel!(fill));
                                    continue;
                                }
                                if level == crate::tab_bar::ChipLevel::Note {
                                    appkit::send_v(ink, sel!(set));
                                    let half = f64::from(crate::tab_bar::NOTE_DASH_HALF) * scale;
                                    let dash = bezier_path();
                                    appkit::send_v_f64(dash, sel!(setLineWidth:), 1.25 * scale);
                                    appkit::send_v_usize(
                                        dash,
                                        sel!(setLineCapStyle:),
                                        consts::NS_LINE_CAP_STYLE_ROUND,
                                    );
                                    appkit::send_v_point(
                                        dash,
                                        sel!(moveToPoint:),
                                        point(x - half, y),
                                    );
                                    appkit::send_v_point(
                                        dash,
                                        sel!(lineToPoint:),
                                        point(x + half, y),
                                    );
                                    appkit::send_v(dash, sel!(stroke));
                                    continue;
                                }
                                if level == crate::tab_bar::ChipLevel::Yours {
                                    appkit::send_v(ink, sel!(set));
                                    let side = 4.4 * scale;
                                    let square = rounded_rect(
                                        rect(x - side * 0.5, y - side * 0.5, side, side),
                                        0.8 * scale,
                                    );
                                    appkit::send_v_f64(square, sel!(setLineWidth:), 1.15 * scale);
                                    appkit::send_v(square, sel!(stroke));
                                    continue;
                                }
                                let hue = if matches!(level, crate::tab_bar::ChipLevel::Stop(_)) {
                                    sel!(systemRedColor)
                                } else {
                                    sel!(systemOrangeColor)
                                };
                                appkit::send_v(ns_color(hue), sel!(set));
                                let radius = 2.5 * scale;
                                let diamond = polygon(&[
                                    point(x, y + radius),
                                    point(x + radius, y),
                                    point(x, y - radius),
                                    point(x - radius, y),
                                ]);
                                if matches!(level, crate::tab_bar::ChipLevel::Stop(_)) {
                                    appkit::send_v(diamond, sel!(fill));
                                } else {
                                    appkit::send_v_f64(diamond, sel!(setLineWidth:), 1.15 * scale);
                                    appkit::send_v(diamond, sel!(stroke));
                                }
                            }
                            TabStatusKind::Connection => {
                                if let Some(role) = metadata.conn {
                                    appkit::send_v(ink, sel!(set));
                                    let (hw, hh) = (2.5 * scale, 2.4 * scale);
                                    match role {
                                        TabConnRole::Outbound => {
                                            let arrow = polygon(&[
                                                point(x, y + hh),
                                                point(x + hw, y - hh),
                                                point(x - hw, y - hh),
                                            ]);
                                            appkit::send_v(arrow, sel!(fill));
                                        }
                                        TabConnRole::Inbound => {
                                            let width = 1.15 * scale;
                                            let (cw, ch) = (hw - width * 0.5, hh - width * 0.5);
                                            let arrow = polygon(&[
                                                point(x - cw, y + ch),
                                                point(x + cw, y + ch),
                                                point(x, y - ch),
                                            ]);
                                            appkit::send_v_f64(arrow, sel!(setLineWidth:), width);
                                            appkit::send_v_usize(
                                                arrow,
                                                sel!(setLineCapStyle:),
                                                consts::NS_LINE_CAP_STYLE_ROUND,
                                            );
                                            appkit::send_v(arrow, sel!(stroke));
                                        }
                                        TabConnRole::Both => {
                                            let hw = 2.2 * scale;
                                            let hourglass = polygon(&[
                                                point(x - hw, y + hh),
                                                point(x + hw, y + hh),
                                                point(x - hw, y - hh),
                                                point(x + hw, y - hh),
                                            ]);
                                            appkit::send_v(hourglass, sel!(fill));
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }

        /// Build a tab view for `index`/`count` showing `title`.
        #[allow(
            clippy::too_many_arguments,
            reason = "tab state (id/index/count/text/active/metadata/ext) plus its render context (mtm/proxy/window/geometry); both are needed at construction and splitting them only relocates the list"
        )]
        fn build(
            mtm: MainThread,
            proxy: EventLoopProxy<Wake>,
            window: WindowId,
            tab: TabId,
            index: usize,
            count: usize,
            title: &str,
            tooltip: Option<&str>,
            active: bool,
            metadata: TabStripMetadata,
            ext: &TabChromeExt,
            frame: CGRect,
            geometry: TabGeometry,
        ) -> Option<Retained<Self>> {
            let ivars = TabIvars {
                proxy,
                window,
                index: Cell::new(index),
                tab_id: Cell::new(tab),
                menu_tab: Cell::new(None),
                count: Cell::new(count),
                active: Cell::new(active),
                metadata: Cell::new(metadata),
                hovered: Cell::new(false),
                press_x: Cell::new(0.0),
                dragged: Cell::new(false),
                solo: Cell::new(geometry.solo),
                center_y: Cell::new(geometry.center_y),
                solo_center_x: Cell::new(geometry.solo_center_x),
                separator: Cell::new(geometry.separator),
                close_available: Cell::new(false),
                icon_rect: Cell::new(None),
                status_rect: Cell::new(None),
                close_btn: RefCell::new(None),
                label: RefCell::new(None),
                desc_label: RefCell::new(None),
                tracking: RefCell::new(None),
                title: RefCell::new(title.to_string()),
                tooltip: RefCell::new(tooltip.map(str::to_string)),
                menu_entries: RefCell::new(Vec::new()),
                menu_conn: RefCell::new(Vec::new()),
                menu_upgrade: RefCell::new(Vec::new()),
            };
            // SAFETY: `alloc_ivars` returns a +1 instance owing exactly one designated initializer, which is the next send; `-initWithFrame:` is `-(id)(NSRect)` and consumes that +1, returning the initialised +1 `Retained::from_owned` adopts.
            let this: Retained<Self> = unsafe {
                let raw = Self::alloc_ivars(mtm, ivars);
                if raw.is_null() {
                    return None;
                }
                Retained::from_owned(appkit::send_id_rect(raw, sel!(initWithFrame:), frame))?
            };

            // SAFETY: `buttonWithTitle:target:action:` is the documented factory; plain
            // setters follow on the fresh button; all on the main thread.
            let close = unsafe {
                let btn = appkit::send_id_id_id_sel(
                    class(c"NSButton").as_id(),
                    sel!(buttonWithTitle:target:action:),
                    appkit::nsstring("✕")?.id(),
                    this.as_id(),
                    sel!(closeTab:),
                );
                let btn = Obj::retain(btn)?;
                appkit::send_v_bool(btn.id(), sel!(setBordered:), false);
                appkit::send_v_usize(btn.id(), sel!(setImagePosition:), consts::NS_NO_IMAGE);
                appkit::send_v_id(btn.id(), sel!(setFont:), system_font(10.0, false));
                appkit::send_v_bool(btn.id(), sel!(setHidden:), true);
                appkit::send_v_id(
                    btn.id(),
                    sel!(setToolTip:),
                    appkit::nsstring("Close Tab")?.id(),
                );
                appkit::send_v_id(this.as_id(), sel!(addSubview:), btn.id());
                btn
            };

            // SAFETY: `labelWithString:` is the documented non-raising factory; plain setters
            // follow; on the main thread.
            let (label, desc) = unsafe {
                let lbl = Obj::retain(appkit::send_id_id(
                    class(c"NSTextField").as_id(),
                    sel!(labelWithString:),
                    appkit::nsstring(title)?.id(),
                ))?;
                let desc = Obj::retain(appkit::send_id_id(
                    class(c"NSTextField").as_id(),
                    sel!(labelWithString:),
                    appkit::nsstring("")?.id(),
                ))?;
                for field in [&lbl, &desc] {
                    appkit::send_v_bool(field.id(), sel!(setDrawsBackground:), false);
                    appkit::send_v_bool(field.id(), sel!(setBezeled:), false);
                    appkit::send_v_bool(field.id(), sel!(setEditable:), false);
                    appkit::send_v_bool(field.id(), sel!(setSelectable:), false);
                    appkit::send_v_bool(field.id(), sel!(setUsesSingleLineMode:), true);
                    truncate_tail(field.id());
                }
                appkit::send_v_bool(desc.id(), sel!(setHidden:), true);
                appkit::send_v_id(this.as_id(), sel!(addSubview:), lbl.id());
                appkit::send_v_id(this.as_id(), sel!(addSubview:), desc.id());
                (lbl, desc)
            };

            *this.ivars().close_btn.borrow_mut() = Some(close);
            *this.ivars().label.borrow_mut() = Some(label);
            *this.ivars().desc_label.borrow_mut() = Some(desc);
            // SAFETY: `-setAccessibilityIdentifier:` is a plain `-(void)(NSString *)` main-thread metadata write on the fresh view.
            unsafe {
                let identifier = appkit::nsstring(&format!("aterm.tab.{index}"))?;
                make_accessibility_element(this.as_id(), "AXRadioButton")?;
                appkit::send_v_id(
                    this.as_id(),
                    sel!(setAccessibilityIdentifier:),
                    identifier.id(),
                );
            }
            this.set_chrome_ext(ext);
            this.sync_close_semantics();
            this.relayout();
            this.sync_semantics();
            install_tracking_area(this.as_id(), &this.ivars().tracking);
            Some(this)
        }

        /// Place sub-views and cache ornament rects from the current bounds and metadata.
        fn relayout(&self) {
            let ivars = self.ivars();
            // SAFETY: `-bounds` is a side-effect-free `-(NSRect)` getter.
            let bounds = unsafe { appkit::send_rect(self.as_id(), sel!(bounds)) };
            let center_y = ivars.center_y.get();
            let title = ivars.title.borrow().clone();
            if ivars.solo.get() {
                self.relayout_solo(&title, bounds, center_y);
                mark_dirty(self.as_id());
                return;
            }
            let metadata = self.metadata();
            let content = native_tab_content_layout(
                bounds.size.width,
                center_y,
                metadata.icon.is_some(),
                metadata.has_status(),
            );
            ivars.close_available.set(content.close_available);
            ivars.icon_rect.set(content.icon);
            ivars.status_rect.set(content.status);
            let active = ivars.active.get();
            // SAFETY: plain main-thread setters on our retained live sub-views; every
            // rect comes from `native_tab_content_layout` (finite, non-negative).
            unsafe {
                if let Some(btn) = ivars.close_btn.borrow().as_ref() {
                    appkit::send_v_rect(btn.id(), sel!(setFrame:), rect_of(content.close));
                }
                if let Some(desc) = ivars.desc_label.borrow().as_ref() {
                    appkit::send_v_bool(desc.id(), sel!(setHidden:), true);
                }
                if let Some(lbl) = ivars.label.borrow().as_ref() {
                    let color = if active {
                        active_label_color()
                    } else {
                        ns_color(sel!(secondaryLabelColor))
                    };
                    appkit::send_v_isize(
                        lbl.id(),
                        sel!(setAlignment:),
                        consts::NS_TEXT_ALIGNMENT_CENTER,
                    );
                    appkit::send_v_id(lbl.id(), sel!(setTextColor:), color);
                    appkit::send_v_id(lbl.id(), sel!(setFont:), system_font(12.0, active));
                    if let Some(text) = appkit::nsstring(&tab_display_label(
                        &title,
                        ivars.index.get(),
                        content.label[2],
                    )) {
                        appkit::send_v_id(lbl.id(), sel!(setStringValue:), text.id());
                    }
                    appkit::send_v_rect(lbl.id(), sel!(setFrame:), rect_of(content.label));
                }
            }
            self.refresh_close_visibility();
            mark_dirty(self.as_id());
        }

        /// Return the rename editor's frame in container coordinates.
        fn editor_frame(&self) -> CGRect {
            let ivars = self.ivars();
            // SAFETY: `-frame` is a side-effect-free `-(NSRect)` getter.
            let frame = unsafe { appkit::send_rect(self.as_id(), sel!(frame)) };
            let y = frame.origin.y + ivars.center_y.get() - TAB_PILL_HEIGHT * 0.5;
            if ivars.solo.get() {
                let width = frame.size.width.min(SOLO_EDITOR_WIDTH);
                let center = frame.origin.x + ivars.solo_center_x.get();
                let x = (center - width * 0.5)
                    .max(frame.origin.x)
                    .min(frame.origin.x + frame.size.width - width);
                return rect(x, y, width, TAB_PILL_HEIGHT);
            }
            rect(
                frame.origin.x + TAB_PILL_INSET_X,
                y,
                (frame.size.width - 2.0 * TAB_PILL_INSET_X).max(1.0),
                TAB_PILL_HEIGHT,
            )
        }
        /// Lay out solo mode with the title, description, app icon, and status canvas centered.
        fn relayout_solo(&self, title: &str, bounds: CGRect, center_y: f64) {
            let ivars = self.ivars();
            let metadata = self.metadata();
            let subtitle = crate::tab_bar::solo_subtitle(title, ivars.tooltip.borrow().as_deref());
            let available = (bounds.size.width - 2.0 * SOLO_EDGE_PAD).max(0.0);
            let icon_w = metadata
                .icon
                .map_or(0.0, |_| TAB_ICON_NATIVE_SIZE + SOLO_GAP);
            let status_w = if metadata.has_status() {
                SOLO_STATUS_SIZE + SOLO_GAP
            } else {
                0.0
            };
            ivars.close_available.set(false);
            // SAFETY: plain main-thread AppKit setters/getters on our retained live
            // sub-views; `sizeToFit` only resizes the label it is sent to.
            let (title_w, desc_w) = unsafe {
                if let Some(btn) = ivars.close_btn.borrow().as_ref() {
                    appkit::send_v_bool(btn.id(), sel!(setHidden:), true);
                }
                let mut title_w = 0.0;
                let mut desc_w = 0.0;
                if let Some(lbl) = ivars.label.borrow().as_ref() {
                    appkit::send_v_isize(
                        lbl.id(),
                        sel!(setAlignment:),
                        consts::NS_TEXT_ALIGNMENT_LEFT,
                    );
                    appkit::send_v_id(lbl.id(), sel!(setFont:), system_font(13.0, false));
                    appkit::send_v_id(lbl.id(), sel!(setTextColor:), ns_color(sel!(labelColor)));
                    if let Some(text) = appkit::nsstring(title) {
                        appkit::send_v_id(lbl.id(), sel!(setStringValue:), text.id());
                    }
                    appkit::send_v(lbl.id(), sel!(sizeToFit));
                    title_w = appkit::send_rect(lbl.id(), sel!(frame)).size.width;
                }
                if let Some(desc) = ivars.desc_label.borrow().as_ref() {
                    match subtitle.as_deref() {
                        Some(text) => {
                            appkit::send_v_bool(desc.id(), sel!(setHidden:), false);
                            appkit::send_v_isize(
                                desc.id(),
                                sel!(setAlignment:),
                                consts::NS_TEXT_ALIGNMENT_LEFT,
                            );
                            appkit::send_v_id(desc.id(), sel!(setFont:), system_font(12.0, false));
                            appkit::send_v_id(
                                desc.id(),
                                sel!(setTextColor:),
                                ns_color(sel!(secondaryLabelColor)),
                            );
                            if let Some(ns) = appkit::nsstring(text) {
                                appkit::send_v_id(desc.id(), sel!(setStringValue:), ns.id());
                            }
                            appkit::send_v(desc.id(), sel!(sizeToFit));
                            desc_w = appkit::send_rect(desc.id(), sel!(frame)).size.width;
                        }
                        None => appkit::send_v_bool(desc.id(), sel!(setHidden:), true),
                    }
                }
                (title_w, desc_w)
            };
            let desc_gap = if desc_w > 0.0 { SOLO_GAP } else { 0.0 };
            let fixed = icon_w + status_w;
            let title_w = title_w.min((available - fixed).max(0.0));
            let desc_w = desc_w.min((available - fixed - title_w - desc_gap).max(0.0));
            let desc_gap = if desc_w > 0.0 { desc_gap } else { 0.0 };
            let total = fixed + title_w + desc_gap + desc_w;
            let mut x = (ivars.solo_center_x.get() - total * 0.5)
                .max(SOLO_EDGE_PAD)
                .min((bounds.size.width - SOLO_EDGE_PAD - total).max(SOLO_EDGE_PAD));

            ivars.icon_rect.set(metadata.icon.map(|_| {
                let rect = [
                    x,
                    center_y - TAB_ICON_NATIVE_SIZE * 0.5,
                    TAB_ICON_NATIVE_SIZE,
                    TAB_ICON_NATIVE_SIZE,
                ];
                x += TAB_ICON_NATIVE_SIZE + SOLO_GAP;
                rect
            }));
            // SAFETY: main-thread geometry setters on the retained live labels.
            unsafe {
                if let Some(lbl) = ivars.label.borrow().as_ref() {
                    appkit::send_v_rect(
                        lbl.id(),
                        sel!(setFrame:),
                        rect_of([
                            x,
                            center_y - SOLO_LABEL_HEIGHT * 0.5,
                            title_w,
                            SOLO_LABEL_HEIGHT,
                        ]),
                    );
                }
                x += title_w;
                if let Some(desc) = ivars.desc_label.borrow().as_ref() {
                    appkit::send_v_bool(desc.id(), sel!(setHidden:), desc_w <= 0.0);
                    if desc_w > 0.0 {
                        appkit::send_v_rect(
                            desc.id(),
                            sel!(setFrame:),
                            rect_of([
                                x + desc_gap,
                                center_y - SOLO_LABEL_HEIGHT * 0.5,
                                desc_w,
                                SOLO_LABEL_HEIGHT,
                            ]),
                        );
                    }
                }
                x += desc_gap + desc_w;
            }
            ivars.status_rect.set(metadata.has_status().then_some([
                x + SOLO_GAP,
                center_y - SOLO_STATUS_SIZE * 0.5,
                SOLO_STATUS_SIZE,
                SOLO_STATUS_SIZE,
            ]));
        }

        /// Update this chip's geometry and relayout if changed.
        fn set_geometry(&self, geometry: TabGeometry) {
            let ivars = self.ivars();
            let unchanged = ivars.solo.get() == geometry.solo
                && ivars.center_y.get() == geometry.center_y
                && ivars.solo_center_x.get() == geometry.solo_center_x
                && ivars.separator.get() == geometry.separator;
            if unchanged {
                return;
            }
            ivars.solo.set(geometry.solo);
            ivars.center_y.set(geometry.center_y);
            ivars.solo_center_x.set(geometry.solo_center_x);
            ivars.separator.set(geometry.separator);
            self.relayout();
        }

        /// Update title and tooltip context in place.
        fn set_context(&self, title: &str, tooltip: Option<&str>) {
            let tooltip_changed = self.ivars().tooltip.borrow().as_deref() != tooltip;
            let title_changed = {
                let current = self.ivars().title.borrow();
                current.as_str() != title
                    && !super::busy_spinner_phase_only_change(current.as_str(), title)
            };
            if !title_changed && !tooltip_changed {
                return;
            }
            if tooltip_changed {
                *self.ivars().tooltip.borrow_mut() = tooltip.map(str::to_string);
            }
            if title_changed {
                *self.ivars().title.borrow_mut() = title.to_string();
                self.sync_close_semantics();
            }
            if title_changed || self.ivars().solo.get() {
                self.relayout();
            }
            self.sync_semantics();
        }

        /// Refresh the close button's tooltip and accessibility labels from the current title.
        fn sync_close_semantics(&self) {
            let title = self.ivars().title.borrow();
            let label_text = tab_close_accessibility_label(&title);
            if let Some(btn) = self.ivars().close_btn.borrow().as_ref() {
                // SAFETY: NSButton inherits the NSAccessibility setters; these are
                // plain main-thread metadata updates on our retained live button.
                unsafe {
                    if let (Some(label), Some(help)) = (
                        appkit::nsstring(&label_text),
                        appkit::nsstring("Closes this tab"),
                    ) {
                        appkit::send_v_id(btn.id(), sel!(setToolTip:), label.id());
                        appkit::send_v_id(btn.id(), sel!(setAccessibilityLabel:), label.id());
                        appkit::send_v_id(btn.id(), sel!(setAccessibilityHelp:), help.id());
                    }
                }
            }
        }

        /// Update the context-menu entries in place.
        fn set_chrome_ext(&self, ext: &TabChromeExt) {
            let ivars = self.ivars();
            if *ivars.menu_entries.borrow() != ext.menu {
                *ivars.menu_entries.borrow_mut() = ext.menu.clone();
            }
        }

        fn set_tab_id(&self, id: TabId) {
            self.ivars().tab_id.set(id);
        }

        fn tab_id(&self) -> TabId {
            self.ivars().tab_id.get()
        }

        /// Build and pop this tab's context menu at the pointer.
        fn show_context_menu(&self, event: Id) {
            let _ = self.ivars().proxy.send_event(Wake::TabContextMenuOpening {
                window: self.ivars().window,
            });
            let entries = crate::session_chrome::menu_shown_at(
                &self.ivars().menu_entries.borrow(),
                crate::turn_ledger::now_ms(),
            );
            if entries.is_empty() {
                return;
            }
            self.ivars().menu_tab.set(Some(self.ivars().tab_id.get()));
            *self.ivars().menu_conn.borrow_mut() = entries
                .iter()
                .filter_map(|e| match e {
                    TabMenuEntry::ConnectionAction { peer_sid, verb, .. } => {
                        Some((peer_sid.clone(), *verb))
                    }
                    _ => None,
                })
                .collect();
            let mut conn_index: isize = 0;
            *self.ivars().menu_upgrade.borrow_mut() = entries
                .iter()
                .filter_map(|e| match e {
                    TabMenuEntry::Upgrade { sid, to, word, .. } => {
                        Some((sid.clone(), to.clone(), *word))
                    }
                    _ => None,
                })
                .collect();
            let mut upgrade_index: isize = 0;
            // SAFETY: `+[NSMenu new]` is `+(id)` and returns a +1 the `Obj`
            // adopts; the menu is released when this frame ends, after the
            // synchronous tracking session `popUpContextMenu:` runs.
            let Some(menu) =
                (unsafe { Obj::from_owned(appkit::send_id(class(c"NSMenu").as_id(), sel!(new))) })
            else {
                return;
            };
            // SAFETY: main-thread NSMenu/NSMenuItem construction and setters on fresh instances (the same non-raising calls `menu.rs` uses). Menu targets are weak: `handle.tabs` normally keeps this view alive through tracking, and if a rebuild drops it the zeroed target makes the click a no-op.
            unsafe {
                let Some(empty) = appkit::nsstring("") else {
                    return;
                };
                let item_with = |title: &str, action: aterm_objc::Sel| -> Option<Obj> {
                    let ns = appkit::nsstring(title)?;
                    Obj::from_owned(appkit::send_id_id_sel_id(
                        appkit::alloc(class(c"NSMenuItem")),
                        sel!(initWithTitle:action:keyEquivalent:),
                        ns.id(),
                        action,
                        empty.id(),
                    ))
                };
                appkit::send_v_bool(menu.id(), sel!(setAutoenablesItems:), false);
                for entry in entries.iter() {
                    match entry {
                        TabMenuEntry::Header(text) | TabMenuEntry::Event { words: text, .. } => {
                            let Some(item) = item_with(text, aterm_objc::Sel::NULL) else {
                                continue;
                            };
                            appkit::send_v_bool(item.id(), sel!(setEnabled:), false);
                            appkit::send_v_id(menu.id(), sel!(addItem:), item.id());
                        }
                        TabMenuEntry::Separator => {
                            appkit::send_v_id(
                                menu.id(),
                                sel!(addItem:),
                                appkit::send_id(class(c"NSMenuItem").as_id(), sel!(separatorItem)),
                            );
                        }
                        TabMenuEntry::Action {
                            label,
                            action,
                            enabled,
                        } => {
                            let Some(item) = item_with(label, sel!(tabMenuAction:)) else {
                                continue;
                            };
                            appkit::send_v_isize(item.id(), sel!(setTag:), action.tag());
                            appkit::send_v_bool(item.id(), sel!(setEnabled:), *enabled);
                            appkit::send_v_id(item.id(), sel!(setTarget:), self.as_id());
                            appkit::send_v_id(menu.id(), sel!(addItem:), item.id());
                        }
                        TabMenuEntry::ConnectionAction { label, enabled, .. } => {
                            let Some(item) = item_with(label, sel!(tabMenuConnection:)) else {
                                continue;
                            };
                            appkit::send_v_isize(item.id(), sel!(setTag:), conn_index);
                            conn_index += 1;
                            appkit::send_v_bool(item.id(), sel!(setEnabled:), *enabled);
                            appkit::send_v_id(item.id(), sel!(setTarget:), self.as_id());
                            appkit::send_v_id(menu.id(), sel!(addItem:), item.id());
                        }
                        TabMenuEntry::Upgrade { label, .. } => {
                            let Some(item) = item_with(label, sel!(tabMenuUpgrade:)) else {
                                continue;
                            };
                            appkit::send_v_isize(item.id(), sel!(setTag:), upgrade_index);
                            upgrade_index += 1;
                            appkit::send_v_bool(item.id(), sel!(setEnabled:), true);
                            appkit::send_v_id(item.id(), sel!(setTarget:), self.as_id());
                            appkit::send_v_id(menu.id(), sel!(addItem:), item.id());
                        }
                    }
                }
                appkit::send_v_id_id_id(
                    class(c"NSMenu").as_id(),
                    sel!(popUpContextMenu:withEvent:forView:),
                    menu.id(),
                    event,
                    self.as_id(),
                );
            }
        }

        /// This tab's context-menu introspection line for `read_tab_menus`.
        fn menu_line(&self, index: usize) -> String {
            crate::session_chrome::tab_menu_chrome_line(
                index,
                &self.ivars().menu_entries.borrow(),
                crate::turn_ledger::now_ms(),
            )
        }

        /// Diff icon, status, and close metadata in place.
        fn set_metadata(&self, metadata: TabStripMetadata) {
            let old = self.ivars().metadata.replace(metadata);
            if old == metadata {
                return;
            }
            let old_with_new_load = TabStripMetadata {
                load: metadata.load,
                ..old
            };
            if old_with_new_load == metadata {
                mark_dirty(self.as_id());
                return;
            }
            self.relayout();
            self.sync_semantics();
        }

        /// Update the load step and repaint when the meter or its inks change.
        fn set_load(&self, load: u8, inks_moved: bool) {
            let ivars = self.ivars();
            let mut metadata = ivars.metadata.get();
            let old = metadata.load;
            metadata.load = load;
            ivars.metadata.set(metadata);
            // SAFETY: `-bounds` is a side-effect-free `-(NSRect)` getter on this live view, on the main thread.
            let bounds = unsafe { appkit::send_rect(self.as_id(), sel!(bounds)) };
            let pill_w = ((bounds.size.width - 2.0 * TAB_PILL_INSET_X).max(0.0)) as f32;
            let moved = crate::tab_bar::chip_load_len_pt(pill_w, old)
                != crate::tab_bar::chip_load_len_pt(pill_w, load);
            let lit = crate::tab_bar::chip_load_len_pt(pill_w, load).is_some();
            if moved || (inks_moved && lit) {
                mark_dirty(self.as_id());
            }
        }

        /// Re-resolve this tab's label ink against the selected-tab color override.
        fn refresh_label_ink(&self) {
            let ivars = self.ivars();
            if ivars.solo.get() {
                return;
            }
            if let Some(lbl) = ivars.label.borrow().as_ref() {
                // SAFETY: main-thread `setTextColor:` on the live retained label.
                unsafe {
                    let color = if ivars.active.get() {
                        active_label_color()
                    } else {
                        ns_color(sel!(secondaryLabelColor))
                    };
                    appkit::send_v_id(lbl.id(), sel!(setTextColor:), color);
                }
            }
        }

        /// Update the active flag in place and repaint.
        fn set_active(&self, active: bool) {
            if self.ivars().active.get() == active {
                return;
            }
            self.ivars().active.set(active);
            self.relayout();
            self.sync_semantics();
        }

        /// Sync hover text and accessibility attributes from the tab presentation.
        fn sync_semantics(&self) {
            let ivars = self.ivars();
            let metadata = self.metadata();
            let title = ivars.title.borrow();
            let tooltip = ivars.tooltip.borrow();
            let help = tab_help(&title, tooltip.as_deref(), metadata, ivars.index.get());
            // SAFETY: NSView implements the NSAccessibility setters on every supported macOS
            // version.
            unsafe {
                let v = self.as_id();
                let (Some(label), Some(help)) = (appkit::nsstring(&title), appkit::nsstring(&help))
                else {
                    return;
                };
                appkit::send_v_id(v, sel!(setToolTip:), help.id());
                appkit::send_v_id(v, sel!(setAccessibilityLabel:), label.id());
                appkit::send_v_id(v, sel!(setAccessibilityHelp:), help.id());
                appkit::send_v_bool(v, sel!(setAccessibilitySelected:), ivars.active.get());
            }
        }

        /// Show the close button only while the pointer is inside this chip.
        fn refresh_close_visibility(&self) {
            let ivars = self.ivars();
            let show = ivars.close_available.get()
                && !ivars.solo.get()
                && ivars.metadata.get().closable
                && ivars.hovered.get();
            if let Some(btn) = ivars.close_btn.borrow().as_ref() {
                // SAFETY: `-setHidden:` is a `-(void)(BOOL)` setter on the live retained button, on the main thread.
                unsafe { appkit::send_v_bool(btn.id(), sel!(setHidden:), !show) };
            }
        }

        /// This tab's canonical, undecorated title for cross-platform introspection.
        fn label_text(&self) -> String {
            self.ivars().title.borrow().clone()
        }

        fn metadata(&self) -> TabStripMetadata {
            self.ivars().metadata.get()
        }

        fn tooltip(&self) -> Option<String> {
            self.ivars().tooltip.borrow().clone()
        }

        /// Whether this tab is the active one (the selected segment, for introspection).
        fn is_active(&self) -> bool {
            self.ivars().active.get()
        }
    }

    /// The toolbar item identifier for the full-width tab-strip custom view.
    const STRIP_ITEM_ID: &str = "aterm.tabstrip";

    /// Instance variables for [`ToolbarDelegate`].
    pub(crate) struct DelegateIvars {
        container: Obj,
    }

    aterm_objc::declare_class! {
        /// `NSToolbarDelegate` vending the single tab-strip custom-view item.
        pub(crate) struct ToolbarDelegate: NSObject {
            const NAME: &str = "ATermToolbarDelegate";
            type Ivars = DelegateIvars;
            protocols: [NSObject, NSToolbarDelegate];

            /// The items shown by default: just the tab-strip custom-view item.
            @sel(toolbarDefaultItemIdentifiers:)
            fn default_item_identifiers(&self, _toolbar: Id) -> Id {
                Self::item_identifiers()
            }

            /// The items the toolbar may contain: the same single set.
            @sel(toolbarAllowedItemIdentifiers:)
            fn allowed_item_identifiers(&self, _toolbar: Id) -> Id {
                Self::item_identifiers()
            }

            /// Build the `NSToolbarItem` for the strip identifier.
            @sel(toolbar:itemForItemIdentifier:willBeInsertedIntoToolbar:)
            fn item_for_identifier(
                &self,
                _toolbar: Id,
                identifier: Id,
                _will_be_inserted: aterm_objc::Bool,
            ) -> Id {
                if identifier.is_null() {
                    return Id::NIL;
                }
                let Some(strip_id) = appkit::nsstring(STRIP_ITEM_ID) else {
                    return Id::NIL;
                };
                // SAFETY: `identifier` is the live NSString AppKit passed and `-isEqualToString:` is a side-effect-free `-(BOOL)(NSString *)` comparison.
                if !unsafe {
                    appkit::send_bool_id(identifier, sel!(isEqualToString:), strip_id.id())
                } {
                    return Id::NIL;
                }
                let ivars = self.ivars();
                // SAFETY: NSToolbarItem construction and plain setters on a fresh instance, on the main thread. `-initWithItemIdentifier:` consumes the +1 from `+alloc`; the container outlives the item (retained here and in the handle). The item is autoreleased on return (+0 owed).
                unsafe {
                    let raw = appkit::send_id_id(
                        appkit::alloc(class(c"NSToolbarItem")),
                        sel!(initWithItemIdentifier:),
                        identifier,
                    );
                    let Some(item) = Obj::from_owned(raw) else {
                        return Id::NIL;
                    };
                    let Some(label) = appkit::nsstring("Tabs") else {
                        return Id::NIL;
                    };
                    appkit::send_v_id(item.id(), sel!(setLabel:), label.id());
                    appkit::send_v_id(item.id(), sel!(setPaletteLabel:), label.id());
                    appkit::send_v_bool(item.id(), sel!(setBordered:), false);
                    appkit::send_v_id(item.id(), sel!(setView:), ivars.container.id());
                    appkit::send_v_size(
                        item.id(),
                        sel!(setMinSize:),
                        aterm_objc::CGSize {
                            width: STRIP_MIN_WIDTH,
                            height: STRIP_HEIGHT,
                        },
                    );
                    appkit::send_v_size(
                        item.id(),
                        sel!(setMaxSize:),
                        aterm_objc::CGSize {
                            width: STRIP_MAX_WIDTH,
                            height: STRIP_HEIGHT,
                        },
                    );
                    item.autorelease()
                }
            }
        }
    }

    impl ToolbarDelegate {
        fn new(mtm: MainThread, container: Obj) -> Option<Retained<Self>> {
            Self::alloc_init(mtm, DelegateIvars { container })
        }

        /// `@[STRIP_ITEM_ID]`, +0 (`+arrayWithObject:` returns it autoreleased).
        fn item_identifiers() -> Id {
            let Some(strip) = appkit::nsstring(STRIP_ITEM_ID) else {
                return Id::NIL;
            };
            // SAFETY: `+arrayWithObject:` is `-(id)(id)` and retains its element, so the +1 `strip` may drop at the end of this frame.
            unsafe {
                appkit::send_id_id(
                    class(c"NSArray").as_id(),
                    sel!(arrayWithObject:),
                    strip.id(),
                )
            }
        }
    }

    /// Attach the native unified-compact toolbar and tab strip to `window`.
    pub(crate) fn install_window_toolbar(
        window: &aterm_window::window::Window,
        proxy: &EventLoopProxy<Wake>,
        wid: WindowId,
    ) -> Option<ToolbarHandle> {
        let mtm = MainThread::new()?;

        let view = crate::platform::ns_view_of(window);
        if view.is_null() {
            return None;
        }
        // SAFETY: `-window` is `-(id)` and `-frame` is `-(NSRect)`, both side-effect-free main-thread getters on that live view / its window.
        let (ns_window, win_w) = unsafe {
            let w = appkit::send_id(view, sel!(window));
            if w.is_null() {
                return None;
            }
            (w, appkit::send_rect(w, sel!(frame)).size.width.max(200.0))
        };

        let plus = ChromeButton::build(
            mtm,
            proxy.clone(),
            MenuAction::NewTab,
            "New Tab",
            rect(
                win_w - TRAILING_PAD - PLUS_WIDTH,
                0.0,
                PLUS_WIDTH,
                STRIP_HEIGHT,
            ),
        )?;
        // SAFETY: plain main-thread setters on the fresh view — `-setToolTip:` is `-(void)(NSString *)` and `-setAutoresizingMask:` is `-(void)(NSAutoresizingMaskOptions)`, an NSUInteger bitmask.
        unsafe {
            appkit::send_v_id(
                plus.as_id(),
                sel!(setToolTip:),
                appkit::nsstring("New Tab")?.id(),
            );
            appkit::send_v_usize(
                plus.as_id(),
                sel!(setAutoresizingMask:),
                consts::NS_VIEW_MIN_X_MARGIN_MAX_Y_MARGIN,
            );
        }

        // SAFETY: standard NSView construction + setters on a fresh instance, on the
        // main thread; `addSubview:` takes the live retained button.
        let container = unsafe {
            let v = Obj::from_owned(appkit::send_id_rect(
                appkit::alloc(class(c"NSView")),
                sel!(initWithFrame:),
                rect(0.0, 0.0, win_w, STRIP_HEIGHT),
            ))?;
            appkit::send_v_usize(
                v.id(),
                sel!(setAutoresizingMask:),
                consts::NS_VIEW_WIDTH_SIZABLE,
            );
            appkit::send_v_id(v.id(), sel!(addSubview:), plus.as_id());
            let role = appkit::nsstring("AXTabGroup")?;
            let label = appkit::nsstring("Tabs")?;
            appkit::send_v_bool(v.id(), sel!(setAccessibilityElement:), true);
            appkit::send_v_id(v.id(), sel!(setAccessibilityRole:), role.id());
            appkit::send_v_id(v.id(), sel!(setAccessibilityLabel:), label.id());
            v
        };

        let delegate = ToolbarDelegate::new(mtm, container.clone_retained())?;

        // SAFETY: standard NSToolbar / NSWindow setup, all on the main thread.
        let toolbar = unsafe {
            let identifier = appkit::nsstring("aterm.toolbar")?;
            let toolbar = Obj::from_owned(appkit::send_id_id(
                appkit::alloc(class(c"NSToolbar")),
                sel!(initWithIdentifier:),
                identifier.id(),
            ))?;
            appkit::send_v_id(toolbar.id(), sel!(setDelegate:), delegate.as_id());
            appkit::send_v_bool(toolbar.id(), sel!(setAllowsUserCustomization:), false);
            appkit::send_v_usize(
                toolbar.id(),
                sel!(setDisplayMode:),
                consts::NS_TOOLBAR_DISPLAY_MODE_ICON_ONLY,
            );
            appkit::send_v_id(ns_window, sel!(setToolbar:), toolbar.id());
            appkit::send_v_isize(
                ns_window,
                sel!(setToolbarStyle:),
                consts::NS_WINDOW_TOOLBAR_STYLE_UNIFIED_COMPACT,
            );
            appkit::send_v_isize(
                ns_window,
                sel!(setTitleVisibility:),
                consts::NS_WINDOW_TITLE_HIDDEN,
            );
            appkit::send_v_bool(ns_window, sel!(setTitlebarAppearsTransparent:), true);
            appkit::send_v_bool(toolbar.id(), sel!(setVisible:), true);
            toolbar
        };

        Some(ToolbarHandle {
            _delegate: delegate,
            _toolbar: toolbar,
            container,
            proxy: proxy.clone(),
            window: wid,
            tabs: RefCell::new(Vec::new()),
            plus,
            // SAFETY: `view` is winit's live content `NSView`; `Obj::retain`
            // takes one +1 of a live object, released when the handle drops.
            content_view: unsafe { Obj::retain(view) }?,
            rename: RefCell::new(None),
            pill: RefCell::new(None),
            pill_w: Cell::new(0.0),
            pill_slot: Cell::new(None),
        })
    }

    /// Selected-tab color override packed as `0xFF_RR_GG_BB` (`0` means no override).
    static ACTIVE_TAB_COLOR: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

    /// Authored chrome accent packed as `0xFF_RR_GG_BB` (`0` uses `controlAccentColor`).
    static STRIP_ACCENT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

    /// Load meter inks packed as `0xFF_RR_GG_BB` for active and inactive chips.
    static STRIP_LOAD_TAB: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
    static STRIP_LOAD_ATERM: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

    /// Return the `NSColor` for a tab chip's load meter.
    /// # Safety
    /// Must be called on the main thread inside a draw cycle.
    unsafe fn strip_load_color(active: bool) -> Id {
        let (slot, alpha) = if active {
            (&STRIP_LOAD_TAB, 1.0)
        } else {
            (&STRIP_LOAD_ATERM, 0.7)
        };
        match unpack(slot.load(std::sync::atomic::Ordering::Relaxed)) {
            // SAFETY: a plain colour constructor on the main thread.
            Some([r, g, b]) => unsafe {
                srgb(
                    f64::from(r) / 255.0,
                    f64::from(g) / 255.0,
                    f64::from(b) / 255.0,
                    alpha,
                )
            },
            // SAFETY: as above.
            None => unsafe { with_alpha(ns_color(sel!(secondaryLabelColor)), alpha) },
        }
    }

    /// Update per-chip load steps and meter inks and repaint the strip.
    pub(crate) fn set_chip_loads(handle: &ToolbarHandle, loads: &[u8], inks: [[u8; 3]; 2]) {
        let mut inks_moved = false;
        for (slot, ink) in [(&STRIP_LOAD_TAB, inks[0]), (&STRIP_LOAD_ATERM, inks[1])] {
            let packed = pack(Some(ink));
            inks_moved |= slot.swap(packed, std::sync::atomic::Ordering::Relaxed) != packed;
        }
        let tabs = handle.tabs.borrow();
        if tabs.len() != loads.len() {
            return;
        }
        for (tab, &load) in tabs.iter().zip(loads) {
            tab.set_load(load, inks_moved);
        }
    }

    /// Return the `NSColor` for strip accent marks.
    /// # Safety
    /// Must be called on the main thread inside a draw cycle.
    unsafe fn strip_accent_color() -> Id {
        match unpack(STRIP_ACCENT.load(std::sync::atomic::Ordering::Relaxed)) {
            // SAFETY: a plain colour constructor on the main thread.
            Some([r, g, b]) => unsafe {
                srgb(
                    f64::from(r) / 255.0,
                    f64::from(g) / 255.0,
                    f64::from(b) / 255.0,
                    1.0,
                )
            },
            // SAFETY: as above.
            None => unsafe { ns_color(sel!(controlAccentColor)) },
        }
    }

    /// Set or clear the strip accent color override and repaint.
    pub(crate) fn set_strip_accent(handle: &ToolbarHandle, accent: Option<[u8; 3]>) {
        let packed = pack(accent);
        if STRIP_ACCENT.swap(packed, std::sync::atomic::Ordering::Relaxed) == packed {
            return;
        }
        for tab in handle.tabs.borrow().iter() {
            mark_dirty(tab.as_id());
        }
        mark_dirty(handle.container.id());
    }

    /// `[r, g, b]` packed `0xFF_RR_GG_BB`; `None` packs to `0`.
    fn pack(color: Option<[u8; 3]>) -> u32 {
        color.map_or(0, |c| {
            0xFF00_0000 | (u32::from(c[0]) << 16) | (u32::from(c[1]) << 8) | u32::from(c[2])
        })
    }

    /// The inverse of [`pack`].
    fn unpack(packed: u32) -> Option<[u8; 3]> {
        (packed >> 24 == 0xFF).then_some([
            ((packed >> 16) & 0xff) as u8,
            ((packed >> 8) & 0xff) as u8,
            (packed & 0xff) as u8,
        ])
    }

    /// The live override as `[r, g, b]`, if one is pinned.
    fn active_tab_color_override() -> Option<[u8; 3]> {
        let packed = ACTIVE_TAB_COLOR.load(std::sync::atomic::Ordering::Relaxed);
        (packed >> 24 == 0xFF).then_some({
            [
                ((packed >> 16) & 0xff) as u8,
                ((packed >> 8) & 0xff) as u8,
                (packed & 0xff) as u8,
            ]
        })
    }

    /// Return the active tab's title `NSColor`.
    /// # Safety
    /// Main thread; the result is +0 and is handed straight to `-setTextColor:`, which retains it.
    unsafe fn active_label_color() -> Id {
        match active_tab_color_override() {
            // SAFETY: plain color constructors on the main thread.
            Some(rgb) => unsafe {
                if crate::tab_bar::bg_is_light(rgb) {
                    ns_color(sel!(blackColor))
                } else {
                    ns_color(sel!(whiteColor))
                }
            },
            // SAFETY: as above.
            None => unsafe { ns_color(sel!(labelColor)) },
        }
    }

    /// Set or clear the active-tab background color override and repaint.
    pub(crate) fn set_active_tab_color(handle: &ToolbarHandle, color: Option<[u8; 3]>) {
        let packed = color.map_or(0, |c| {
            0xFF00_0000 | (u32::from(c[0]) << 16) | (u32::from(c[1]) << 8) | u32::from(c[2])
        });
        if ACTIVE_TAB_COLOR.swap(packed, std::sync::atomic::Ordering::Relaxed) == packed {
            return;
        }
        for tab in handle.tabs.borrow().iter() {
            tab.refresh_label_ink();
            mark_dirty(tab.as_id());
        }
        mark_dirty(handle.container.id());
    }

    /// Resolve the system's selected-content color in the strip's appearance as sRGB.
    pub(crate) fn native_selected_pill_rgb(handle: &ToolbarHandle) -> Option<[u8; 3]> {
        // SAFETY: main thread (the handle is main-thread only). Every call is a
        // documented non-raising AppKit message: `+currentAppearance` and
        // `-effectiveAppearance` return +0 appearances, `+setCurrentAppearance:`
        // takes one (or nil, restoring the default) for the duration of the
        // resolution and is put back before returning, `+sRGBColorSpace` is an
        // immortal shared space, `-colorUsingColorSpace:` returns a +0 colour or
        // nil, and the component getters return plain `CGFloat`s.
        unsafe {
            let appearance = class(c"NSAppearance").as_id();
            let previous = appkit::send_id(appearance, sel!(currentAppearance));
            let strip = appkit::send_id(handle.container.id(), sel!(effectiveAppearance));
            appkit::send_v_id(appearance, sel!(setCurrentAppearance:), strip);
            let space = appkit::send_id(class(c"NSColorSpace").as_id(), sel!(sRGBColorSpace));
            let color = appkit::send_id_id(
                ns_color(sel!(selectedContentBackgroundColor)),
                sel!(colorUsingColorSpace:),
                space,
            );
            let rgb = (!color.is_null()).then(|| {
                let channel = |component: aterm_objc::Sel| {
                    (appkit::send_f64(color, component).clamp(0.0, 1.0) * 255.0).round() as u8
                };
                [
                    channel(sel!(redComponent)),
                    channel(sel!(greenComponent)),
                    channel(sel!(blueComponent)),
                ]
            });
            appkit::send_v_id(appearance, sel!(setCurrentAppearance:), previous);
            rgb
        }
    }

    pub(crate) fn set_strip_dark(handle: &ToolbarHandle, dark: bool) {
        // SAFETY: `+[NSAppearance appearanceNamed:]` returns a +0 built-in appearance (or nil) for a system name read by `appkit::appearance_name`. `-setAppearance:` is a plain main-thread setter on the retained container view, and it retains.
        unsafe {
            let appearance = appkit::send_id_id(
                class(c"NSAppearance").as_id(),
                sel!(appearanceNamed:),
                appkit::appearance_name(dark),
            );
            if !appearance.is_null() {
                appkit::send_v_id(handle.container.id(), sel!(setAppearance:), appearance);
            }
        }
    }

    /// Synchronize the native tab strip with the window's current tab state.
    #[allow(
        clippy::too_many_arguments,
        reason = "the strip's one consistent snapshot: every per-tab vector, the active index and the window's pill"
    )]
    pub(crate) fn set_window_tabs(
        handle: &ToolbarHandle,
        titles: &[String],
        ids: &[TabId],
        metadata: &[TabStripMetadata],
        tooltips: &[Option<String>],
        ext: &[TabChromeExt],
        active: usize,
        pill: Option<&crate::app_presence::NeedsPillModel>,
    ) {
        let Some(mtm) = MainThread::new() else {
            return;
        };
        sync_pill(handle, mtm, pill);
        let container = handle.container.id();
        let metadata_at = |i: usize| {
            metadata.get(i).copied().unwrap_or(TabStripMetadata {
                icon: None,
                dirty: false,
                busy: false,
                attention: crate::tab_bar::ChipLevel::Off,
                conn: None,
                closable: true,
                team: crate::tab_bar::TabTeam::NONE,
                load: 0,
            })
        };
        let id_at = |i: usize| ids.get(i).copied().unwrap_or(TabId::from_stored(0));
        let default_ext = TabChromeExt::default();
        let ext_at = |i: usize| ext.get(i).unwrap_or(&default_ext);
        let tooltip_at = |i: usize| tooltips.get(i).and_then(Option::as_deref);

        if titles.is_empty() {
            // SAFETY: `removeFromSuperview` is a main-thread mutator.
            {
                let mut tabs = handle.tabs.borrow_mut();
                for old in tabs.drain(..) {
                    unsafe { appkit::send_v(old.as_id(), sel!(removeFromSuperview)) };
                }
            }
            sync_rename_overlay(handle, &[]);
            return;
        }

        let StripPlan {
            cells,
            metrics,
            strip_h,
        } = plan_strip(handle, titles.len(), active);
        let n = titles.len();
        let active = active.min(n.saturating_sub(1));
        let geometry_at = |index: usize, cell_x: f64| metrics.geometry(index, n, active, cell_x);

        {
            let tabs = handle.tabs.borrow();
            if tabs.len() == n
                && tabs.iter().zip(&cells).all(|(tab, &(cx, cw))| {
                    // SAFETY: `-frame` is a side-effect-free `-(NSRect)` getter on a live retained chip.
                    let f = unsafe { appkit::send_rect(tab.as_id(), sel!(frame)) };
                    f.origin.x == cx && f.size.width == cw
                })
            {
                for (i, (tab, title)) in tabs.iter().zip(titles).enumerate() {
                    tab.set_geometry(geometry_at(i, cells[i].0));
                    tab.set_context(title, tooltip_at(i));
                    tab.set_tab_id(id_at(i));
                    tab.set_metadata(metadata_at(i));
                    tab.set_chrome_ext(ext_at(i));
                    tab.set_active(i == active);
                }
                drop(tabs);
                sync_rename_overlay(handle, ids);
                return;
            }
        }

        // SAFETY: `removeFromSuperview` is a main-thread mutator.
        {
            let mut tabs = handle.tabs.borrow_mut();
            for old in tabs.drain(..) {
                unsafe { appkit::send_v(old.as_id(), sel!(removeFromSuperview)) };
            }
        }
        let mut new_tabs: Vec<Retained<TabView>> = Vec::with_capacity(n);
        for (i, title) in titles.iter().enumerate() {
            let (cx, cw) = cells[i];
            let frame = rect(cx, 0.0, cw, strip_h);
            let tab = TabView::build(
                mtm,
                handle.proxy.clone(),
                handle.window,
                id_at(i),
                i,
                n,
                title,
                tooltip_at(i),
                i == active,
                metadata_at(i),
                ext_at(i),
                frame,
                geometry_at(i, cx),
            );
            let Some(tab) = tab else { continue };
            // SAFETY: `addSubview:` on the live container, main thread.
            unsafe { appkit::send_v_id(container, sel!(addSubview:), tab.as_id()) };
            new_tabs.push(tab);
        }
        *handle.tabs.borrow_mut() = new_tabs;
        sync_rename_overlay(handle, ids);
    }

    /// Plan chip `(x, width)` cells, traffic-light metrics, and strip height for `n` chips.
    fn plan_strip(handle: &ToolbarHandle, n: usize, active: usize) -> StripPlan {
        let container = handle.container.id();
        // SAFETY: `container` is the live retained strip view; `-bounds` is a
        // side-effect-free `-(NSRect)` getter.
        let (metrics, strip_h) = unsafe {
            (
                strip_metrics(container),
                appkit::send_rect(container, sel!(bounds))
                    .size
                    .height
                    .max(STRIP_HEIGHT),
            )
        };

        let left = STRIP_LEADING_PAD.max(metrics.lights_right + LIGHTS_CLEARANCE);
        let cluster = TAB_GAP + PLUS_WIDTH + TRAILING_PAD;
        // SAFETY: `-frame` is a side-effect-free `-(NSRect)` getter.
        let total_w = unsafe { appkit::send_rect(container, sel!(frame)) }
            .size
            .width
            .max(left + cluster + 1.0);
        let slot = PillSlot {
            share: pill_share((total_w - left - cluster).max(0.0)),
            right: total_w - cluster,
            strip_h,
            center_y: metrics.center_y,
        };
        handle.pill_slot.set(Some(slot));
        let pill_w = place_pill(handle, slot);
        handle.pill_w.set(pill_w);
        let reserved = if pill_w > 0.0 { pill_w + TAB_GAP } else { 0.0 };
        let band_w = (total_w - left - cluster - reserved).max(1.0);
        let active = active.min(n.saturating_sub(1));

        // SAFETY: main-thread geometry setter on the retained live button.
        unsafe {
            appkit::send_v_rect(
                handle.plus.as_id(),
                sel!(setFrame:),
                rect(
                    total_w - TRAILING_PAD - PLUS_WIDTH,
                    0.0,
                    PLUS_WIDTH,
                    strip_h,
                ),
            );
        }
        handle.plus.set_center_y(metrics.center_y);

        let cells = native_tab_cells(band_w, n, active)
            .into_iter()
            .map(|(x, width)| (left + x, width))
            .collect::<Vec<_>>();
        StripPlan {
            cells,
            metrics,
            strip_h,
        }
    }

    /// Reflow live tab chips in place to match the container's current width.
    pub fn reflow_window_tabs(handle: &ToolbarHandle) {
        if MainThread::new().is_none() {
            return;
        }
        let tabs = handle.tabs.borrow();
        let n = tabs.len();
        if n == 0 {
            return;
        }
        let active = tabs.iter().position(|tab| tab.is_active()).unwrap_or(0);
        let StripPlan {
            cells,
            metrics,
            strip_h,
        } = plan_strip(handle, n, active);
        let mut moved = false;
        for (i, (tab, &(cx, cw))) in tabs.iter().zip(&cells).enumerate() {
            // SAFETY: `-frame` is a side-effect-free `-(NSRect)` getter on a live retained chip.
            let f = unsafe { appkit::send_rect(tab.as_id(), sel!(frame)) };
            let reframe = f.origin.x != cx || f.size.width != cw || f.size.height != strip_h;
            if reframe {
                // SAFETY: main-thread geometry setter on a live retained chip.
                unsafe {
                    appkit::send_v_rect(tab.as_id(), sel!(setFrame:), rect(cx, 0.0, cw, strip_h));
                }
                moved = true;
            }
            tab.set_geometry(metrics.geometry(i, n, active, cx));
            if reframe {
                tab.relayout();
            }
        }
        if moved {
            let ids: Vec<TabId> = tabs.iter().map(|tab| tab.tab_id()).collect();
            drop(tabs);
            sync_rename_overlay(handle, &ids);
        }
    }

    /// Format the `toolbar-tabs` introspection line from live [`TabView`]s.
    pub(crate) fn read_tab_chrome(handle: &ToolbarHandle) -> Option<String> {
        MainThread::new()?;
        let tabs = handle.tabs.borrow();
        if tabs.is_empty() {
            return None;
        }
        let active = tabs.iter().position(|t| t.is_active()).unwrap_or(0);
        let labels = tabs.iter().map(|tab| tab.label_text()).collect::<Vec<_>>();
        let metadata = tabs.iter().map(|tab| tab.metadata()).collect::<Vec<_>>();
        let tooltips = tabs.iter().map(|tab| tab.tooltip()).collect::<Vec<_>>();
        format_tab_chrome(&labels, &metadata, &tooltips, active)
    }

    /// Format per-tab context-menu introspection lines from live [`TabView`]s.
    #[must_use]
    pub(crate) fn read_tab_menus(handle: &ToolbarHandle) -> Vec<String> {
        if MainThread::new().is_none() {
            return Vec::new();
        }
        handle
            .tabs
            .borrow()
            .iter()
            .enumerate()
            .map(|(i, t)| t.menu_line(i))
            .collect()
    }

    /// Callback state for [`TabRenameTarget`].
    pub(crate) struct RenameIvars {
        /// The `Wake` channel every outcome relays through.
        proxy: EventLoopProxy<Wake>,
        /// The window whose strip owns the editor.
        window: WindowId,
        /// The session being renamed, captured by `App` at begin from the tab's
        /// focused pane; the commit is validated against it.
        session: u64,
        /// Byte cap for the edited field, enforced live (`controlTextDidChange:`).
        cap: usize,
        /// Escape was pressed: the next end-of-editing is a cancel, not a commit.
        cancelled: Cell<bool>,
        /// Whether this editor has reported its outcome. AppKit can end editing more
        /// than once around teardown, so the latch guarantees exactly one Commit or
        /// Cancel.
        done: Cell<bool>,
    }

    aterm_objc::declare_class! {
        /// Delegate relaying inline rename text-field callbacks into `Wake` events.
        pub(crate) struct TabRenameTarget: NSObject {
            const NAME: &str = "ATermTabRenameTarget";
            type Ivars = RenameIvars;
            protocols: [NSObject, NSTextFieldDelegate];

            /// Sanitize and clamp the edited text as the user types.
            @sel(controlTextDidChange:)
            fn control_text_did_change(&self, notification: Id) {
                // SAFETY: `-object` on the live NSNotification is the edited NSControl;
                // `-stringValue` / `-setStringValue:` are its main-thread accessors.
                unsafe {
                    let field = appkit::send_id(notification, sel!(object));
                    if field.is_null() {
                        return;
                    }
                    let current =
                        appkit::nsstring_to_rust(appkit::send_id(field, sel!(stringValue)));
                    let canonical = crate::session_timeline::sanitize_presentation_line(
                        &current,
                        self.ivars().cap,
                    );
                    if current.trim() != canonical
                        && let Some(ns) = appkit::nsstring(&canonical)
                    {
                        appkit::send_v_id(field, sel!(setStringValue:), ns.id());
                    }
                }
            }

            /// Intercept Escape to cancel the rename edit.
            @sel(control:textView:doCommandBySelector:)
            fn control_text_view_do_command(
                &self,
                control: Id,
                _text_view: Id,
                command: aterm_objc::Sel,
            ) -> aterm_objc::Bool {
                if command != sel!(cancelOperation:) {
                    return aterm_objc::Bool::NO;
                }
                let ivars = self.ivars();
                ivars.cancelled.set(true);
                if !ivars.done.replace(true) {
                    let _ = ivars.proxy.send_event(Wake::CancelSessionRename {
                        window: ivars.window,
                        session: ivars.session,
                    });
                }
                let _ = control;
                aterm_objc::Bool::YES
            }

            /// Commit or cancel the rename when editing ends.
            @sel(controlTextDidEndEditing:)
            fn control_text_did_end_editing(&self, notification: Id) {
                if self.ivars().done.replace(true) {
                    return;
                }
                let ivars = self.ivars();
                if ivars.cancelled.get() {
                    let _ = ivars.proxy.send_event(Wake::CancelSessionRename {
                        window: ivars.window,
                        session: ivars.session,
                    });
                    return;
                }
                // SAFETY: `-object` on the live NSNotification is the edited NSControl, whose `-stringValue` is a plain getter.
                let text = unsafe {
                    let field = appkit::send_id(notification, sel!(object));
                    appkit::nsstring_to_rust(if field.is_null() {
                        Id::NIL
                    } else {
                        appkit::send_id(field, sel!(stringValue))
                    })
                };
                let _ = ivars.proxy.send_event(Wake::CommitSessionRename {
                    window: ivars.window,
                    session: ivars.session,
                    text,
                });
            }
        }
    }

    impl TabRenameTarget {
        fn build(
            mtm: MainThread,
            proxy: EventLoopProxy<Wake>,
            window: WindowId,
            session: u64,
            cap: usize,
        ) -> Option<Retained<Self>> {
            Self::alloc_init(
                mtm,
                RenameIvars {
                    proxy,
                    window,
                    session,
                    cap,
                    cancelled: Cell::new(false),
                    done: Cell::new(false),
                },
            )
        }
    }

    /// Whether `handle` has a live window that can host an inline rename editor.
    pub(crate) fn can_present_tab_rename(handle: &ToolbarHandle) -> bool {
        // SAFETY: `-window` is a side-effect-free `-(id)` getter on the live retained container view.
        MainThread::new().is_some()
            && !unsafe { appkit::send_id(handle.container.id(), sel!(window)) }.is_null()
    }

    /// Open the inline tab-rename text field over `tab`.
    pub(crate) fn begin_tab_rename(
        handle: &ToolbarHandle,
        tab: TabId,
        session: u64,
        seed: &str,
        placeholder: &str,
    ) -> bool {
        let Some(mtm) = MainThread::new() else {
            return false;
        };
        end_tab_rename(handle);
        let frame = {
            let tabs = handle.tabs.borrow();
            let Some(chip) = tabs.iter().find(|t| t.tab_id() == tab) else {
                return false;
            };
            chip.editor_frame()
        };
        let Some(target) = TabRenameTarget::build(
            mtm,
            handle.proxy.clone(),
            handle.window,
            session,
            crate::session_timeline::MetaField::Title.cap(),
        ) else {
            return false;
        };
        // SAFETY: `textFieldWithString:` is the documented non-raising factory; the
        // rest are plain setters on the fresh field, all on the main thread.
        let Some(field) = (unsafe {
            let Some(seed_ns) = appkit::nsstring(seed) else {
                return false;
            };
            let Some(field) = Obj::retain(appkit::send_id_id(
                class(c"NSTextField").as_id(),
                sel!(textFieldWithString:),
                seed_ns.id(),
            )) else {
                return false;
            };
            appkit::send_v_rect(field.id(), sel!(setFrame:), frame);
            appkit::send_v_bool(field.id(), sel!(setEditable:), true);
            appkit::send_v_bool(field.id(), sel!(setSelectable:), true);
            appkit::send_v_bool(field.id(), sel!(setBezeled:), true);
            appkit::send_v_bool(field.id(), sel!(setDrawsBackground:), true);
            appkit::send_v_bool(field.id(), sel!(setUsesSingleLineMode:), true);
            appkit::send_v_isize(
                field.id(),
                sel!(setAlignment:),
                consts::NS_TEXT_ALIGNMENT_LEFT,
            );
            appkit::send_v_id(field.id(), sel!(setFont:), system_font(12.0, false));
            if !placeholder.is_empty()
                && let Some(ns) = appkit::nsstring(placeholder)
            {
                appkit::send_v_id(field.id(), sel!(setPlaceholderString:), ns.id());
            }
            appkit::send_v_id_isize_id(
                handle.container.id(),
                sel!(addSubview:positioned:relativeTo:),
                field.id(),
                consts::NS_WINDOW_ABOVE,
                Id::NIL,
            );
            Some(field)
        }) else {
            return false;
        };
        // SAFETY: `-window` is `-(id)`; `-makeFirstResponder:` is `-(BOOL)(NSResponder *)` and an
        // `NSTextField` is an `NSResponder`.
        let focused = unsafe {
            let w = appkit::send_id(handle.container.id(), sel!(window));
            !w.is_null() && appkit::send_bool_id(w, sel!(makeFirstResponder:), field.id())
        };
        if !focused {
            // SAFETY: main-thread teardown of the field we just added.
            unsafe { appkit::send_v(field.id(), sel!(removeFromSuperview)) };
            return false;
        }
        // SAFETY: `-selectText:` is `-(void)(id)` on the main thread.
        unsafe { appkit::send_v_id(field.id(), sel!(selectText:), Id::NIL) };
        // SAFETY: `-setDelegate:` is `-(void)(id)` and holds the delegate weakly, which is why `RenameEditor` retains it below.
        unsafe { appkit::send_v_id(field.id(), sel!(setDelegate:), target.as_id()) };
        *handle.rename.borrow_mut() = Some(RenameEditor { field, target, tab });
        true
    }

    /// Close the inline tab-rename editor and restore first responder to the terminal.
    pub(crate) fn end_tab_rename(handle: &ToolbarHandle) {
        let Some(editor) = handle.rename.borrow_mut().take() else {
            return;
        };
        editor.target.ivars().done.set(true);
        // SAFETY: main-thread responder restore, then teardown;
        // `-setDelegate: nil` drops AppKit's weak reference before the relay's
        // last strong reference goes away here.
        unsafe {
            let window = appkit::send_id(handle.container.id(), sel!(window));
            if !window.is_null() {
                let _ = appkit::send_bool_id(
                    window,
                    sel!(makeFirstResponder:),
                    handle.content_view.id(),
                );
            }
            appkit::send_v_id(editor.field.id(), sel!(setDelegate:), Id::NIL);
            appkit::send_v(editor.field.id(), sel!(removeFromSuperview));
        }
    }

    /// Current text of the active inline rename editor, if open.
    pub(crate) fn rename_editor_text(handle: &ToolbarHandle) -> Option<String> {
        let rename = handle.rename.borrow();
        let editor = rename.as_ref()?;
        // SAFETY: `-stringValue` is a plain `-(NSString *)` main-thread value getter on the live field, and the result is +0 — copied out here.
        Some(unsafe {
            appkit::nsstring_to_rust(appkit::send_id(editor.field.id(), sel!(stringValue)))
        })
    }

    /// Dispatch an editing action (`Cut`, `Copy`, `Paste`, `SelectAll`) to the active rename editor.
    pub(crate) fn rename_editor_edit(
        handle: &ToolbarHandle,
        action: crate::platform::RenameEditorEdit,
    ) -> bool {
        use crate::platform::RenameEditorEdit;
        let rename = handle.rename.borrow();
        let Some(editor) = rename.as_ref() else {
            return false;
        };
        // SAFETY: `currentEditor` is a plain main-thread getter; the returned NSText
        // implements the standard editing actions, each taking a `sender`.
        unsafe {
            let text = appkit::send_id(editor.field.id(), sel!(currentEditor));
            if text.is_null() {
                return false;
            }
            let action = match action {
                RenameEditorEdit::Copy => sel!(copy:),
                RenameEditorEdit::Paste => sel!(paste:),
                RenameEditorEdit::SelectAll => sel!(selectAll:),
            };
            appkit::send_v_id(text, action, Id::NIL);
        }
        true
    }

    /// Keep a live rename editor aligned with its tab across a strip refresh.
    fn sync_rename_overlay(handle: &ToolbarHandle, ids: &[TabId]) {
        let outcome = {
            let rename = handle.rename.borrow();
            let Some(editor) = rename.as_ref() else {
                return;
            };
            let tabs = handle.tabs.borrow();
            ids.iter()
                .position(|id| *id == editor.tab)
                .and_then(|i| tabs.get(i))
                .map(|chip| chip.editor_frame())
                .ok_or(editor.target.ivars().session)
        };
        let frame = match outcome {
            Ok(frame) => frame,
            Err(session) => {
                let window = handle.window;
                end_tab_rename(handle);
                let _ = handle
                    .proxy
                    .send_event(Wake::CancelSessionRename { window, session });
                return;
            }
        };
        let rename = handle.rename.borrow();
        let Some(editor) = rename.as_ref() else {
            return;
        };
        // SAFETY: main-thread geometry setter and re-insert of a view already in this container.
        unsafe {
            appkit::send_v_rect(editor.field.id(), sel!(setFrame:), frame);
            appkit::send_v_id_isize_id(
                handle.container.id(),
                sel!(addSubview:positioned:relativeTo:),
                editor.field.id(),
                consts::NS_WINDOW_ABOVE,
                Id::NIL,
            );
        }
    }

    /// Runtime tests for this module's five declared classes: each asks the
    /// Objective-C runtime or Foundation what was registered, and behavioural tests
    /// dispatch through machinery other than the code under test.
    #[cfg(test)]
    mod objc_tests {
        use aterm_objc::{
            Bool, ClassType, Encode, Id, Obj, Sel, autoreleasepool, checked_msg, class, class_name,
            method_types, sel,
        };

        use super::{
            ChromeButton, NeedsPill, STRIP_ITEM_ID, TabRenameTarget, TabView, ToolbarDelegate,
        };
        use crate::appkit;

        fn bool_enc() -> &'static str {
            Bool::ENCODING
        }

        fn a_view() -> Obj {
            // SAFETY: `+alloc`/`-init` on `NSView` gives a +1 zero-frame view;
            // `Obj::from_owned` adopts that one +1.
            unsafe { Obj::from_owned(appkit::send_id(appkit::alloc(class(c"NSView")), sel!(init))) }
                .expect("NSView")
        }

        #[test]
        fn all_five_declared_classes_register_as_they_did() {
            for (cls, name, superclass) in [
                (ChromeButton::class(), c"ATermChromeButton", c"NSView"),
                (NeedsPill::class(), c"ATermNeedsPill", c"NSView"),
                (TabView::class(), c"ATermTabView", c"NSView"),
                (
                    ToolbarDelegate::class(),
                    c"ATermToolbarDelegate",
                    c"NSObject",
                ),
                (
                    TabRenameTarget::class(),
                    c"ATermTabRenameTarget",
                    c"NSObject",
                ),
            ] {
                assert!(!cls.is_null());
                assert_eq!(class(name), cls, "objc_getClass disagrees for {name:?}");
                // SAFETY: `cls` is a class this module registered, so it and its superclass are live, immortal class objects.
                unsafe {
                    assert_eq!(class_name(cls), name);
                    assert_eq!(class_name(aterm_objc::superclass_of(cls)), superclass);
                }
            }
        }

        #[test]
        fn the_toolbar_delegate_conforms_to_nstoolbardelegate() {
            let cls = ToolbarDelegate::class();
            let proto = aterm_objc::protocol(c"NSToolbarDelegate");
            assert!(
                !proto.is_null()
                    && !aterm_objc::protocols_registered_by_aterm().contains(&c"NSToolbarDelegate"),
                "this host's AppKit registers NSToolbarDelegate — not one aterm had to supply"
            );
            // SAFETY: `+conformsToProtocol:` and `+instancesRespondToSelector:` are side-effect-free NSObject queries on a live class object.
            unsafe {
                assert!(
                    appkit::send_bool_id(
                        cls.as_id(),
                        sel!(conformsToProtocol:),
                        Id::from_ptr(proto.as_ptr()),
                    ),
                    "the declared class does not answer NSToolbarDelegate"
                );
                for s in [
                    sel!(toolbarDefaultItemIdentifiers:),
                    sel!(toolbarAllowedItemIdentifiers:),
                    sel!(toolbar:itemForItemIdentifier:willBeInsertedIntoToolbar:),
                ] {
                    assert!(appkit::send_bool_sel(
                        cls.as_id(),
                        sel!(instancesRespondToSelector:),
                        s
                    ));
                }
            }
        }

        #[test]
        fn the_rename_rows_encode_to_the_protocol_authority() {
            let cls = TabRenameTarget::class();
            let proto = aterm_objc::protocol(c"NSTextFieldDelegate");
            assert!(
                !proto.is_null()
                    && !aterm_objc::protocols_registered_by_aterm()
                        .contains(&c"NSTextFieldDelegate"),
                "this host's AppKit registers NSTextFieldDelegate — not one aterm had to supply"
            );
            // SAFETY: `+conformsToProtocol:` is a side-effect-free `NSObject` query on a live class object.
            assert!(
                unsafe {
                    appkit::send_bool_id(
                        cls.as_id(),
                        sel!(conformsToProtocol:),
                        Id::from_ptr(proto.as_ptr()),
                    )
                },
                "the declared class does not answer NSTextFieldDelegate"
            );
            let mut checked = 0usize;
            for s in [
                sel!(controlTextDidChange:),
                sel!(controlTextDidEndEditing:),
                sel!(control:textView:doCommandBySelector:),
            ] {
                // SAFETY: `proto` is a live protocol object, `cls` a live class this module registered, and both queries are read-only.
                let (want, got) = unsafe {
                    (
                        aterm_objc::protocol_method_types(proto, s, true),
                        method_types(cls, s),
                    )
                };
                let want = want
                    .as_deref()
                    .map(aterm_objc::strip_method_offsets)
                    .unwrap_or_else(|| panic!("{s:?} is not declared by NSTextFieldDelegate"));
                assert_eq!(got.as_deref(), Some(want.as_str()), "{s:?}");
                checked += 1;
            }
            assert_eq!(checked, 3, "every rename row was read from the authority");
        }

        #[test]
        fn the_runtime_reports_every_encoding_the_table_says() {
            let b = bool_enc();
            let rect = "{CGRect={CGPoint=dd}{CGSize=dd}}";
            let cases: Vec<(aterm_objc::ClassPtr, Sel, String)> = vec![
                (ChromeButton::class(), sel!(drawRect:), format!("v@:{rect}")),
                (ChromeButton::class(), sel!(mouseDown:), "v@:@".into()),
                (ChromeButton::class(), sel!(mouseDragged:), "v@:@".into()),
                (ChromeButton::class(), sel!(mouseUp:), "v@:@".into()),
                (ChromeButton::class(), sel!(mouseEntered:), "v@:@".into()),
                (ChromeButton::class(), sel!(mouseExited:), "v@:@".into()),
                (
                    ChromeButton::class(),
                    sel!(updateTrackingAreas),
                    "v@:".into(),
                ),
                (
                    ChromeButton::class(),
                    sel!(acceptsFirstMouse:),
                    format!("{b}@:@"),
                ),
                (
                    ChromeButton::class(),
                    sel!(accessibilityPerformPress),
                    format!("{b}@:"),
                ),
                (ChromeButton::class(), sel!(dealloc), "v@:".into()),
                (NeedsPill::class(), sel!(drawRect:), format!("v@:{rect}")),
                (NeedsPill::class(), sel!(mouseDown:), "v@:@".into()),
                (NeedsPill::class(), sel!(mouseUp:), "v@:@".into()),
                (
                    NeedsPill::class(),
                    sel!(acceptsFirstMouse:),
                    format!("{b}@:@"),
                ),
                (
                    NeedsPill::class(),
                    sel!(accessibilityPerformPress),
                    format!("{b}@:"),
                ),
                (NeedsPill::class(), sel!(dealloc), "v@:".into()),
                (TabView::class(), sel!(drawRect:), format!("v@:{rect}")),
                (TabView::class(), sel!(mouseDown:), "v@:@".into()),
                (TabView::class(), sel!(rightMouseDown:), "v@:@".into()),
                (TabView::class(), sel!(mouseDragged:), "v@:@".into()),
                (TabView::class(), sel!(mouseUp:), "v@:@".into()),
                (TabView::class(), sel!(mouseEntered:), "v@:@".into()),
                (TabView::class(), sel!(mouseExited:), "v@:@".into()),
                (TabView::class(), sel!(updateTrackingAreas), "v@:".into()),
                (
                    TabView::class(),
                    sel!(acceptsFirstMouse:),
                    format!("{b}@:@"),
                ),
                (
                    TabView::class(),
                    sel!(accessibilityPerformPress),
                    format!("{b}@:"),
                ),
                (TabView::class(), sel!(closeTab:), "v@:@".into()),
                (TabView::class(), sel!(tabMenuAction:), "v@:@".into()),
                (TabView::class(), sel!(tabMenuConnection:), "v@:@".into()),
                (TabView::class(), sel!(dealloc), "v@:".into()),
                (
                    ToolbarDelegate::class(),
                    sel!(toolbarDefaultItemIdentifiers:),
                    "@@:@".into(),
                ),
                (
                    ToolbarDelegate::class(),
                    sel!(toolbarAllowedItemIdentifiers:),
                    "@@:@".into(),
                ),
                (
                    ToolbarDelegate::class(),
                    sel!(toolbar:itemForItemIdentifier:willBeInsertedIntoToolbar:),
                    format!("@@:@@{b}"),
                ),
                (ToolbarDelegate::class(), sel!(dealloc), "v@:".into()),
                (
                    TabRenameTarget::class(),
                    sel!(controlTextDidChange:),
                    "v@:@".into(),
                ),
                (
                    TabRenameTarget::class(),
                    sel!(control:textView:doCommandBySelector:),
                    format!("{b}@:@@:"),
                ),
                (
                    TabRenameTarget::class(),
                    sel!(controlTextDidEndEditing:),
                    "v@:@".into(),
                ),
                (TabRenameTarget::class(), sel!(dealloc), "v@:".into()),
            ];
            for (cls, s, want) in cases {
                // SAFETY: every `cls` is a live class this module registered.
                let got = unsafe { method_types(cls, s) };
                assert_eq!(
                    got.as_deref(),
                    Some(want.as_str()),
                    "{s:?} on {:?}",
                    // SAFETY: as above.
                    unsafe { class_name(cls) }
                );
            }
            #[cfg(target_arch = "aarch64")]
            {
                assert_eq!(b, "B");
                // SAFETY: live registered class.
                assert_eq!(
                    unsafe {
                        method_types(
                            ToolbarDelegate::class(),
                            sel!(toolbar:itemForItemIdentifier:willBeInsertedIntoToolbar:),
                        )
                    }
                    .as_deref(),
                    Some("@@:@@B")
                );
                // SAFETY: live registered class.
                assert_eq!(
                    unsafe {
                        method_types(
                            TabRenameTarget::class(),
                            sel!(control:textView:doCommandBySelector:),
                        )
                    }
                    .as_deref(),
                    Some("B@:@@:")
                );
            }
        }

        #[test]
        fn the_identifier_methods_vend_exactly_the_strip_item() {
            let delegate =
                ToolbarDelegate::new(crate::appkit::test_witness(), a_view()).expect("delegate");
            autoreleasepool(|_| {
                // SAFETY: `-performSelector:withObject:` is the runtime's own dispatch, `-count` is `-(NSUInteger)` and `-objectAtIndex:` is `-(id)(NSUInteger)` on the returned NSArray, which is alive for this pool.
                unsafe {
                    for s in [
                        sel!(toolbarDefaultItemIdentifiers:),
                        sel!(toolbarAllowedItemIdentifiers:),
                    ] {
                        let perform: unsafe extern "C-unwind" fn(Id, Sel, Sel, Id) -> Id =
                            checked_msg();
                        let arr = perform(
                            delegate.as_id(),
                            sel!(performSelector:withObject:),
                            s,
                            Id::NIL,
                        );
                        assert!(!arr.is_null(), "{s:?} returned nil");
                        assert_eq!(appkit::send_usize(arr, sel!(count)), 1);
                        let first: unsafe extern "C-unwind" fn(Id, Sel, usize) -> Id =
                            checked_msg();
                        assert_eq!(
                            appkit::nsstring_to_rust(first(arr, sel!(objectAtIndex:), 0)),
                            STRIP_ITEM_ID
                        );
                    }
                }
            });
        }
    }
}
