// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

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
