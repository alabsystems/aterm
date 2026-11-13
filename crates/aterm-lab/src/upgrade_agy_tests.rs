// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn a_reply_is_whole_once_agy_wrote_it_done_to_its_last_tick() {
    let row = |kind: &str, status: &str, content: &str| {
        format!(
            r#"{{"step_index":9,"source":"MODEL","type":"{kind}","status":"{status}","created_at":"2026-10-29T07:02:23Z","content":"{content}"}}"#
        )
    };
    let ticks = |to: i64| {
        (16..=to)
            .map(|i| format!("tick {i}"))
            .collect::<Vec<_>>()
            .join(" ")
    };
    let last = crate::stub::SLOW_SECONDS;
    assert!(agy_reply_whole(&row(
        "PLANNER_RESPONSE",
        "DONE",
        &ticks(last)
    )));
    assert!(
        !agy_reply_whole(&row("PLANNER_RESPONSE", "DONE", &ticks(last - 1))),
        "a reply cut before its last tick"
    );
    assert!(
        !agy_reply_whole(&row("PLANNER_RESPONSE", "RUNNING", &ticks(last))),
        "a reply agy had not finished"
    );
    assert!(
        !agy_reply_whole(&row("USER_INPUT", "DONE", &ticks(last))),
        "another row's words"
    );
}

#[test]
fn a_plain_versions_build_id_is_atpkgs() {
    assert_eq!(plain_build_id("1.4.0"), Some(1_000_001_000_004_000_000));
    assert_eq!(plain_build_id("1.4.1"), Some(1_000_001_000_004_000_001));
    for bad in ["1.4.0-R1", "1.04.0", "1.4.1000000", "1.4.0.1", "x"] {
        assert_eq!(plain_build_id(bad), None, "{bad}");
    }
    assert_eq!(bumped("1.4.0", 0).as_deref(), Some("1.4.0"));
    assert_eq!(bumped("1.4.0", 6).as_deref(), Some("1.4.6"));
    assert_eq!(bumped("1.4.10-R6427.1", 1), None);
}

/// A LANDED BUILD HOLDS THE SCRATCH UNTIL IT SHOWED IT RAN: each landing
/// holds it before its clone is laid, and a build shown to run releases
/// its own hold only. NEGATIVE CONTROL: the other build's hold stays.
#[test]
fn a_landed_build_holds_the_scratch_until_it_showed_it_ran() {
    let root = aterm_tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(root.path()).unwrap();
    let source = root.join("agy");
    std::fs::write(&source, "a build").unwrap();
    let store = Store {
        prefix: root.join("pkg"),
        source,
        record: String::new(),
        run: root.join("run"),
    };
    let ids = ["1.4.0", "1.4.1"].map(|v| plain_build_id(v).unwrap());
    let held = |id: u64| {
        root.join("run")
            .join(crate::lab::HELD)
            .join(format!("agy-{id}"))
    };
    for v in ["1.4.0", "1.4.1"] {
        assert!(store.land(v).unwrap().is_file(), "{v}");
    }
    assert!(held(ids[0]).is_dir() && held(ids[1]).is_dir());
    store.ran(ids[0]);
    assert!(!held(ids[0]).exists(), "shown to run");
    assert!(held(ids[1]).is_dir(), "the other build still holds it");
}
