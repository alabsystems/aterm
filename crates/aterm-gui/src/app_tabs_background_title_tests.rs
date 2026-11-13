// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::{
    background_title_refresh_needed, resolved_terminal_title_rung, synchronize_captured_title_cache,
};

#[test]
fn spinner_epochs_coalesce_before_tab_refresh() {
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
    let mut epoch = None;
    let mut chrome: Option<String> = None;
    let mut refreshes = 0;

    for index in 0..100_u64 {
        let next = frames[index as usize % frames.len()];
        let live_epoch = index + 1;
        if background_title_refresh_needed(epoch, chrome.as_deref(), live_epoch, next) {
            chrome = Some(next.to_string());
            refreshes += 1;
        }
        epoch = Some(live_epoch);
    }

    assert_eq!(
        refreshes, 1,
        "spinner phases must not rebuild all tab chrome"
    );
    assert_eq!(chrome.as_deref(), Some("⠋ aterm"));
    assert!(!background_title_refresh_needed(
        epoch,
        chrome.as_deref(),
        epoch.unwrap(),
        "⠙ project",
    ));
    assert!(background_title_refresh_needed(
        epoch,
        chrome.as_deref(),
        epoch.unwrap() + 1,
        "⠙ project",
    ));
    assert!(background_title_refresh_needed(
        epoch,
        chrome.as_deref(),
        epoch.unwrap() + 1,
        "aterm",
    ));
}

#[test]
fn a_console_program_path_falls_through_to_the_cwd() {
    let pwsh =
        "C:\\Program Files\\WindowsApps\\Microsoft.PowerShell_7.6.6.0_x64__8wekyb3d8bbwe\\pwsh.exe";
    assert_eq!(
        resolved_terminal_title_rung(None, pwsh, Some("/srv/demo-cwd")).as_deref(),
        Some("/srv/demo-cwd")
    );
    assert_eq!(resolved_terminal_title_rung(None, pwsh, None), None);
    assert_eq!(
        resolved_terminal_title_rung(
            None,
            "C:\\Windows\\system32\\cmd.exe - ping build-01",
            Some("/srv/demo-cwd")
        )
        .as_deref(),
        Some("ping build-01")
    );
    assert_eq!(
        resolved_terminal_title_rung(None, "~\\aterm", Some("/srv/demo-cwd")).as_deref(),
        Some("~\\aterm")
    );
}

#[test]
fn captured_drift_snapshot_wins_a_contended_refresh_read() {
    let captured = |title: &str, untitled: bool| super::CapturedTitle {
        title: title.to_owned(),
        untitled,
    };
    let mut cache = std::collections::HashMap::from([(7, captured("stale", false))]);

    synchronize_captured_title_cache(&mut cache, 7, Some("fresh"), true, true);
    assert_eq!(cache.get(&7), Some(&captured("fresh", true)));

    synchronize_captured_title_cache(&mut cache, 7, Some("⠙ fresh"), false, false);
    assert_eq!(cache.get(&7), Some(&captured("fresh", true)));

    synchronize_captured_title_cache(&mut cache, 7, None, false, true);
    assert!(!cache.contains_key(&7));
}
