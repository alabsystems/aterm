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
    }
}

/// A stamp is one file per tab, written whole: it reads back as written, a second act's replaces
/// the first, and nothing else is left beside it. Its text carries no `phase`; a tab key that is
/// not a plain word names no file.
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
    assert_eq!(Stamp::from_json("{\"pid\":1}"), None, "a torn stamp");
    assert_eq!(Cause::parse("stall"), Some(Cause::Stall));
    assert_eq!(Cause::parse("asked"), Some(Cause::Asked));
    assert_eq!(Cause::parse("exit"), None);
}

/// Taking a stamp takes only the one meant: one `ours` accepts is removed and returned; one it
/// refuses is never moved; and a newer stamp landing between the judgement and the move is put
/// back, never taken.
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
    // Judged in place: the act's watch finds the stamp while a take judges it.
    assert_eq!(
        take_if(state, "s-1", |_| {
            assert_eq!(read(state, "s-1"), Some(newer.clone()), "in place");
            false
        }),
        None
    );
    // A newer stamp landing between the judgement and the move is put back,
    // never taken for the one judged.
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

/// The reader's lookup: a stamp naming the process whose exit is read gives its cause and stands
/// until the relaunch takes it; one naming another process that still runs is left alone; one
/// naming a gone process, or older than `STALE_S`, is removed as it is found. The live process is
/// this test's own.
#[test]
fn the_lookup_names_its_exit_and_sweeps_what_is_over() {
    let tmp = scratch();
    let state = tmp.path();
    let me = std::process::id();
    let start = kernel_start(me).expect("this process's start");
    let now = aterm_time::unix_secs().expect("clock");
    // The exit read is the stamped process's: its cause, kept.
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
    // Another process's exit, the stamped one still running: left alone.
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
    // The stamped process is gone (a start that is not this pid's): swept.
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
    // Older than STALE_S: swept, even naming the exit read.
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
    // No stamp: nothing.
    assert_eq!(lookup(state, "s-9", 4242, "x", now), None);
}

/// The stamp standing for a process is the one its exit would be read by: the tab's stamp naming
/// that very process (pid and kernel start), no older than `STALE_S`. Asking writes or removes
/// nothing.
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

/// The act reads a start in the snapshot's own spelling: on macOS by syscall, never a `ps`, and
/// equal to what `ps -o lstart=` in UTC and the C locale says of the same process. A stamp stands a
/// second short of going stale.
#[test]
fn the_act_reads_a_start_in_the_snapshots_own_spelling() {
    let me = std::process::id();
    let start = kernel_start(me).expect("this process's start");
    assert_eq!(
        Some(start),
        super::super::upgrade_drive::kernel_start(me),
        "the syscall's spelling is ps's"
    );
    let s = stamp(me, "x", Cause::Asked, 1_000);
    let stale_at = 1_000 + super::super::relaunch::STALE_S;
    assert_eq!(s.stands_for(stale_at - 7).as_secs(), 7);
    assert_eq!(s.stands_for(stale_at).as_secs(), 0);
    assert!(!s.stale(stale_at) && s.stale(stale_at + 1));
}

/// The prune removes only a stale stamp: one past `STALE_S` goes whatever its process; one still
/// standing stays even naming a gone process (its exit may not have been read yet); a file that is
/// no stamp's is not the prune's.
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
    ] {
        assert!(!prune(state, other, now), "{other}");
    }
}

/// The relaunch that files an asked exit consumes its stamp, and the cause `asked` is a relaunch's,
/// not a restart that ended its agent. Negative controls: an exit no act made (`ExitCause::Exit`),
/// a stamp naming another process, and a dry run take nothing.
#[test]
fn the_relaunch_that_files_an_asked_exit_takes_its_stamp_and_says_why() {
    use super::super::relaunch::{
        CAUSE_ASKED, ExitCause, Snapshot, consume_stamp, ends_its_agent, relaunch_cause,
        resumed_prompt,
    };
    let tmp = scratch();
    let opts = super::super::upgrade_drive::Opts {
        home: tmp.path().join("home"),
        state: tmp.path().join("state"),
        sock: None,
        only_sid: Some("s-1".to_string()),
        dry_run: false,
        human_grace_s: 0,
        hand_back: true,
        background: false,
        aterm_state: None,
        ask: false,
        several_logins: false,
        usage_tap: None,
        instances: crate::harness::upgrade_drive::Instances::Listed(Vec::new()),
        agents_off: Vec::new(),
    };
    let snap = Snapshot {
        stored_login: false,
        tab: "s-1".to_string(),
        pid: 4242,
        start: "Wed Sep 23 20:13:55 2026".to_string(),
        shell: 4241,
        program: "/opt/claude".into(),
        argv: vec!["/opt/claude".to_string()],
        session: None,
        cwd: "/".to_string(),
        version: None,
        codex: None,
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
    let said = resumed_prompt("2.1.283", CAUSE_ASKED);
    assert!(
        said.contains("someone asked aterm to restart Claude Code"),
        "{said}"
    );
}
