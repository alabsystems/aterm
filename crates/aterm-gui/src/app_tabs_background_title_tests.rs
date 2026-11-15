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
        resolved_terminal_title_rung(None, pwsh, false, None, Some("/srv/demo-cwd")).as_deref(),
        Some("/srv/demo-cwd")
    );
    assert_eq!(
        resolved_terminal_title_rung(None, pwsh, false, None, None),
        None
    );
    assert_eq!(
        resolved_terminal_title_rung(
            None,
            "C:\\Windows\\system32\\cmd.exe - ping build-01",
            false,
            None,
            Some("/srv/demo-cwd")
        )
        .as_deref(),
        Some("ping build-01")
    );
    assert_eq!(
        resolved_terminal_title_rung(None, "~\\aterm", false, None, Some("/srv/demo-cwd"))
            .as_deref(),
        Some("~\\aterm")
    );
}

/// THE TASK'S FIRST LINE IS A RUNG BEFORE THE SHELL'S TITLE AND THE FOLDER (tracker ST-02): it
/// names a terminal its program has not titled, under the person's pin. CONTROLS: a title the
/// program gave wins over it, a line where the program titled the tab is not read, and with no
/// line the shell's title and then the folder name it as before.
#[test]
fn the_task_line_comes_before_the_shells_title_and_the_folder() {
    let line = Some("Fix the flaky title test");
    let cwd = Some("/srv/demo-cwd");
    assert_eq!(
        resolved_terminal_title_rung(None, "~/aterm", true, line, cwd).as_deref(),
        line,
        "the shell's prompt title is its folder"
    );
    assert_eq!(
        resolved_terminal_title_rung(None, "", true, line, cwd).as_deref(),
        line
    );
    assert_eq!(
        resolved_terminal_title_rung(Some("release notes"), "~/aterm", true, line, cwd).as_deref(),
        Some("release notes"),
        "the pin wins"
    );
    assert_eq!(
        resolved_terminal_title_rung(None, "refactor-titles", false, line, cwd).as_deref(),
        Some("refactor-titles"),
        "CONTROL: the program's own title wins"
    );
    assert_eq!(
        resolved_terminal_title_rung(None, "~/aterm", true, None, cwd).as_deref(),
        Some("~/aterm"),
        "CONTROL: no line"
    );
    assert_eq!(
        resolved_terminal_title_rung(None, "", true, Some(""), cwd).as_deref(),
        Some("/srv/demo-cwd"),
        "CONTROL: an empty line is no rung"
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
