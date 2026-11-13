// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use std::ffi::OsString;

fn args(list: &[&str]) -> Vec<OsString> {
    list.iter().map(OsString::from).collect()
}

fn echo_dir(raw: &str) -> Result<String, String> {
    Ok(raw.to_string())
}

#[test]
fn the_default_policy_is_new_window() {
    assert_eq!(WindowingBehavior::default(), WindowingBehavior::NewWindow);
    assert_eq!(WindowingBehavior::parse("join-please"), None);
}

#[test]
fn policy_values_and_their_windows_terminal_aliases_parse() {
    for spelling in [
        "attach",
        " Attach ",
        "use_existing",
        "useExisting",
        "existing",
    ] {
        assert_eq!(
            WindowingBehavior::parse(spelling),
            Some(WindowingBehavior::Attach),
            "{spelling}"
        );
    }
    for spelling in ["new_window", "NEW-WINDOW", "useNew", "new"] {
        assert_eq!(
            WindowingBehavior::parse(spelling),
            Some(WindowingBehavior::NewWindow),
            "{spelling}"
        );
    }
}

#[test]
fn routing_is_the_whole_table() {
    use LaunchIntent::{NewTab, NewWindow, Plain, SplitPane};
    use WindowRoute::{Forward, Spawn};
    use WindowingBehavior::{Attach, NewWindow as PolicyNewWindow};
    let cases = [
        (NewTab, PolicyNewWindow, true, Spawn),
        (NewTab, PolicyNewWindow, false, Spawn),
        (NewTab, Attach, true, Forward),
        (NewTab, Attach, false, Spawn),
        (SplitPane, PolicyNewWindow, true, Spawn),
        (SplitPane, PolicyNewWindow, false, Spawn),
        (SplitPane, Attach, true, Forward),
        (SplitPane, Attach, false, Spawn),
        (Plain, PolicyNewWindow, true, Spawn),
        (Plain, PolicyNewWindow, false, Spawn),
        (Plain, Attach, true, Forward),
        (Plain, Attach, false, Spawn),
        (NewWindow, Attach, true, Spawn),
        (NewWindow, Attach, false, Spawn),
        (NewWindow, PolicyNewWindow, true, Spawn),
        (NewWindow, PolicyNewWindow, false, Spawn),
    ];
    for (intent, behavior, reachable, expected) in cases {
        assert_eq!(
            route_launch(intent, behavior, reachable),
            expected,
            "{intent:?} under {behavior:?} with reachable={reachable}"
        );
    }
}

#[test]
fn the_grammar_accepts_every_directory_spelling() {
    for spelling in [
        &["-d", "/tmp/x"][..],
        &["--directory", "/tmp/x"][..],
        &["--working-directory", "/tmp/x"][..],
        &["--startingDirectory", "/tmp/x"][..],
        &["-d=/tmp/x"][..],
        &["--startingDirectory=/tmp/x"][..],
    ] {
        let req = parse_window_request("new-tab", &args(spelling), echo_dir)
            .unwrap_or_else(|e| panic!("{spelling:?}: {e}"));
        assert_eq!(req.intent, LaunchIntent::NewTab);
        assert_eq!(req.dir.as_deref(), Some("/tmp/x"));
    }
}

#[test]
fn the_grammar_maps_each_verb_to_its_intent_and_refuses_the_rest() {
    assert_eq!(
        parse_window_request("new-window", &[], echo_dir)
            .unwrap()
            .intent,
        LaunchIntent::NewWindow
    );
    assert_eq!(
        parse_window_request("split-pane", &[], echo_dir)
            .unwrap()
            .intent,
        LaunchIntent::SplitPane
    );
    assert!(parse_window_request("ctl", &[], echo_dir).is_err());
}

#[test]
fn split_orientation_is_wt_shaped_and_only_on_split_pane() {
    let v = parse_window_request("split-pane", &args(&["-V"]), echo_dir).unwrap();
    assert_eq!(v.split, SplitOrientation::Vertical);
    let h = parse_window_request("split-pane", &args(&["-H"]), echo_dir).unwrap();
    assert_eq!(h.split, SplitOrientation::Horizontal);
    let d = parse_window_request("split-pane", &[], echo_dir).unwrap();
    assert_eq!(d.split, SplitOrientation::Vertical);
    assert!(parse_window_request("new-tab", &args(&["-H"]), echo_dir).is_err());
}

#[test]
fn a_missing_or_unknown_option_is_a_loud_error() {
    assert!(parse_window_request("new-tab", &args(&["-d"]), echo_dir).is_err());
    assert!(parse_window_request("new-tab", &args(&["--profile", "x"]), echo_dir).is_err());
    let bad = parse_window_request("new-tab", &args(&["-d", "nope"]), |_| {
        Err("not a directory: nope".to_string())
    })
    .unwrap_err();
    assert_eq!(bad, "not a directory: nope");
}

#[test]
fn the_control_request_is_the_spawn_verb_with_a_quoted_cwd() {
    let plain = WindowRequest {
        intent: LaunchIntent::NewTab,
        dir: None,
        split: SplitOrientation::Vertical,
    };
    assert_eq!(plain.control_request().unwrap(), "spawn\n");

    let simple = WindowRequest {
        intent: LaunchIntent::NewTab,
        dir: Some(r"C:\Windows".to_string()),
        split: SplitOrientation::Vertical,
    };
    assert_eq!(simple.control_request().unwrap(), "spawn cwd=C:\\Windows\n");

    let spaced = WindowRequest {
        intent: LaunchIntent::NewTab,
        dir: Some(r"C:\Program Files\Git".to_string()),
        split: SplitOrientation::Vertical,
    };
    assert_eq!(
        spaced.control_request().unwrap(),
        "spawn cwd=\"C:\\Program Files\\Git\"\n"
    );

    let split = WindowRequest {
        intent: LaunchIntent::SplitPane,
        dir: Some(r"C:\Windows".to_string()),
        split: SplitOrientation::Horizontal,
    };
    assert_eq!(
        split.control_request().unwrap(),
        "spawn split=h cwd=C:\\Windows\n"
    );
}

#[test]
fn an_unframeable_directory_is_refused_not_encoded() {
    for hostile in ["/tmp/a\nspawn", "/tmp/a\rb", "/tmp/\"quoted\""] {
        let req = WindowRequest {
            intent: LaunchIntent::NewTab,
            dir: Some(hostile.to_string()),
            split: SplitOrientation::Vertical,
        };
        assert!(
            req.control_request().is_err(),
            "{hostile:?} must be refused"
        );
    }
}

#[test]
fn window_args_are_the_window_librarys_own_flag() {
    let none = WindowRequest {
        intent: LaunchIntent::NewWindow,
        dir: None,
        split: SplitOrientation::Vertical,
    };
    assert!(none.window_args().is_empty());
    let some = WindowRequest {
        intent: LaunchIntent::NewWindow,
        dir: Some("/tmp/x".to_string()),
        split: SplitOrientation::Vertical,
    };
    assert_eq!(some.window_args(), args(&["-d", "/tmp/x"]));
}

#[test]
fn only_a_bare_window_launch_is_policy_eligible() {
    let plain = LaunchEnv::default();
    assert!(plain_launch_is_policy_eligible(&[], plain));
    assert!(plain_launch_is_policy_eligible(&args(&["--window"]), plain));
    assert!(plain_launch_is_policy_eligible(
        &args(&["--window", "-d", "/tmp"]),
        plain
    ));
    assert!(plain_launch_is_policy_eligible(
        &args(&["--working-directory", "/tmp"]),
        plain
    ));
    for hazard in [
        &["-e", "vim"][..],
        &["--command", "vim"][..],
        &["--"][..],
        &["--headless"][..],
        &["--window", "-d", "/tmp", "--headless"][..],
        &["--diagnose"][..],
        &["--containment", "user"][..],
        &["-d"][..],
    ] {
        assert!(
            !plain_launch_is_policy_eligible(&args(hazard), plain),
            "{hazard:?} must never be forwarded"
        );
    }
    let updated = LaunchEnv { updated_from: true };
    assert!(!plain_launch_is_policy_eligible(&[], updated));
    assert!(!plain_launch_is_policy_eligible(
        &args(&["--window"]),
        updated
    ));
}

#[test]
fn the_plain_gate_only_knows_the_directory_flags_the_window_parses() {
    let plain = LaunchEnv::default();
    for window_spelling in [&["-d", "/tmp"][..], &["--working-directory", "/tmp"][..]] {
        assert!(
            plain_launch_is_policy_eligible(&args(window_spelling), plain),
            "{window_spelling:?}"
        );
        assert_eq!(
            plain_launch_dir_operand(&args(window_spelling)).as_deref(),
            Some("/tmp"),
            "{window_spelling:?}"
        );
    }
    for verb_only_spelling in [
        &["-d=/tmp"][..],
        &["--directory", "/tmp"][..],
        &["--directory=/tmp"][..],
        &["--working-directory=/tmp"][..],
        &["--startingDirectory", "/tmp"][..],
        &["--startingDirectory=/tmp"][..],
    ] {
        assert!(
            !plain_launch_is_policy_eligible(&args(verb_only_spelling), plain),
            "{verb_only_spelling:?} is not a spelling the window understands, so a plain \
             launch carrying it must take the SAME route under both policies"
        );
        assert!(
            parse_window_request("new-tab", &args(verb_only_spelling), echo_dir).is_ok(),
            "{verb_only_spelling:?} must stay legal on the explicit verbs"
        );
    }
    let repeated = args(&["-d", "/first", "--working-directory", "/second"]);
    assert!(plain_launch_is_policy_eligible(&repeated, plain));
    assert_eq!(
        plain_launch_dir_operand(&repeated).as_deref(),
        Some("/second")
    );
    assert_eq!(
        parse_window_request("new-tab", &repeated, echo_dir)
            .expect("valid")
            .dir
            .as_deref(),
        Some("/second")
    );
    assert_eq!(
        plain_launch_dir_operand(&args(&["-d", "/first", "-d"])),
        None
    );
    assert_eq!(plain_launch_dir_operand(&args(&["--window"])), None);
}

#[test]
fn push_utf16_unit_pairs_surrogates_and_replaces_unpaired_ones() {
    let cases: &[(&[u16], &str)] = &[
        (&[b'a' as u16, 0x00E9, 0x65E5], "a\u{00e9}\u{65e5}"),
        (&[0xD83D, 0xDE00], "\u{1F600}"),
        (&[0xD83D, b'x' as u16], "\u{FFFD}x"),
        (&[0xD83D, 0xD83D, 0xDE00], "\u{FFFD}\u{1F600}"),
        (&[0xDE00, b'y' as u16], "\u{FFFD}y"),
        (&[0xDE00], "\u{FFFD}"),
    ];
    for &(units, want) in cases {
        let mut pending = None;
        let mut out = Vec::new();
        for &u in units {
            push_utf16_unit(u, &mut pending, &mut out);
        }
        if pending.take().is_some() {
            out.extend_from_slice(
                char::REPLACEMENT_CHARACTER
                    .encode_utf8(&mut [0; 4])
                    .as_bytes(),
            );
        }
        assert_eq!(
            std::str::from_utf8(&out).expect("utf-8"),
            want,
            "{units:x?}"
        );
    }
    let mut pending = None;
    let mut out = Vec::new();
    push_utf16_unit(0xD83D, &mut pending, &mut out);
    assert_eq!(pending, Some(0xD83D));
    assert!(out.is_empty());
    push_utf16_unit(0xDE00, &mut pending, &mut out);
    assert_eq!(pending, None);
    assert_eq!(std::str::from_utf8(&out).expect("utf-8"), "\u{1F600}");
}
