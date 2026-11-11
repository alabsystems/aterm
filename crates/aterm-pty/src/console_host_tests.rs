// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn the_config_value_names_the_choice_and_absent_is_the_bundled_host() {
    for absent in [
        None,
        Some(""),
        Some("  "),
        Some("bundled"),
        Some(" Bundled "),
    ] {
        assert_eq!(
            ConsoleHostChoice::from_config(absent),
            ConsoleHostChoice::Bundled,
            "{absent:?}"
        );
    }
    assert_eq!(
        ConsoleHostChoice::from_config(Some("Inbox")),
        ConsoleHostChoice::Inbox
    );
    assert_eq!(
        ConsoleHostChoice::from_config(Some(" AUTO ")),
        ConsoleHostChoice::Auto
    );
    assert_eq!(
        ConsoleHostChoice::from_config(Some(r"C:\tools\OpenConsole.exe")),
        ConsoleHostChoice::Path(PathBuf::from(r"C:\tools\OpenConsole.exe"))
    );
}

#[test]
fn only_openconsole_passes_screen_switches_through() {
    assert!(ConsoleHostKind::OpenConsole.passes_screen_switches_through());
    assert!(!ConsoleHostKind::Inbox.passes_screen_switches_through());
}
