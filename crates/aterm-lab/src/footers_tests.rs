// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn the_mode_row_names_its_mode() {
    assert_eq!(
        mode_word("x\n  ⏵⏵ auto mode on (shift+tab to cycle) · ← for agents\n"),
        Some("auto")
    );
    assert_eq!(
        mode_word("  ⏸ plan mode on (shift+tab to cycle)"),
        Some("plan")
    );
    assert_eq!(
        mode_word("  ⏵⏵ accept edits on (shift+tab to cycle)"),
        Some("accept-edits")
    );
    assert_eq!(mode_word("  ⏵⏵ something new on"), None);
    assert_eq!(mode_word("no footer"), None);
}

#[test]
fn a_cycle_from_bypass_numbers_each_capture_by_its_press() {
    assert_eq!(Cycle::Launch.name(2, "plan"), "footer-plan");
    assert_eq!(
        Cycle::FromBypass.name(1, "manual"),
        "footer-bypass-then-1-manual"
    );
    assert_eq!(Cycle::Launch.again(5, "auto"), "footer-auto-again");
    assert_eq!(
        Cycle::FromBypass.again(5, "bypass"),
        "footer-bypass-then-5-bypass-again"
    );
}

/// The pattern a press waits on is the mode row's, as a literal, never the effort hint
/// Claude wraps onto a row of its own under it (2.1.316's bypass-idle screen at 80 columns),
/// which a press can leave standing while the mode changes.
#[test]
fn the_mode_row_is_the_pattern_a_press_waits_on() {
    let screen = "❯ \n──────\n  ⏸ manual mode on · ? for shortcuts · ← for agents\n\n   \n";
    assert_eq!(
        mode_row_pattern(screen).as_deref(),
        Some("⏸.manual.mode.on.·.\\?.for.shortcuts.·.←.for.agents")
    );
    let wrapped = "❯ \n──────\n  ⏵⏵ bypass permissions on (shift+tab to cycle) · ← for agents\n                                                              ◐ medium · /effort\n";
    let pattern = mode_row_pattern(wrapped).expect("the mode row");
    assert_eq!(
        pattern,
        "⏵⏵.bypass.permissions.on.\\(shift\\+tab.to.cycle\\).·.←.for.agents"
    );
    let re = aterm_regex::Regex::new(&pattern).unwrap();
    assert!(!re.is_match("◐ medium · /effort"), "never the effort row");
    assert!(
        !re.is_match("  ⏵⏵ auto mode on (shift+tab to cycle)  ◐ medium · /effort"),
        "gone once the mode changes, the hint kept"
    );
    assert_eq!(mode_row_pattern("\n  \n"), None);
}

#[test]
fn the_done_row_pattern_reads_what_the_done_row_reader_reads() {
    let re = aterm_regex::Regex::new(DONE_ROW).unwrap();
    let wired = aterm_regex::Regex::new(&crate::door::wire_pattern(DONE_ROW)).unwrap();
    for row in [
        "✻ Worked for 3m 2s · done 11:08 PM",
        "✻ Baked for 0s · done 5:39 AM",
        "✻ Worked hard for 3s",
        "✻  for 1s",
        "✻ Worked for ever and for 9s",
    ] {
        assert!(re.is_match(row) && crate::screen::is_done_row(row), "{row}");
        assert!(wired.is_match(row), "{row}");
    }
    for not_done in [
        "✻ Working… (3s · ↓ 12 tokens)",
        "x ✻aWorkedXforY1",
        "  ✻ Worked for 3s",
        "✻\tWorked for 3s",
        "✻ Worked for ever",
    ] {
        assert!(
            !re.is_match(not_done) && !crate::screen::is_done_row(not_done),
            "{not_done}"
        );
        assert!(!wired.is_match(not_done), "{not_done}");
    }
}

#[test]
fn mode_rows_are_read_oldest_first() {
    let transcript = concat!(
        "{\"type\":\"user\",\"note\":\"permission-mode\"}\n",
        "{\"type\":\"permission-mode\",\"permissionMode\":\"auto\"}\n",
        "not json \"permission-mode\"\n",
        "{\"type\":\"permission-mode\",\"permissionMode\":\"bypassPermissions\"}\n",
    );
    assert_eq!(mode_rows_in(transcript), ["auto", "bypassPermissions"]);
}

#[test]
fn the_variant_is_parsed_and_named() {
    let a = |v: &[&str]| v.iter().map(|s| (*s).to_string()).collect::<Vec<_>>();
    let o = Opts::parse(&a(&[])).unwrap();
    assert!(!o.subscriber && o.settings.is_none() && o.turn && o.bypass);
    assert_eq!(o.variant(), "the lab's default settings");
    let o = Opts::parse(&a(&[
        "--subscriber",
        "--settings",
        r#"{"effortLevel":"xhigh"}"#,
        "--no-turn",
    ]))
    .unwrap();
    assert!(o.subscriber && !o.turn);
    assert_eq!(o.variant(), r#"settings {"effortLevel":"xhigh"}"#);
    let o = Opts::parse(&a(&[
        "--launch-args",
        "--allow-dangerously-skip-permissions  --permission-mode plan",
        "--relaunch-args",
        "--continue",
    ]))
    .unwrap();
    assert_eq!(
        o.launch_args,
        a(&[
            "--allow-dangerously-skip-permissions",
            "--permission-mode",
            "plan"
        ])
    );
    assert_eq!(o.relaunch_args, Some(a(&["--continue"])));
    assert!(Opts::parse(&a(&[])).unwrap().relaunch_args.is_none());
    assert_eq!(Opts::parse(&a(&["--settings", "[1]"])).unwrap_err().code, 2);
    assert_eq!(Opts::parse(&a(&["--settings"])).unwrap_err().code, 2);
    assert_eq!(Opts::parse(&a(&["--bogus"])).unwrap_err().code, 2);
}
