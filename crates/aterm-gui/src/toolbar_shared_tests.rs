// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

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
    assert!(line.contains(r#"["selected", "dirty", "busy", "attention", "conn-out", "conn-in"]"#));
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
fn system_state_help_names_system_state_not_settings() {
    let help = tab_help(
        "System State",
        None,
        TabStripMetadata {
            icon: Some(crate::tab_bar::TabIconKind::SystemState),
            dirty: false,
            busy: false,
            attention: crate::tab_bar::ChipLevel::Off,
            conn: None,
            closable: true,
            team: crate::tab_bar::TabTeam::NONE,
            load: 0,
        },
        0,
    );
    assert!(!help.contains("Settings"), "{help}");
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
