// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The command meter's edges, each kind's row, and the `telemetry` verb's framing.

use super::*;

fn mark(id: u64, runs: bool) -> BlockMark {
    BlockMark(Some((id, runs)))
}

fn edge(cpu: u64, children: &[u32]) -> Option<Edge> {
    Some(Edge {
        cpu,
        children: Some(children.to_vec()),
    })
}

/// A block that ran before `m`'s first: its D leaves the prompt with `children`.
fn after_a_block(m: &CmdMeter, children: &[u32]) {
    m.slice_with(mark(1, false), mark(1, true), 9, |_| edge(0, children));
    m.slice_with(mark(1, true), mark(2, false), 9, |_| edge(0, children));
    let _ = m.take_cpu(1);
}

#[test]
fn a_command_gets_the_cpu_between_its_c_and_its_d() {
    let m = CmdMeter::default();
    // A prompt helper that lives on beside every command costs nothing.
    after_a_block(&m, &[50]);
    // The prompt, then C: a start reading. The command's own child (60) is already forked.
    m.slice_with(mark(4, false), mark(4, true), 9, |_| {
        edge(1_000_000, &[50, 60])
    });
    // Output with no edge: no reading.
    m.slice_with(mark(4, true), mark(4, true), 9, |_| {
        panic!("no edge, no read")
    });
    // D, then the next prompt in the same slice: the end reading, its own child reaped.
    m.slice_with(mark(4, true), mark(5, false), 9, |_| {
        edge(251_000_000, &[50])
    });
    assert_eq!(m.take_cpu(4), Some(250_000_000));
    assert_eq!(m.take_cpu(4), None, "taken once");
}

#[test]
fn a_job_reaped_beside_a_command_leaves_its_cpu_unknown_not_charged() {
    let m = CmdMeter::default();
    // `make &` left job 70 running at the prompt; the next command starts beside it.
    after_a_block(&m, &[50, 70]);
    m.slice_with(mark(4, false), mark(4, true), 9, |_| edge(1_000, &[50, 70]));
    // The shell reaped 70 while the command ran: its CPU is in the window.
    m.slice_with(mark(4, true), mark(5, false), 9, |_| {
        edge(9_000_000_000, &[50])
    });
    assert_eq!(m.take_cpu(4), None, "another job's CPU is no command's");
    // The next command starts with 70 gone: its CPU is its own again.
    m.slice_with(mark(5, false), mark(5, true), 9, |_| {
        edge(9_000_000_000, &[50])
    });
    m.slice_with(mark(5, true), mark(6, false), 9, |_| {
        edge(9_000_002_000, &[50])
    });
    assert_eq!(m.take_cpu(5), Some(2_000));
    // A job that ended at the prompt was reaped before the C reading: not in the window.
    after_a_block(&m, &[50, 80]);
    m.slice_with(mark(7, false), mark(7, true), 9, |_| edge(100, &[50]));
    m.slice_with(mark(7, true), mark(8, false), 9, |_| edge(400, &[50]));
    assert_eq!(m.take_cpu(7), Some(300));
}

#[test]
fn c_and_d_in_one_slice_leave_the_cpu_unknown_not_zero() {
    let m = CmdMeter::default();
    after_a_block(&m, &[]);
    m.slice_with(mark(4, false), mark(5, false), 9, |_| {
        panic!("no executing edge")
    });
    assert_eq!(m.take_cpu(4), None);
    // A reading the kernel refused leaves it unknown too.
    m.slice_with(mark(6, false), mark(6, true), 9, |_| None);
    m.slice_with(mark(6, true), mark(6, false), 9, |_| edge(5, &[]));
    assert_eq!(m.take_cpu(6), None);
    // So does a child list it refused, and a first block with no prompt read before it.
    let unlisted = CmdMeter::default();
    after_a_block(&unlisted, &[]);
    unlisted.slice_with(mark(7, false), mark(7, true), 9, |_| {
        Some(Edge {
            cpu: 1,
            children: None,
        })
    });
    unlisted.slice_with(mark(7, true), mark(8, false), 9, |_| edge(9, &[]));
    assert_eq!(unlisted.take_cpu(7), None);
    let first = CmdMeter::default();
    first.slice_with(mark(1, false), mark(1, true), 9, |_| edge(1, &[]));
    first.slice_with(mark(1, true), mark(2, false), 9, |_| edge(9, &[]));
    assert_eq!(first.take_cpu(1), None);
}

#[test]
fn the_meter_keeps_a_bounded_number_of_blocks() {
    let m = CmdMeter::default();
    for id in 0..(METER_KEEP as u64 * 3) {
        m.slice_with(mark(id, false), mark(id, true), 9, |_| edge(id, &[]));
    }
    let st = m.0.lock().expect("lock");
    assert_eq!(st.started.len(), METER_KEEP);
}

#[test]
fn each_block_is_reported_once_in_order() {
    let m = CmdMeter::default();
    assert!(m.report(3));
    assert!(!m.report(3));
    assert!(!m.report(2));
    assert!(m.report(4));
}

fn facts() -> Digest {
    Digest::of("test", &[b"facts"])
}

fn blk(exit: Option<Option<i32>>) -> BlockFacts<'static> {
    BlockFacts {
        line: "AWS_SECRET=xyz targo build -p aterm",
        dir: Some("/Users//me/aterm"),
        program: Some("targo"),
        started_ms: Some(1_000),
        ended_ms: Some(4_500),
        exit,
    }
}

#[test]
fn a_command_row_says_what_its_block_and_readings_say() {
    let ok = command_row(
        &blk(Some(Some(0))),
        Some(1_200_000_000),
        Some((Some(512 << 20), false)),
        (Some("s-7"), "m14", facts()),
    );
    assert_eq!(ok.kind, Kind::Command);
    assert_eq!(ok.wall_ms, Fig::Known(3_500));
    assert_eq!(ok.cpu_ms, Fig::Known(1_200));
    assert_eq!(ok.peak_bytes, Fig::Known(512 << 20));
    assert_eq!(
        (ok.outcome, ok.stop, ok.code, ok.sig),
        (Outcome::Ok, Stop::Exit, Some(0), None)
    );
    assert_eq!(ok.label, "targo");
    assert_eq!(ok.sid.as_deref(), Some("s-7"));
    assert_eq!(
        ok.key,
        Some(Digest::key(
            &Kind::Command,
            "targo build -p aterm",
            Some("/Users//me/aterm")
        )),
        "the assignment is no part of the key"
    );
    assert_eq!(ok.meter, Meter::Exact);
    let line = ok.line();
    assert!(
        !line.contains("SECRET") && !line.contains("/Users//me"),
        "{line}"
    );

    let killed = command_row(
        &blk(Some(Some(130))),
        None,
        None,
        (Some("s-7"), "m14", facts()),
    );
    assert_eq!(
        (killed.outcome, killed.stop, killed.sig),
        (Outcome::Fail, Stop::Signal, Some(2))
    );
    assert_eq!(killed.cpu_ms, Fig::unknown(source::SHELL_RUSAGE));
    assert_eq!(killed.peak_bytes, Fig::unknown(source::JOBS));

    let stuck = command_row(
        &blk(Some(Some(1))),
        None,
        Some((None, true)),
        (Some("s-7"), "m14", facts()),
    );
    assert_eq!((stuck.outcome, stuck.stop), (Outcome::Fail, Stop::Stalled));

    let no_d = command_row(&blk(None), None, None, (Some("s-7"), "m14", facts()));
    assert_eq!(
        (no_d.outcome, no_d.stop, no_d.code),
        (Outcome::Unknown, Stop::Unknown, None)
    );

    // A line no program confirmed (a password typed at the prompt) labels nothing.
    let unconfirmed = BlockFacts {
        line: "hunter2",
        program: None,
        ..blk(Some(Some(127)))
    };
    let typed = command_row(&unconfirmed, None, None, (Some("s-7"), "m14", facts()));
    assert_eq!((typed.label.as_str(), typed.key), ("-", None));
    assert!(!typed.line().contains("hunter2"), "{}", typed.line());
    assert_eq!(no_d.wait_ms, Fig::unknown(source::CAPACITY));
    assert_eq!(no_d.cache, Fig::unknown(source::TARGO));
}

fn fact(detail: Option<&str>, end: crate::session_log::BlockEnd) -> crate::session_log::BlockFact {
    crate::session_log::BlockFact {
        id: 3,
        started_ms: Some(1_000),
        end,
        detail: detail.map(str::to_owned),
        detail_final: true,
    }
}

#[test]
fn a_block_names_only_the_program_its_detail_confirmed() {
    use crate::session_log::BlockEnd;
    let ended = BlockEnd::Unended {
        ended_ms: Some(2_000),
    };
    // A password typed at the prompt: the line's word, which no program of that name ran.
    let unconfirmed = fact(None, ended);
    let typed = block_facts(&unconfirmed, "hunter2", Some("/w")).expect("closed");
    assert_eq!(typed.program, None);
    let row = command_row(&typed, None, None, (Some("s-7"), "m14", facts()));
    assert_eq!((row.label.as_str(), row.key), ("-", None));
    assert!(!row.line().contains("hunter2"), "{}", row.line());
    // A confirmed detail names its program, never the line's first word.
    let git = fact(Some("git status"), ended);
    let confirmed = block_facts(&git, "GIT_PAGER=cat git status", Some("/w")).expect("closed");
    assert_eq!(confirmed.program, Some("git"));
    let running = fact(Some("git"), BlockEnd::Running);
    assert!(block_facts(&running, "git", None).is_none());
}

#[test]
fn a_row_names_its_session_by_the_public_id_the_sweep_noted() {
    note_sid(900_001, "s-public");
    assert_eq!(sid_of(900_001).as_deref(), Some("s-public"));
    assert_eq!(sid_of(900_002), None, "a session the sweep never read");
}

#[test]
fn an_agent_call_row_is_sampled_from_its_job() {
    let ended = aterm_vitals::jobs::Ended {
        tab: 3,
        block: None,
        kind: aterm_vitals::jobs::JobKind::AgentCall,
        consumer: aterm_vitals::jobs::Consumer::AgentFg,
        id: "a77".to_owned(),
        command: "CARGO_TERM_COLOR=never /opt/bin/targo test -p x".to_owned(),
        name: "targo".to_owned(),
        opened_ms: 10_000,
        last_read_ms: Some(18_000),
        ended_ms: 20_000,
        exit: None,
        ended_by: None,
        ran: Some("targo".to_owned()),
        peak_bytes: Some(1 << 30),
        cpu_ns: Some(42_000_000_000),
        stuck: false,
    };
    let row = agent_call_row(&ended, 21_000, 1_793_000_001_000, "m14", facts()).expect("a row");
    assert_eq!(row.at_ms, 1_793_000_000_000);
    assert_eq!(row.wall_ms, Fig::Known(10_000));
    assert_eq!(row.cpu_ms, Fig::Known(42_000));
    assert_eq!(row.label, "targo");
    assert_eq!(row.sid, None, "tab 3's public id was never noted");
    assert_eq!((row.outcome, row.stop), (Outcome::Unknown, Stop::Unknown));
    assert_eq!(row.meter, Meter::Sampled);
    let shell = aterm_vitals::jobs::Ended {
        kind: aterm_vitals::jobs::JobKind::Shell,
        ..ended
    };
    assert!(
        agent_call_row(&shell, 21_000, 0, "m14", facts()).is_none(),
        "a block's job is its command's row"
    );
}

#[test]
fn a_spawn_row_is_ok_or_refused_by_its_answer() {
    let ok = spawn_row("claude", Some("/w"), 900, "OK s-12 tab=3", "m14", facts());
    assert_eq!((ok.kind, ok.outcome), (Kind::Spawn, Outcome::Ok));
    assert_eq!((ok.cpu_ms.clone(), ok.cache.clone()), (Fig::Na, Fig::Na));
    let refused = spawn_row("claude", None, 3, "ERR spawn: no room", "m14", facts());
    assert_eq!(refused.outcome, Outcome::Refused);
}

#[test]
fn the_verb_frames_its_lines_and_its_json() {
    let tmp = aterm_tempfile::tempdir().expect("tempdir");
    let log = aterm_telemetry::Log::new(
        tmp.path().to_path_buf(),
        aterm_telemetry::Limits::DEFAULT,
        aterm_telemetry::HostFacts::default(),
    );
    let mut lines = Vec::new();
    for (i, exit) in [Some(0), Some(1), Some(0)].into_iter().enumerate() {
        let mut row = command_row(
            &blk(Some(exit)),
            Some(1_000_000),
            None,
            (Some("s-1"), "m14", facts()),
        );
        row.at_ms = 1_000 + i as u64;
        lines.push(row.line());
    }
    let mut spawn = spawn_row("claude", None, 5, "OK", "m14", facts());
    spawn.at_ms = 2_000;
    lines.push(spawn.line());
    log.append(&lines).expect("append");

    let summary = cmd_in(tmp.path(), "", false, 10_000, (5, 4, 1));
    let mut it = summary.lines();
    assert_eq!(it.next(), Some("OK 4"), "{summary}");
    assert!(
        it.next()
            .is_some_and(|l| l.starts_with("telemetry schema=1 rows=4 "))
    );
    assert!(
        it.next()
            .is_some_and(|l| l.starts_with("kind=command n=3 failed=1 "))
    );
    assert!(it.next().is_some_and(|l| l.starts_with("kind=spawn n=1 ")));
    assert_eq!(
        it.next(),
        Some("calls watched=5 read=4 missed=1"),
        "{summary}"
    );

    let rows = cmd_in(
        tmp.path(),
        "rows --kind command --limit 2",
        false,
        10_000,
        (0, 0, 0),
    );
    assert!(rows.starts_with("OK 3\n"), "{rows}");
    assert_eq!(rows.matches("kind=command").count(), 2);

    let json = cmd_in(tmp.path(), "since=5s", true, 10_000, (0, 0, 0));
    assert!(json.starts_with("OK 1\n{"), "{json}");
    assert!(json.contains(r#""rows_read":0"#), "{json}");

    assert!(
        cmd_in(tmp.path(), "--since yesterday", false, 10_000, (0, 0, 0))
            .starts_with("ERR telemetry:")
    );
    assert!(cmd_in(tmp.path(), "bogus", false, 10_000, (0, 0, 0)).starts_with("ERR usage:"));
}

fn call(ended_by: Option<aterm_vitals::jobs::ExitOf>, stuck: bool) -> aterm_vitals::jobs::Ended {
    aterm_vitals::jobs::Ended {
        tab: 3,
        block: None,
        kind: aterm_vitals::jobs::JobKind::AgentCall,
        consumer: aterm_vitals::jobs::Consumer::Agent,
        id: "a77".to_owned(),
        command: String::new(),
        name: "zsh".to_owned(),
        opened_ms: 10_000,
        last_read_ms: Some(18_000),
        ended_ms: 20_000,
        exit: None,
        ended_by,
        ran: Some("sleep".to_owned()),
        peak_bytes: None,
        cpu_ns: None,
        stuck,
    }
}

#[test]
fn an_agent_call_row_says_how_its_leader_ended() {
    use aterm_vitals::jobs::ExitOf;
    let at = |ended_by, stuck| {
        let r = agent_call_row(&call(ended_by, stuck), 21_000, 0, "m14", facts()).expect("row");
        (r.outcome, r.stop, r.code, r.sig)
    };
    assert_eq!(
        at(Some(ExitOf::Code(0)), false),
        (Outcome::Ok, Stop::Exit, Some(0), None)
    );
    assert_eq!(
        at(Some(ExitOf::Code(3)), false),
        (Outcome::Fail, Stop::Exit, Some(3), None)
    );
    assert_eq!(
        at(Some(ExitOf::Signal(15)), false),
        (Outcome::Fail, Stop::Signal, None, Some(15)),
        "a death by a signal has no exit status"
    );
    assert_eq!(
        at(None, false),
        (Outcome::Unknown, Stop::Unknown, None, None),
        "an unwatched call's end is unknown, never guessed"
    );
    assert_eq!(
        at(Some(ExitOf::Code(0)), true),
        (Outcome::Ok, Stop::Stalled, Some(0), None),
        "a call judged stuck at its last pass keeps the status it ended with"
    );
}

#[test]
fn an_agent_call_is_labelled_by_the_program_it_ran_never_its_wrapper() {
    let ran = agent_call_row(&call(None, false), 21_000, 0, "m14", facts()).expect("row");
    assert_eq!(ran.label, "sleep", "not the wrapper `zsh`");
    assert_eq!(
        ran.key,
        Some(Digest::key(&Kind::AgentCall, "sleep", None)),
        "keyed by the program, as no join named its command"
    );
    let builtins = aterm_vitals::jobs::Ended {
        ran: None,
        ..call(None, false)
    };
    let row = agent_call_row(&builtins, 21_000, 0, "m14", facts()).expect("row");
    assert_eq!(
        (row.label.as_str(), row.key),
        ("-", None),
        "no program seen"
    );
}

/// A live agent call's row for `pid`, its leader started at `start`.
#[cfg(target_vendor = "apple")]
fn live_call(pid: u32, start: Option<u64>) -> aterm_vitals::jobs::JobRow {
    aterm_vitals::jobs::JobRow {
        leader: (pid, start),
        ..crate::vitals_jobs::tests::call_row(
            3,
            aterm_vitals::jobs::Consumer::Agent,
            false,
            1,
            pid,
            "zsh",
        )
    }
}

#[cfg(target_vendor = "apple")]
fn birth(pid: u32) -> Option<u64> {
    aterm_uds::process::read_birth(i32::try_from(pid).ok()?).map(|b| b.0)
}

#[cfg(target_vendor = "apple")]
#[test]
fn a_watched_leader_hands_the_next_pass_its_status_decoded() {
    use aterm_vitals::jobs::ExitOf;
    for (script, end) in [
        ("read _; exit 3", ExitOf::Code(3)),
        ("read _; kill -TERM $$", ExitOf::Signal(15)),
    ] {
        let mut child = aterm_uds::spawnfd::command("/bin/sh")
            .args(["-c", script])
            .stdin(aterm_uds::spawnfd::Stdio::piped())
            .spawn()
            .expect("spawn");
        let pid = child.id();
        let start = birth(pid);
        let mut calls = CallWatch::new(true);
        calls.watch(&[live_call(pid, start)]);
        assert_eq!(calls.watching(), 1, "a live leader is watched");
        assert!(calls.read(|_| true).is_empty(), "nothing ended yet");
        drop(child.stdin.take());
        let _ = child.wait();
        // The sweep no longer lists the reaped leader: its event is read, waited for briefly.
        let exits = calls.read(|_| false);
        assert_eq!(
            exits,
            vec![aterm_vitals::jobs::LeaderExit { pid, start, end }],
            "{script}"
        );
        assert_eq!(calls.watching(), 0, "its watch is let go");
    }
}

#[cfg(target_vendor = "apple")]
#[test]
fn a_pid_taken_anew_or_never_read_is_never_watched_as_the_calls() {
    let mut child = aterm_uds::spawnfd::command("/bin/sleep")
        .arg("600")
        .spawn()
        .expect("spawn");
    let pid = child.id();
    let start = birth(pid).expect("a live child's birth");
    let mut calls = CallWatch::new(true);
    // The row read another process's start under this pid: the watch is let go at once.
    calls.watch_with(&[live_call(pid, Some(start + 1))], birth);
    assert_eq!(calls.watching(), 0, "another process's pid");
    calls.watch_with(&[live_call(pid, None)], birth);
    assert_eq!(calls.watching(), 0, "a start never read");
    calls.watch_with(&[live_call(pid, Some(start))], birth);
    assert_eq!(calls.watching(), 1, "the true start is watched");
    // A pass whose sweep missed the leader, though it lives, keeps its watch.
    assert!(calls.read(|_| false).is_empty());
    assert_eq!(
        calls.watching(),
        1,
        "a live leader's watch outlives a partial sweep"
    );
    // A call the engine no longer holds open lets its watch go.
    calls.watch_with(&[], birth);
    assert_eq!(calls.watching(), 0);
    let _ = child.kill();
    let _ = child.wait();
}

#[cfg(target_vendor = "apple")]
#[test]
fn past_the_cap_a_call_waits_for_a_free_watch() {
    let mut kids: Vec<_> = (0..=CALL_WATCH_CAP)
        .map(|_| {
            aterm_uds::spawnfd::command("/bin/sleep")
                .arg("600")
                .spawn()
                .expect("spawn")
        })
        .collect();
    let rows: Vec<_> = kids
        .iter()
        .map(|k| live_call(k.id(), birth(k.id())))
        .collect();
    let mut calls = CallWatch::new(true);
    calls.watch_with(&rows, birth);
    assert_eq!(calls.watching(), CALL_WATCH_CAP);
    calls.watch_with(&rows[1..], birth);
    calls.watch_with(&rows[1..], birth);
    assert_eq!(
        calls.watching(),
        CALL_WATCH_CAP,
        "the call past the cap is watched once one ends"
    );
    for k in &mut kids {
        let _ = k.kill();
        let _ = k.wait();
    }
}

#[test]
fn a_call_that_ends_with_no_status_is_counted_missed() {
    use aterm_vitals::jobs::{ExitOf, JobKind};
    let shell = aterm_vitals::jobs::Ended {
        kind: JobKind::Shell,
        ..call(None, false)
    };
    let ended = [call(None, false), call(Some(ExitOf::Code(0)), false), shell];
    assert_eq!(count_missed(&ended), 1, "only the call with no status");
}

#[cfg(target_vendor = "apple")]
#[test]
fn a_test_pass_watches_nothing() {
    let mut calls = CallWatch::new(false);
    calls.watch(&[live_call(std::process::id(), birth(std::process::id()))]);
    assert_eq!(calls.watching(), 0);
}
