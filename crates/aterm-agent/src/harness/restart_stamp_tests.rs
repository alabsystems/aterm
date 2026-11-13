// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

fn scratch() -> aterm_tempfile::TempDir {
    aterm_tempfile::TempDir::new().expect("scratch")
}

fn stamp(pid: u32, start: &str, cause: Cause, at: u64) -> Stamp {
    Stamp {
        pid,
        start: start.to_string(),
        cause,
        at,
        wanted: None,
        input: None,
    }
}

#[test]
fn a_restart_input_receipt_roundtrips_and_malformed_evidence_grants_nothing() {
    let mut accepted = stamp(4242, "Wed Sep 23 20:13:55 2026", Cause::Asked, 100);
    accepted.input = Some(Input {
        host: "aterm-harness@42".to_string(),
        seq: 7,
    });
    assert_eq!(
        Stamp::from_json(&accepted.to_json()),
        Some(accepted.clone())
    );
    let mut value: aterm_json::Value = aterm_json::from_str(&accepted.to_json()).expect("stamp");
    for malformed in [
        "null",
        "{}",
        "{\"host\":\"\",\"seq\":7}",
        "{\"host\":\" aterm-harness@42\",\"seq\":7}",
        "{\"host\":\"aterm-harness@42\",\"seq\":\"7\"}",
        "{\"host\":\"aterm-harness@42\",\"seq\":-1}",
    ] {
        value.as_object_mut().expect("stamp").insert(
            "input".into(),
            aterm_json::from_str(malformed).expect("malformed receipt value"),
        );
        let read = Stamp::from_json(&aterm_json::to_string(&value).expect("stamp"))
            .expect("the restart itself remains recorded");
        assert_eq!(read.input, None, "{malformed}");
        assert!(read.names(accepted.pid, &accepted.start));
    }
    value.as_object_mut().expect("stamp").remove("input");
    assert_eq!(
        Stamp::from_json(&aterm_json::to_string(&value).expect("stamp"))
            .expect("stamp with no input receipt")
            .input,
        None
    );
}

/// NEGATIVE CONTROL: a symlinked `state` or `<state>/upgrade` directory is refused on read, write, and take.
#[test]
fn a_stamp_is_one_file_per_tab_written_whole() {
    let tmp = scratch();
    let state = tmp.path();
    let first = stamp(4242, "Wed Sep 23 20:13:55 2026", Cause::Stall, 100);
    write(state, "s-1", &first).expect("written");
    assert_eq!(read(state, "s-1"), Some(first.clone()));
    assert!(!first.to_json().contains("phase"), "{}", first.to_json());
    let second = stamp(4243, "Wed Sep 23 20:14:00 2026", Cause::Asked, 101);
    write(state, "s-1", &second).expect("replaced");
    assert_eq!(read(state, "s-1"), Some(second));
    let names: Vec<String> = std::fs::read_dir(state.join("upgrade"))
        .expect("dir")
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(names, ["asked-s-1.json"], "no temp file left");
    assert_eq!(read(state, "s-2"), None);
    for bad in ["", "../s-1", "s 1", "s/1"] {
        assert_eq!(path(state, bad), None, "{bad:?}");
        assert!(write(state, bad, &first).is_err(), "{bad:?}");
    }
    #[cfg(unix)]
    {
        let outside = scratch();
        std::fs::create_dir_all(outside.path().join("upgrade")).expect("outside upgrade");
        std::fs::write(
            outside.path().join("upgrade/asked-s-1.json"),
            first.to_json(),
        )
        .expect("plant outside stamp");
        let linked_state = tmp.path().join("linked-state");
        std::os::unix::fs::symlink(outside.path(), &linked_state).expect("symlink state");
        assert_eq!(read(&linked_state, "s-1"), None, "symlinked state refused");
        assert_eq!(
            take_if(&linked_state, "s-1", |_| true),
            None,
            "symlinked state not unlinked"
        );
        assert!(
            write(&linked_state, "s-3", &first).is_err(),
            "symlinked state refused on write"
        );
        assert!(
            !outside.path().join("upgrade/asked-s-3.json").exists(),
            "nothing written into outside target"
        );

        let real_state = tmp.path().join("real-state");
        std::fs::create_dir_all(&real_state).expect("real state");
        std::os::unix::fs::symlink(outside.path().join("upgrade"), real_state.join("upgrade"))
            .expect("symlink upgrade");
        assert_eq!(read(&real_state, "s-1"), None, "symlinked upgrade refused");
        assert_eq!(
            take_if(&real_state, "s-1", |_| true),
            None,
            "symlinked upgrade not unlinked"
        );
        assert!(
            write(&real_state, "s-3", &first).is_err(),
            "symlinked upgrade refused on write"
        );
        assert!(
            outside.path().join("upgrade/asked-s-1.json").exists(),
            "outside stamp untouched"
        );
    }
    assert_eq!(Stamp::from_json("{\"pid\":1}"), None, "a torn stamp");
    assert_eq!(Cause::parse("stall"), Some(Cause::Stall));
    assert_eq!(Cause::parse("asked"), Some(Cause::Asked));
    assert_eq!(Cause::parse("exit"), None);
}

#[test]
fn taking_a_stamp_takes_only_the_one_meant() {
    let tmp = scratch();
    let state = tmp.path();
    let s = stamp(4242, "Wed Sep 23 20:13:55 2026", Cause::Asked, 100);
    write(state, "s-1", &s).expect("written");
    assert_eq!(take_if(state, "s-1", |_| false), None);
    assert_eq!(read(state, "s-1"), Some(s.clone()), "put back");
    let newer = stamp(4243, "Wed Sep 23 20:14:00 2026", Cause::Asked, 101);
    assert_eq!(
        take_if(state, "s-1", |_| {
            write(state, "s-1", &newer).expect("a newer act");
            false
        }),
        None
    );
    assert_eq!(
        read(state, "s-1"),
        Some(newer.clone()),
        "the newer one stands"
    );
    assert_eq!(
        take_if(state, "s-1", |_| {
            assert_eq!(read(state, "s-1"), Some(newer.clone()), "in place");
            false
        }),
        None
    );
    let newest = stamp(4244, "Wed Sep 23 20:14:05 2026", Cause::Stall, 102);
    assert_eq!(
        take_if(state, "s-1", |_| {
            write(state, "s-1", &newest).expect("a newer act still");
            true
        }),
        None
    );
    assert_eq!(read(state, "s-1"), Some(newest.clone()), "put back");
    write(state, "s-1", &newer).expect("the act's again");
    assert_eq!(take_if(state, "s-1", |t| t.pid == 4243), Some(newer));
    assert_eq!(read(state, "s-1"), None, "taken");
    assert_eq!(take_if(state, "s-1", |_| true), None, "nothing to take");
    assert_eq!(
        std::fs::read_dir(state.join("upgrade"))
            .expect("dir")
            .count(),
        0,
        "nothing left aside"
    );
}

#[test]
fn the_lookup_names_its_exit_and_sweeps_what_is_over() {
    let tmp = scratch();
    let state = tmp.path();
    let me = std::process::id();
    let start = kernel_start(me).expect("this process's start");
    let now = aterm_time::unix_secs().expect("clock");
    write(
        state,
        "s-1",
        &stamp(4242, "Wed Sep 23 20:13:55 2026", Cause::Stall, now),
    )
    .expect("written");
    assert_eq!(
        lookup(state, "s-1", 4242, "Wed Sep 23 20:13:55 2026", now),
        Some(Cause::Stall)
    );
    assert!(
        read(state, "s-1").is_some(),
        "kept for the relaunch to take"
    );
    write(state, "s-1", &stamp(me, &start, Cause::Asked, now)).expect("written");
    assert_eq!(
        lookup(state, "s-1", 4242, "Wed Sep 23 20:13:55 2026", now),
        None
    );
    assert!(
        read(state, "s-1").is_some(),
        "the act on a live process stands"
    );
    assert!(alive_as(me, &start));
    write(
        state,
        "s-1",
        &stamp(me, "Thu Jan 1 00:00:00 1970", Cause::Asked, now),
    )
    .expect("written");
    assert_eq!(
        lookup(state, "s-1", 4242, "Wed Sep 23 20:13:55 2026", now),
        None
    );
    assert_eq!(read(state, "s-1"), None, "a gone process's stamp is swept");
    let old = now - super::super::relaunch::STALE_S - 1;
    write(
        state,
        "s-1",
        &stamp(4242, "Wed Sep 23 20:13:55 2026", Cause::Asked, old),
    )
    .expect("written");
    assert_eq!(
        lookup(state, "s-1", 4242, "Wed Sep 23 20:13:55 2026", now),
        None
    );
    assert_eq!(read(state, "s-1"), None, "a stale stamp is swept");
    assert_eq!(lookup(state, "s-9", 4242, "x", now), None);
}

#[test]
fn the_stamp_standing_for_a_process_is_the_one_its_exit_is_read_by() {
    let tmp = scratch();
    let state = tmp.path();
    let now = 10_000;
    let s = stamp(4242, "Wed Sep 23 20:13:55 2026", Cause::Stall, now - 3);
    assert_eq!(
        standing(state, "s-1", 4242, &s.start, now),
        None,
        "none yet"
    );
    write(state, "s-1", &s).expect("written");
    assert_eq!(standing(state, "s-1", 4242, &s.start, now), Some(s.clone()));
    assert_eq!(
        standing(state, "s-1", 4243, &s.start, now),
        None,
        "another pid"
    );
    assert_eq!(
        standing(state, "s-1", 4242, "Wed Sep 23 20:14:00 2026", now),
        None,
        "another process by that pid"
    );
    assert_eq!(
        standing(state, "s-2", 4242, &s.start, now),
        None,
        "another tab"
    );
    let later = now - 3 + super::super::relaunch::STALE_S + 1;
    assert!(s.stale(later) && !s.stale(later - 1));
    assert_eq!(standing(state, "s-1", 4242, &s.start, later), None, "stale");
    assert_eq!(read(state, "s-1"), Some(s), "asking removes nothing");
}

/// The act reads a start in the snapshot's own spelling, by syscall, never a `ps`: on Unix equal
/// to what `ps -o lstart=` in UTC and the C locale says of the same process, on Windows the
/// creation `FILETIME` Claude Code writes as its `procStart`. A stamp stands a second short of
/// going stale.
#[test]
fn the_act_reads_a_start_in_the_snapshots_own_spelling() {
    let me = std::process::id();
    let start = kernel_start(me).expect("this process's start");
    assert_eq!(
        Some(start),
        super::super::upgrade_drive::kernel_start(me),
        "the stamp's spelling is the snapshot's"
    );
    let s = stamp(me, "x", Cause::Asked, 1_000);
    let stale_at = 1_000 + super::super::relaunch::STALE_S;
    assert_eq!(s.stands_for(stale_at - 7).as_secs(), 7);
    assert_eq!(s.stands_for(stale_at).as_secs(), 0);
    assert!(!s.stale(stale_at) && s.stale(stale_at + 1));
}

#[test]
fn the_prune_removes_only_a_stale_stamp() {
    let tmp = scratch();
    let state = tmp.path();
    let now = 10_000;
    let stale_at = super::super::relaunch::STALE_S + 1;
    let old = stamp(
        4242,
        "Thu Jan 1 00:00:00 1970",
        Cause::Asked,
        now - stale_at,
    );
    let standing = stamp(4243, "Thu Jan 1 00:00:00 1970", Cause::Stall, now - 1);
    write(state, "s-old", &old).expect("written");
    write(state, "s-new", &standing).expect("written");
    assert!(prune(state, "asked-s-old.json", now));
    assert!(prune(state, "asked-s-new.json", now));
    assert_eq!(read(state, "s-old"), None, "stale: removed");
    assert_eq!(read(state, "s-new"), Some(standing), "standing: kept");
    for other in [
        "0badf00d.json",
        "codex-s-1.json",
        "asked-s-new.json.1.2.3.tmp",
        "asked-.json",
        "asked-s-new.json.1.2.3.json",
    ] {
        assert!(!prune(state, other, now), "{other}");
    }
    assert_eq!(
        Stamp::from_json(r#"{"pid":0,"start":"x","cause":"asked","at":1}"#),
        None
    );
    assert_eq!(
        Stamp::from_json(r#"{"pid":1,"start":"","cause":"asked","at":1}"#),
        None
    );
}

#[test]
fn the_relaunch_that_files_an_asked_exit_takes_its_stamp() {
    use super::super::relaunch::{
        CAUSE_ASKED, ExitCause, Snapshot, consume_stamp, ends_its_agent, relaunch_cause,
    };
    let tmp = scratch();
    let opts = super::super::upgrade_drive::Opts {
        home: tmp.path().join("home"),
        state: tmp.path().join("state"),
        account_roster: None,
        sock: None,
        token: None,
        only_sid: Some("s-1".to_string()),
        dry_run: false,
        human_grace_s: 0,
        hand_back: true,
        background: false,
        aterm_state: None,
        ask: false,
        usage_tap: None,
        instances: crate::harness::upgrade_drive::Instances::Listed(Vec::new()),
        agents_off: Vec::new(),
        land: false,
        tell: crate::harness::upgrade_drive::Tell::Off,
        press: None,
        timings: crate::harness::Timings::FAST,
    };
    let snap = Snapshot {
        lifetime: None,
        launch_env: None,
        auth_observed: true,
        route: crate::harness::accounts::AuthRoute::Outside,
        profile: None,
        tab: "s-1".to_string(),
        pid: 4242,
        start: "Wed Sep 23 20:13:55 2026".to_string(),
        shell: 4241,
        program: "/opt/claude".into(),
        argv: vec!["/opt/claude".to_string()],
        session: None,
        cwd: "/".to_string(),
        version: None,
        run: crate::harness::relaunch::AgentRun::Claude,
        dialect: None,
        login: None,
        home: None,
        claude_dir: None,
    };
    let ours = stamp(4242, "Wed Sep 23 20:13:55 2026", Cause::Asked, 1);
    write(&opts.state, "s-1", &ours).expect("written");
    consume_stamp(&opts, &snap, ExitCause::Exit);
    assert_eq!(
        read(&opts.state, "s-1"),
        Some(ours.clone()),
        "no act's exit"
    );
    consume_stamp(
        &super::super::upgrade_drive::Opts {
            dry_run: true,
            ..opts.clone()
        },
        &snap,
        ExitCause::Asked,
    );
    assert_eq!(read(&opts.state, "s-1"), Some(ours.clone()), "a dry run");
    let other = Snapshot {
        pid: 4243,
        ..snap.clone()
    };
    consume_stamp(&opts, &other, ExitCause::Asked);
    assert_eq!(read(&opts.state, "s-1"), Some(ours), "another process");
    consume_stamp(&opts, &snap, ExitCause::Asked);
    assert_eq!(read(&opts.state, "s-1"), None, "taken as filed");
    write(
        &opts.state,
        "s-1",
        &stamp(4242, &snap.start, Cause::Stall, 1),
    )
    .expect("written");
    consume_stamp(&opts, &snap, ExitCause::Stall);
    assert_eq!(read(&opts.state, "s-1"), None, "the stall's too");
    assert!(relaunch_cause(CAUSE_ASKED));
    assert!(
        !ends_its_agent(CAUSE_ASKED),
        "its exit is dated as any relaunch's"
    );
}

/// A stamp names the account its relaunch should start on only when its act asked for one, and a
/// stamp written before that field existed reads as a plain restart.
#[test]
fn a_stamp_carries_the_account_its_act_asked_for() {
    let plain = stamp(4242, "Wed Sep 23 20:13:55 2026", Cause::Asked, 100);
    assert!(!plain.to_json().contains("wanted"), "{}", plain.to_json());
    assert_eq!(Stamp::from_json(&plain.to_json()), Some(plain.clone()));
    assert_eq!(
        Stamp::from_json(
            r#"{"pid":4242,"start":"Wed Sep 23 20:13:55 2026","cause":"asked","at":100}"#
        ),
        Some(plain.clone()),
        "the shape an earlier act wrote"
    );
    for wanted in [Wanted::Row("work-2".to_string()), Wanted::Pool] {
        let asked = Stamp {
            wanted: Some(wanted.clone()),
            ..plain.clone()
        };
        let read = Stamp::from_json(&asked.to_json()).expect("a stamp");
        assert_eq!(read.wanted, Some(wanted), "{}", asked.to_json());
        assert_eq!(read, asked);
        assert_ne!(read, plain, "another act's stamp");
    }
    // A row labelled `auto` and the pool's choice are two asks.
    let auto_row = Stamp {
        wanted: Some(Wanted::Row("auto".to_string())),
        ..plain.clone()
    };
    assert_eq!(
        Stamp::from_json(&auto_row.to_json()).and_then(|s| s.wanted),
        Some(Wanted::Row("auto".to_string()))
    );
}

/// NEGATIVE CONTROL: with the whole stamp refused for an unreadable destination, the lookup names
/// no cause and the act's own signal exit is read as nobody's, which is never relaunched.
#[test]
fn a_destination_a_stamp_cannot_name_is_dropped_and_its_restart_stands() {
    let tmp = scratch();
    let state = tmp.path();
    let start = "Wed Sep 23 20:13:55 2026";
    let now = 10_000;
    for bad in [
        r#"{"row":"Work"}"#,
        r#"{"row":"help"}"#,
        r#"{"row":"../x"}"#,
        r#"{"row":""}"#,
        r#"{"row":7}"#,
        r#"{"pool":false}"#,
        r#"{"row":"b","pool":true}"#,
        r#"{}"#,
        r#""pool""#,
        "null",
        "7",
    ] {
        let text = format!(
            r#"{{"pid":4242,"start":"{start}","cause":"stall","at":{now},"wanted":{bad}}}"#
        );
        let read = Stamp::from_json(&text).unwrap_or_else(|| panic!("still a stamp: {bad}"));
        assert_eq!(read.wanted, None, "{bad}");
        assert_eq!(read.cause, Cause::Stall, "{bad}");
        std::fs::create_dir_all(state.join("upgrade")).expect("dir");
        std::fs::write(path(state, "s-1").expect("path"), &text).expect("planted");
        assert_eq!(
            lookup(state, "s-1", 4242, start, now),
            Some(Cause::Stall),
            "its exit is still the act's: {bad}"
        );
        assert_eq!(wanted_for(state, "s-1", 4242, start, now), None, "{bad}");
    }
}

/// One stamp carries both an act's input receipt and the account it named, and an unreadable one
/// of them drops alone: the other field and the restart itself still stand.
#[test]
fn a_receipt_and_a_destination_share_a_stamp_and_each_drops_alone() {
    let both = Stamp {
        wanted: Some(Wanted::Row("work-2".to_string())),
        input: Some(Input {
            host: "aterm-harness@42".to_string(),
            seq: 7,
        }),
        ..stamp(4242, "Wed Sep 23 20:13:55 2026", Cause::Asked, 100)
    };
    assert_eq!(Stamp::from_json(&both.to_json()), Some(both.clone()));
    let mut value: aterm_json::Value = aterm_json::from_str(&both.to_json()).expect("stamp");
    let o = value.as_object_mut().expect("stamp");
    o.insert("wanted".into(), aterm_json::Value::from("Work"));
    let read = Stamp::from_json(&aterm_json::to_string(&value).expect("stamp"))
        .expect("a torn destination leaves the stamp");
    assert_eq!(read.wanted, None);
    assert_eq!(
        read.input, both.input,
        "the receipt survives a torn destination"
    );
    let o = value.as_object_mut().expect("stamp");
    o.insert(
        "wanted".into(),
        aterm_json::from_str(r#"{"pool":true}"#).expect("pool"),
    );
    o.insert(
        "input".into(),
        aterm_json::from_str(r#"{"host":""}"#).expect("torn"),
    );
    let read = Stamp::from_json(&aterm_json::to_string(&value).expect("stamp"))
        .expect("a torn receipt leaves the stamp");
    assert_eq!(read.input, None);
    assert_eq!(
        read.wanted,
        Some(Wanted::Pool),
        "the destination survives a torn receipt"
    );
    assert!(read.names(both.pid, &both.start));
}

#[test]
fn the_account_asked_for_is_read_only_for_the_process_its_stamp_names() {
    let tmp = scratch();
    let state = tmp.path();
    let now = 10_000;
    let start = "Wed Sep 23 20:13:55 2026";
    let row = Wanted::Row("b".to_string());
    assert_eq!(wanted_for(state, "s-1", 4242, start, now), None, "none yet");
    let asked = Stamp {
        wanted: Some(row.clone()),
        ..stamp(4242, start, Cause::Asked, now - 3)
    };
    write(state, "s-1", &asked).expect("written");
    assert_eq!(wanted_for(state, "s-1", 4242, start, now), Some(row));
    assert_eq!(wanted_for(state, "s-1", 4243, start, now), None, "a pid");
    assert_eq!(
        wanted_for(state, "s-1", 4242, "Wed Sep 23 20:14:00 2026", now),
        None,
        "another process by that pid"
    );
    assert_eq!(wanted_for(state, "s-2", 4242, start, now), None, "a tab");
    let later = now - 3 + super::super::relaunch::STALE_S + 1;
    assert_eq!(wanted_for(state, "s-1", 4242, start, later), None, "stale");
    assert_eq!(
        read(state, "s-1"),
        Some(asked.clone()),
        "asking removes nothing"
    );
    // The relaunch takes the stamp it judged, destination and all, and not a plain one beside it.
    let plain = stamp(4242, start, Cause::Asked, now - 3);
    assert_eq!(take_if(state, "s-1", |s| *s == plain), None);
    assert_eq!(take_if(state, "s-1", |s| *s == asked), Some(asked));
    assert_eq!(read(state, "s-1"), None, "taken");
    write(state, "s-1", &plain).expect("a plain restart");
    assert_eq!(wanted_for(state, "s-1", 4242, start, now), None, "plain");
}
