// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The Muse Code live upgrade's pure half: the revision-aware order of two
//! builds, the session log's part of a process and what in it holds a restart
//! off (records shaped as 1.4.10-R6427.1 writes them, measured in the lab),
//! and the gate's order. Each guard positive and negative.

use std::cmp::Ordering;

use super::*;

/// The pid of the process whose part of the log is read, and another.
const PID: u32 = 57_495;
const OTHER_PID: u32 = 72_752;
/// The process's start (microseconds) and its log's records after it.
const START_US: u64 = 1_792_284_625_000_000;

/// One record line as Muse writes it: `payload_type`, `payload`, at `at_us`.
fn rec(payload_type: &str, payload: &str, at_us: u64) -> String {
    format!(
        r#"{{"schema_version":1,"stream":{{"kind":"session","id":"s"}},"sequence":1,"recorded_at":{at_us},"record_type":"event","durability":"durable","payload_type":"{payload_type}","payload_schema_version":1,"payload":{payload}}}"#
    )
}

fn route(pid: u32, at_us: u64) -> String {
    rec(
        "runtime.session.route_facts",
        &format!(
            r#"{{"kind":"route_facts","record":{{"cwd":"/w","pid":{pid},"terminal_title_identity":"t"}}}}"#
        ),
        at_us,
    )
}

fn run(id: &str, event: &str, at_us: u64) -> String {
    rec(
        "runtime.session",
        &format!(r#"{{"kind":"run","run_id":"{id}","event":{event}}}"#),
        at_us,
    )
}

fn task(id: &str, event: &str, at_us: u64) -> String {
    rec(
        "runtime.session",
        &format!(r#"{{"kind":"task","run_id":"r","task_id":"{id}","event":{event}}}"#),
        at_us,
    )
}

/// A whole turn: started, then terminal.
fn turn(id: &str, at_us: u64) -> Vec<String> {
    vec![
        run(id, r#"{"kind":"started","prompt":"hi"}"#, at_us),
        run(
            id,
            r#"{"kind":"terminal","terminal":"completed","reason":null}"#,
            at_us + 10,
        ),
    ]
}

/// A backgrounded shell's records while it runs (the measured shape: the
/// task accepted and started, its status `terminal process running`, its
/// output `background_running`).
fn backgrounded(id: &str, at_us: u64) -> Vec<String> {
    vec![
        task(id, r#"{"kind":"proposed","task_kind":"tool.bash"}"#, at_us),
        task(id, r#"{"kind":"accepted"}"#, at_us + 1),
        task(id, r#"{"kind":"started","span_id":"x"}"#, at_us + 2),
        task(
            id,
            r#"{"kind":"status","message":"terminal process running","details":{"facets":[{"kind":"live_process","system":"unified_exec"}]}}"#,
            at_us + 3,
        ),
        task(
            id,
            r#"{"kind":"output","chunk":"{\n  \"execution_state\": \"background_running\",\n  \"work_id\": \"w\"\n}"}"#,
            at_us + 4,
        ),
    ]
}

/// Its end: the inbox item queued, the output with its exit code, the task
/// completed.
fn bg_end(id: &str, run_id: &str, at_us: u64) -> Vec<String> {
    vec![
        run(
            run_id,
            &format!(
                r#"{{"kind":"inbox_item_queued","source":{{"source":"background_task_terminal","task_id":"{id}","terminal_kind":"completed"}},"summary":"background completed","disposition":"queue"}}"#
            ),
            at_us,
        ),
        task(
            id,
            r#"{"kind":"output","chunk":"{\"exit_code\":0,\"terminal_status\":\"completed\"}"}"#,
            at_us + 1,
        ),
        task(id, r#"{"kind":"completed"}"#, at_us + 2),
    ]
}

/// Muse's own run on it: started with an empty prompt, the item drained.
fn drain(id: &str, run_id: &str, at_us: u64) -> Vec<String> {
    vec![
        run(run_id, r#"{"kind":"started","prompt":""}"#, at_us),
        run(
            run_id,
            &format!(
                r#"{{"kind":"inbox_item_drained","item_id":"i","delivery_snapshot":{{"source":{{"source":"background_task_terminal","task_id":"{id}","terminal_kind":"completed"}}}}}}"#
            ),
            at_us + 1,
        ),
    ]
}

fn log(lines: &[Vec<String>]) -> String {
    let mut out = String::new();
    for l in lines.iter().flatten() {
        out.push_str(l);
        out.push('\n');
    }
    out
}

/// THE ORDER IS REVISION-AWARE: atpkg's grammar reads `1.4.11-R6500.1` over
/// `1.4.10-R6427.1`, a higher revision of the same `x.y.z` over a lower, and
/// a sub-revision over none. NEGATIVE CONTROLS: the Claude lane's
/// `upgrade::Version` refuses the revision form outright (the very reason
/// this order exists); the same version, an older revision and an older
/// `x.y.z` are no move; two grammars and an unparsable word have no order.
#[test]
fn the_order_reads_the_revision_form() {
    assert_eq!(
        crate::harness::upgrade::Version::parse("1.4.10-R6427.1"),
        None,
        "the Claude lane's version refuses Muse's form"
    );
    for (running, target) in [
        ("1.4.10-R6427.1", "1.4.11-R6500.1"),
        ("1.4.10-R6427.1", "1.4.10-R6500.1"),
        ("1.4.10-R6427", "1.4.10-R6427.1"),
        ("1.4.10-R6427.1", "2.0.0-R1"),
    ] {
        assert_eq!(
            order(running, target),
            Some(Ordering::Greater),
            "{running} {target}"
        );
        assert!(moves_onto(running, target), "{running} -> {target}");
    }
    for (running, target, want) in [
        ("1.4.10-R6427.1", "1.4.10-R6427.1", Some(Ordering::Equal)),
        ("1.4.10-R6427.1", "1.4.10-R6400.1", Some(Ordering::Less)),
        ("1.4.11-R6500.1", "1.4.10-R6900.1", Some(Ordering::Less)),
        ("1.4.10-R6427.1", "1.4.11", None),
        ("1.4.10-R6427.1", "1.4.11-beta", None),
        ("1.4.10-R6427.1", "", None),
    ] {
        assert_eq!(order(running, target), want, "{running} {target}");
        assert!(!moves_onto(running, target), "{running} -> {target}");
    }
}

/// A build's version is its record's, only where the build's directory is
/// that version's build id, as atpkg lays it. NEGATIVE CONTROLS: a record
/// whose directory names another build, a directory that is no number, and a
/// version the grammar refuses.
#[test]
fn a_build_version_is_its_record_under_its_own_build_id() {
    assert!(build_version("1.4.10-R6427.1", Some("2001004010006427002")).is_some());
    assert!(build_version("1.4.11-R6500.1", Some("2001004011006500002")).is_some());
    assert!(build_version("1.4.11-R6500.1", None).is_some());
    assert!(build_version("1.4.11-R6500.1", Some("2001004010006427002")).is_none());
    assert!(build_version("1.4.11-R6500.1", Some("current")).is_none());
    assert!(build_version("1.4.11-R6500.x", None).is_none());
}

/// THE PROCESS'S PART OF THE LOG is from the LAST route record naming its
/// pid, written since it started. NEGATIVE CONTROLS: a log with another
/// process's route only, and a route of the same pid number written before
/// this process started (an earlier process's), prove no part.
#[test]
fn the_part_of_a_process_starts_at_its_own_route_record() {
    let text = log(&[
        vec![route(OTHER_PID, START_US - 500)],
        turn("r0", START_US - 400),
        vec![route(PID, START_US + 1)],
        turn("r1", START_US + 2),
    ]);
    let (at, from) = part_start(&text, PID, START_US).expect("its part");
    assert_eq!(from, START_US + 1);
    assert!(text[at..].contains("r1") && !text[at..].contains("r0"));
    assert_eq!(part_start(&text, 4_242, START_US), None, "no route of it");
    let stale = log(&[vec![route(PID, START_US - 9_000_000)], turn("r", START_US)]);
    assert_eq!(
        part_start(&stale, PID, START_US),
        None,
        "an earlier process's"
    );
    assert_eq!(quiet_log(&stale, PID, START_US), Err(Open::Unproved));
}

/// G-M1: a run started with no terminal holds; every run ended (a cancelled
/// one too) does not. NEGATIVE CONTROL: an earlier process's open run (a
/// SIGKILLed one, before this process's route) is no run of this part.
#[test]
fn an_open_run_holds_and_an_ended_one_does_not() {
    let open = log(&[
        vec![route(PID, START_US)],
        turn("r1", START_US + 1),
        vec![run(
            "r2",
            r#"{"kind":"started","prompt":"go"}"#,
            START_US + 50,
        )],
    ]);
    assert_eq!(quiet_log(&open, PID, START_US), Err(Open::Turn));
    let cancelled = log(&[
        vec![route(PID, START_US)],
        vec![
            run("r2", r#"{"kind":"started","prompt":"go"}"#, START_US + 50),
            run(
                "r2",
                r#"{"kind":"terminal","terminal":"cancelled","reason":"cancelled during model step"}"#,
                START_US + 60,
            ),
        ],
    ]);
    assert_eq!(quiet_log(&cancelled, PID, START_US), Ok(START_US));
    let earlier = log(&[
        vec![route(OTHER_PID, START_US - 900)],
        vec![run(
            "r0",
            r#"{"kind":"started","prompt":"go"}"#,
            START_US - 800,
        )],
        vec![route(PID, START_US)],
        turn("r1", START_US + 1),
    ]);
    assert_eq!(quiet_log(&earlier, PID, START_US), Ok(START_US));
}

/// G-M2, the measured backgrounded shell: while it runs its run is over and
/// its task open (`background-work`); once it ended its item stands queued
/// until Muse's own run drains it (`queued-work`), and that run is open
/// (`turn-open`) until its terminal; then nothing holds. A task's end is any
/// of the four: `failed` and `rejected` close it as `completed` and
/// `cancelled` do. NEGATIVE CONTROL: a task with no background word is still
/// open work (`open-task`).
#[test]
fn background_work_and_its_queued_end_hold_until_drained() {
    let base = || vec![vec![route(PID, START_US)], turn("r1", START_US + 1)];
    let mut running = base();
    running.push(backgrounded("t1", START_US + 100));
    assert_eq!(
        quiet_log(&log(&running), PID, START_US),
        Err(Open::Task { background: true })
    );
    let mut queued = running.clone();
    queued.push(bg_end("t1", "r1", START_US + 200));
    assert_eq!(quiet_log(&log(&queued), PID, START_US), Err(Open::Queued));
    let mut draining = queued.clone();
    draining.push(drain("t1", "r3", START_US + 300));
    assert_eq!(quiet_log(&log(&draining), PID, START_US), Err(Open::Turn));
    let mut done = draining.clone();
    done.push(vec![run(
        "r3",
        r#"{"kind":"terminal","terminal":"completed","reason":null}"#,
        START_US + 400,
    )]);
    assert_eq!(quiet_log(&log(&done), PID, START_US), Ok(START_US));
    for end in ["completed", "cancelled", "failed", "rejected"] {
        let closed = log(&[
            vec![route(PID, START_US)],
            vec![
                task("t2", r#"{"kind":"accepted"}"#, START_US + 1),
                task("t2", r#"{"kind":"started"}"#, START_US + 2),
                task("t2", &format!(r#"{{"kind":"{end}"}}"#), START_US + 3),
            ],
        ]);
        assert_eq!(quiet_log(&closed, PID, START_US), Ok(START_US), "{end}");
    }
    let plain = log(&[
        vec![route(PID, START_US)],
        vec![
            task("t3", r#"{"kind":"accepted"}"#, START_US + 1),
            task(
                "t3",
                r#"{"kind":"status","message":"opening stream"}"#,
                START_US + 2,
            ),
        ],
    ]);
    assert_eq!(
        quiet_log(&plain, PID, START_US),
        Err(Open::Task { background: false })
    );
    // The other task shape Muse writes (`runtime.session.task`).
    let other = log(&[
        vec![route(PID, START_US)],
        vec![rec(
            "runtime.session.task",
            r#"{"task_id":"t4","event":{"kind":"started","task_id":"t4"}}"#,
            START_US + 1,
        )],
    ]);
    assert_eq!(
        quiet_log(&other, PID, START_US),
        Err(Open::Task { background: false })
    );
}

/// A drain of another item drains nothing of this one; an item drained
/// before it was queued (not in order) is still queued.
#[test]
fn a_drain_closes_only_its_own_item() {
    let text = log(&[
        vec![route(PID, START_US)],
        bg_end("t1", "r1", START_US + 1),
        drain("t9", "r2", START_US + 2),
        vec![run(
            "r2",
            r#"{"kind":"terminal","terminal":"completed"}"#,
            START_US + 3,
        )],
    ]);
    assert_eq!(quiet_log(&text, PID, START_US), Err(Open::Queued));
}

/// FAIL CLOSED: a line that does not parse — a torn tail as it is written —
/// proves nothing (`log-unreadable`). A retained frame or marker (no
/// `payload_type`) is no record and holds nothing.
#[test]
fn a_torn_log_fails_closed_and_a_frame_is_no_record() {
    let mut text = log(&[vec![route(PID, START_US)], turn("r1", START_US + 1)]);
    text.push_str(r#"{"schema_version":1,"payload_type":"runtime.sess"#);
    assert_eq!(quiet_log(&text, PID, START_US), Err(Open::Unreadable));
    let framed = format!(
        "{}{}\n",
        log(&[vec![route(PID, START_US)]]),
        r#"{"retained_marker":"omitted_live_only","schema_version":1,"omitted_record":{"record_type":"status"}}"#
    );
    assert_eq!(quiet_log(&framed, PID, START_US), Ok(START_US));
}

/// A SUBAGENT'S LOG has no route of its own: read from the part's start by
/// its records' time. Its open task holds; one recorded before the part (an
/// earlier process's, never ended) does not.
#[test]
fn a_subagent_log_is_read_from_the_parts_start() {
    let open = log(&[backgrounded("s1", START_US + 5)]);
    assert_eq!(
        open_work(&open, START_US),
        Err(Open::Task { background: true })
    );
    let earlier = log(&[backgrounded("s0", START_US - 5)]);
    assert_eq!(open_work(&earlier, START_US), Ok(()));
}

/// THE GATE'S ORDER: a person, the screen, the draft, the log, the process
/// table, a child — each alone holds with its own word, and with none the
/// restart goes.
#[test]
fn the_gate_names_each_hold_and_lets_a_quiet_point_go() {
    let quiet = Look {
        held: false,
        idle: true,
        composer_empty: true,
        log: Ok(()),
        children: Some(0),
    };
    assert_eq!(gate(&quiet), Ok(()));
    for (look, word) in [
        (
            Look {
                held: true,
                ..quiet
            },
            "held",
        ),
        (
            Look {
                idle: false,
                ..quiet
            },
            "screen-not-idle",
        ),
        (
            Look {
                composer_empty: false,
                ..quiet
            },
            "draft",
        ),
        (
            Look {
                log: Err(Open::Turn),
                ..quiet
            },
            "turn-open",
        ),
        (
            Look {
                log: Err(Open::Task { background: true }),
                ..quiet
            },
            "background-work",
        ),
        (
            Look {
                log: Err(Open::Task { background: false }),
                ..quiet
            },
            "open-task",
        ),
        (
            Look {
                log: Err(Open::Queued),
                ..quiet
            },
            "queued-work",
        ),
        (
            Look {
                log: Err(Open::Unproved),
                ..quiet
            },
            "log-unproved",
        ),
        (
            Look {
                log: Err(Open::Unreadable),
                ..quiet
            },
            "log-unreadable",
        ),
        (
            Look {
                children: None,
                ..quiet
            },
            "process-table",
        ),
        (
            Look {
                children: Some(1),
                ..quiet
            },
            "child-process",
        ),
    ] {
        assert_eq!(gate(&look), Err(word), "{look:?}");
    }
}

/// The measured backgrounded-shell log (2026-10-17, records 161-251 of the
/// lab's `q3-session-after-bg-finish.jsonl`, reduced to the records the gate
/// reads and in their order): every stage of it reads as measured.
#[test]
fn the_measured_background_shell_reads_as_measured() {
    // While it runs: the run's terminal (record 228) is written after the
    // task's `background_running` output (174), and the task is still open.
    let mut lines = vec![vec![route(PID, START_US)]];
    lines.push(vec![run(
        "r1",
        r#"{"kind":"started","prompt":"toolstart"}"#,
        START_US + 10,
    )]);
    lines.push(backgrounded("01a14c7f-aabd", START_US + 20));
    lines.push(vec![run(
        "r1",
        r#"{"kind":"terminal","terminal":"completed"}"#,
        START_US + 30,
    )]);
    let running = log(&lines);
    assert_eq!(
        quiet_log(&running, PID, START_US),
        Err(Open::Task { background: true }),
        "the run is over and the shell runs on"
    );
}

/// THE PART IS BOUNDED BY WHERE ITS ROUTE RECORD LIES IN THE FILE, never by
/// the records' wall clock: a run started and a task accepted AFTER the route
/// record but stamped before it (the clock stepped back) are this process's
/// open work. NEGATIVE CONTROL: an earlier process's open run written BEFORE
/// the route record holds nothing, however late its stamp says it was.
#[test]
fn a_record_written_after_the_route_holds_whatever_its_stamp() {
    let stepped_back = log(&[
        vec![route(PID, START_US)],
        vec![run(
            "r1",
            r#"{"kind":"started","prompt":"go"}"#,
            START_US - 5_000_000,
        )],
    ]);
    assert_eq!(quiet_log(&stepped_back, PID, START_US), Err(Open::Turn));
    let task_back = log(&[
        vec![route(PID, START_US)],
        turn("r1", START_US + 1),
        vec![task("t1", r#"{"kind":"accepted"}"#, START_US - 4_000_000)],
    ]);
    assert_eq!(
        quiet_log(&task_back, PID, START_US),
        Err(Open::Task { background: false })
    );
    let before_route = log(&[
        vec![run(
            "r0",
            r#"{"kind":"started","prompt":"go"}"#,
            START_US + 9_000_000,
        )],
        vec![route(PID, START_US)],
        turn("r1", START_US + 1),
    ]);
    assert_eq!(quiet_log(&before_route, PID, START_US), Ok(START_US));
}

/// The session-name reservation's records as 1.4.10-R6427.1 writes them
/// (`runtime.session.task`, the payloads of a real log's records 14-18,
/// `d1rv`, 2026-10-16), for task `id` of kind `kind`: proposed, accepted,
/// scheduled, its side-effect intent, started — and never ended.
fn reservation(id: &str, kind: &str, at_us: u64) -> Vec<String> {
    let t = |event: String, at: u64| {
        rec(
            "runtime.session.task",
            &format!(r#"{{"task_id":"{id}","event":{event}}}"#),
            at,
        )
    };
    vec![
        t(
            format!(r#"{{"kind":"proposed","task_id":"{id}","task_kind":"{kind}"}}"#),
            at_us,
        ),
        t(
            format!(r#"{{"kind":"accepted","task_id":"{id}"}}"#),
            at_us + 1,
        ),
        t(
            format!(r#"{{"kind":"scheduled","task_id":"{id}","idempotency_key":"k"}}"#),
            at_us + 2,
        ),
        t(
            format!(
                r#"{{"kind":"side_effect_intent","task_id":"{id}","operation":"{kind}","idempotency_key":"k","policy_decision":"not_applicable","cancellation_handle":"runtime-task:{id}","parent_task_id":null}}"#
            ),
            at_us + 3,
        ),
        t(
            format!(r#"{{"kind":"started","task_id":"{id}"}}"#),
            at_us + 4,
        ),
    ]
}

/// G-M2 READS NO WORK INTO THE SESSION-NAME RESERVATION: a process whose
/// `session_name.allocate` task stayed open (as 55 of the 72 in the kept lab
/// logs did, the next process failing it at its resume and proposing its
/// own) is upgraded once its turns and its background work are over — the
/// real `d1rv` part's shape: the reservation at the start, a turn, a
/// backgrounded shell that ended and was drained. NEGATIVE CONTROLS: the same
/// records of any other kind (`tool.bash`) are open work; the reservation
/// exempts only its own task, never another left open beside it; and a task
/// whose `proposed` record is missing is work.
#[test]
fn an_open_session_name_reservation_holds_no_restart() {
    let part = |kind: &str| {
        let mut lines = vec![vec![route(PID, START_US)]];
        lines.push(reservation("01a147af-bdf6", kind, START_US + 100));
        lines.push(turn("r1", START_US + 200));
        lines.push(backgrounded("t1", START_US + 300));
        lines.push(bg_end("t1", "r1", START_US + 400));
        lines.push(drain("t1", "r2", START_US + 500));
        lines.push(vec![run(
            "r2",
            r#"{"kind":"terminal","terminal":"completed"}"#,
            START_US + 600,
        )]);
        lines
    };
    assert_eq!(
        quiet_log(&log(&part("session_name.allocate")), PID, START_US),
        Ok(START_US),
        "the reservation is no work"
    );
    assert_eq!(
        quiet_log(&log(&part("tool.bash")), PID, START_US),
        Err(Open::Task { background: false }),
        "the same records of a tool are work"
    );
    let mut beside = part("session_name.allocate");
    beside.push(vec![
        task(
            "t9",
            r#"{"kind":"proposed","task_kind":"model.meta.response"}"#,
            START_US + 700,
        ),
        task("t9", r#"{"kind":"accepted"}"#, START_US + 701),
    ]);
    assert_eq!(
        quiet_log(&log(&beside), PID, START_US),
        Err(Open::Task { background: false }),
        "another task left open beside it"
    );
    let unproposed = log(&[
        vec![route(PID, START_US)],
        reservation("01a147af-bdf6", "session_name.allocate", START_US + 100)[1..].to_vec(),
    ]);
    assert_eq!(
        quiet_log(&unproposed, PID, START_US),
        Err(Open::Task { background: false }),
        "no kind read, so work"
    );
}
