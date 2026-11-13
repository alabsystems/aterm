// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn a_turn_is_whole_once_muse_committed_its_last_tick_and_its_run_completed() {
    let record = |run: &str, event: &str| {
        format!(
            r#"{{"schema_version":1,"record_type":"event","payload_type":"runtime.session","payload":{{"kind":"run","run_id":"{run}","event":{event}}}}}"#
        )
    };
    let committed = |run: &str, text: &str| {
        record(
            run,
            &format!(r#"{{"kind":"assistant_message_committed","text":"{text}"}}"#),
        )
    };
    let ended =
        |run: &str, how: &str| record(run, &format!(r#"{{"kind":"terminal","terminal":"{how}"}}"#));
    let ticks = |to: i64| (1..=to).map(|i| format!("tick {i} ")).collect::<String>();
    let last = crate::stub::SLOW_SECONDS;
    let whole = ticks(last);
    let log = |rows: &[String]| rows.join("\n");
    assert!(muse_reply_whole(&log(&[
        committed("r1", &whole),
        ended("r1", "completed")
    ])));
    assert!(
        !muse_reply_whole(&committed("r1", &whole)),
        "a reply committed whole whose run has not ended: the end-of-turn gate still runs"
    );
    assert!(
        !muse_reply_whole(&log(&[
            committed("r1", &ticks(last - 1)),
            ended("r1", "completed")
        ])),
        "a reply cut before its last tick"
    );
    assert!(
        !muse_reply_whole(&log(&[committed("r1", &whole), ended("r2", "completed")])),
        "another run's end"
    );
    assert!(
        !muse_reply_whole(&log(&[committed("r1", &whole), ended("r1", "cancelled")])),
        "a run that did not complete"
    );
    let approval =
        r#"{"payload":{"kind":"approval_wait_effect","run_id":"r1","record":{"kind":"terminal"}}}"#;
    assert!(
        !muse_reply_whole(&log(&[committed("r1", &whole), approval.to_string()])),
        "another record's terminal"
    );
    assert!(
        !muse_reply_whole(&log(&[
            record("r1", &format!(r#"{{"kind":"status","text":"{whole}"}}"#)),
            ended("r1", "completed")
        ])),
        "another event's words"
    );
    assert!(
        !ends_on_last_tick(&format!("tick {last}0")),
        "a longer reply's tick"
    );
}

/// One scripted run of [`held_while_with`] over eight looks: the `gone`
/// answers in order, one per ask (the confirming ask after the work is
/// seen ended takes the next), and the `running` answers likewise (`None`
/// a failed read); the clock one second an ask.
fn judged(looks: &[(bool, Option<bool>)]) -> (bool, Option<i64>) {
    let gones: std::cell::RefCell<std::collections::VecDeque<bool>> =
        std::cell::RefCell::new(looks.iter().map(|l| l.0).collect());
    let runs: std::cell::RefCell<std::collections::VecDeque<Option<bool>>> =
        std::cell::RefCell::new(looks.iter().map(|l| l.1).collect());
    let clock = std::cell::Cell::new(0);
    held_while_with(
        8,
        |_| Ok(()),
        || gones.borrow_mut().pop_front().unwrap_or(true),
        || runs.borrow_mut().pop_front().unwrap_or(Some(false)),
        || {
            clock.set(clock.get() + 1000);
            clock.get()
        },
    )
    .expect("no stop")
}

/// THE JUDGE OF A RESTART HELD WHILE WORK RAN: work seen ended while the
/// agent still stands, and still stands a moment later, is held and ended
/// (`(true, Some)`).
#[test]
fn work_that_ends_with_the_agent_is_never_read_as_ended_first() {
    let (ran, ended) = (Some(true), Some(false));
    assert_eq!(
        judged(&[(false, ran), (false, ended), (false, ran)]),
        (true, Some(1000))
    );
    assert_eq!(judged(&[(false, ran), (true, ended)]), (false, None));
    assert_eq!(
        judged(&[(false, ran), (false, ended), (true, ran)]),
        (false, None)
    );
    assert_eq!(judged(&[(false, ran), (true, ran)]), (false, None));
    assert_eq!(judged(&[(false, ran); 8]), (true, None));
}

/// A FAILED READ IS NO END: a look whose read failed sees nothing, so an
/// agent restarted on the next look was restarted while the work ran
/// (read as an end, that look would have judged it held and ended); an
/// end seen after it still counts, and reads that all fail see none.
#[test]
fn a_failed_read_is_never_read_as_the_work_ended() {
    let (ran, ended, failed) = (Some(true), Some(false), None);
    assert_eq!(
        judged(&[(false, ran), (false, failed), (false, ran), (true, ran)]),
        (false, None)
    );
    assert_eq!(
        judged(&[(false, ran), (false, failed), (false, ended), (false, ran)]),
        (true, Some(1000))
    );
    assert_eq!(judged(&[(false, failed); 8]), (true, None));
}

/// The build id atpkg lays a revision-form version under, as its store
/// holds them (the scratch-agents copy's `2001004010006427002` is
/// `1.4.10-R6427.1`).
#[test]
fn a_revision_versions_build_id_is_atpkgs() {
    assert_eq!(
        revision_build_id("1.4.10-R6427.1"),
        Some(2_001_004_010_006_427_002)
    );
    assert_eq!(
        revision_build_id("1.4.11-R6500.1"),
        Some(2_001_004_011_006_500_002)
    );
    assert_eq!(
        revision_build_id("1.4.10-R6427"),
        Some(2_001_004_010_006_427_000)
    );
    for bad in ["1.4.10", "1.04.10-R1", "1.4.1000-R1", "1.4.10-R1.999", "x"] {
        assert_eq!(revision_build_id(bad), None, "{bad}");
    }
}

/// A bumped version is the next patch, its revision kept; a record names
/// it once, its other lines kept.
#[test]
fn a_landing_names_a_newer_version_in_its_record() {
    assert_eq!(
        bumped("1.4.10-R6427.1", 1).as_deref(),
        Some("1.4.11-R6427.1")
    );
    assert_eq!(
        bumped("1.4.10-R6427.1", 3).as_deref(),
        Some("1.4.13-R6427.1")
    );
    assert_eq!(bumped("1.4.10", 1), None);
    let r = record_naming(
        "schema = 1\nprogram = \"muse\"\nversion = \"1.4.10-R6427.1\"\nvendor = \"Meta\"\n",
        "1.4.11-R6427.1",
    );
    assert_eq!(r.matches("version = ").count(), 1, "{r}");
    assert!(r.contains("version = \"1.4.11-R6427.1\"") && r.contains("vendor = \"Meta\""));
}

/// A LANDED BUILD HOLDS THE SCRATCH UNTIL IT SHOWED IT RAN: each landing
/// holds it before its clone is laid, and a build shown to run releases
/// its own hold only. NEGATIVE CONTROL: the other build's hold stays.
#[test]
fn a_landed_build_holds_the_scratch_until_it_showed_it_ran() {
    let root = aterm_tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(root.path()).unwrap();
    let source = root.join("muse");
    std::fs::write(&source, "a build").unwrap();
    let store = Store {
        prefix: root.join("pkg"),
        source,
        record: String::new(),
        run: root.join("run"),
    };
    let versions = ["1.4.10-R6427.1", "1.4.11-R6427.1"];
    let ids = versions.map(|v| revision_build_id(v).unwrap());
    let held = |id: u64| {
        root.join("run")
            .join(crate::lab::HELD)
            .join(format!("muse-{id}"))
    };
    for v in versions {
        assert_eq!(
            store.land(v).unwrap(),
            store.bin(revision_build_id(v).unwrap()),
            "{v}"
        );
    }
    assert!(held(ids[0]).is_dir() && held(ids[1]).is_dir());
    store.ran(ids[0]);
    assert!(!held(ids[0]).exists(), "shown to run");
    assert!(held(ids[1]).is_dir(), "the other build still holds it");
}
