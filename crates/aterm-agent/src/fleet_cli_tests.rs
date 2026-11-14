// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

/// `-h` and `--help` are help; no mode and `--json` are the view; the lab words are the
/// lab verbs; the membership words are membership; each removed mode is refused; any other
/// word is unknown.
#[test]
fn every_mode_word_routes_to_its_one_answer() {
    for help in [Some("--help"), Some("-h")] {
        assert_eq!(route(help), Route::Help, "{help:?}");
    }
    for view in [None, Some("--json")] {
        assert_eq!(route(view), Route::View, "{view:?}");
    }
    for word in ["remove", "admit", "invite", "requests", "ignore"] {
        assert_eq!(route(Some(word)), Route::Lab, "{word}");
    }
    for word in [
        "init", "join", "peers", "allow", "access", "publish", "sync", "census", "name", "leave",
    ] {
        assert_eq!(route(Some(word)), Route::Membership, "{word}");
    }
    assert_eq!(route(Some("activity")), Route::Activity);
    for mode in [
        "events",
        "exec",
        "status",
        "inspect",
        "manage",
        "unmanage",
        "next",
        "extend",
        "ack",
        "reconcile",
        "clear-fault",
        "propose",
    ] {
        assert!(matches!(route(Some(mode)), Route::Gone(_)), "{mode}");
    }
    for word in ["bogus", "", "INIT", "--init", "undo", "list"] {
        assert_eq!(route(Some(word)), Route::Unknown, "{word:?}");
    }
}

/// Help exits 0; a removed mode or an unknown word exits 2, and so do a view given words
/// and a lab verb given the wrong ones. A membership mode changes the machine and the view
/// reads this Mac's, so only their routes are tested, never a run.
#[test]
fn help_succeeds_and_a_refused_word_is_a_usage_error() {
    let exit = |args: &[&str]| main_entry(args.iter().map(std::ffi::OsString::from).collect());
    #[cfg(unix)]
    for args in [
        &["--json", "extra"][..],
        &["remove"],
        &["admit"],
        &["invite", "x"],
        &["activity", "--since"],
        &["activity", "--since", "soon"],
        &["activity", "--since", "5s"],
        &["activity", "--json", "--json"],
        &["activity", "extra"],
    ] {
        assert_eq!(exit(args), ExitCode::from(2), "{args:?}");
    }
    for args in [&["--help"][..], &["-h"]] {
        assert_eq!(exit(args), ExitCode::SUCCESS, "{args:?}");
    }
    for word in ["events", "exec", "status", "propose", "bogus"] {
        assert_eq!(exit(&[word]), ExitCode::from(2), "{word}");
    }
}
