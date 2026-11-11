// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

use super::*;

#[cfg(unix)]
#[test]
fn the_marker_names_the_platforms_state_folder_under_the_worlds_home() {
    let home = Path::new("/w/home");
    #[cfg(target_os = "macos")]
    let want = "/w/home/Library/Application Support/aterm";
    #[cfg(not(target_os = "macos"))]
    let want = "/w/home/.local/state/aterm";
    assert_eq!(state_home(home), Some(PathBuf::from(want)));
}

#[test]
fn the_lanes_off_open_tables_only() {
    let first = LANES_OFF.lines().next().unwrap_or_default();
    assert!(first.starts_with('['), "{LANES_OFF}");
}
