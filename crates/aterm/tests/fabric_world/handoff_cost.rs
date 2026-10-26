// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Seamless update bridge handoff and clean-stop flush counts (sim item S6).

use crate::harness;

use harness::{World, alive, until};

/// The sessions the instance hosts: its boot session and seven spawned.
const SESSIONS: usize = 8;
/// The topic every session opts into.
const TOPIC: &str = "s6.cost";

/// Whether `line`, one that counts or times durable flushes, is of the flushes aterm ships:
/// it carries no [`aterm_durable::PLAIN_DURABLE_FLUSHES`]. Every instance whose lines are
/// MEASURED here is relaunched by [`World::relaunch_gui_shipping_flushes`]; a world's first
/// instance keeps this test binary's mark, and [`its_first_start_is_marked`] shows on it that
/// this check can fail.
fn ships(line: &str) -> bool {
    !line.contains(aterm_durable::PLAIN_DURABLE_FLUSHES)
}

/// CONTROL for [`ships`]: the world's first instance keeps this test binary's mark, so on
/// Apple targets every start line it printed names its plain `fsync(2)`s. Read before any
/// relaunch, while the log holds only its lines.
fn its_first_start_is_marked(w: &World) {
    let began = said(w, 0, "aterm-link: the run record says a bridge runs");
    assert!(
        !began.is_empty(),
        "the first instance's start says what it flushed"
    );
    for line in &began {
        assert_eq!(
            ships(line),
            !cfg!(target_vendor = "apple"),
            "CONTROL: under this test binary's mark, a start line names its plain fsync(2)s \
             on Apple targets: {line}"
        );
    }
}

/// The lines of aterm's log past byte `from` that start with `prefix`.
fn said(w: &World, from: usize, prefix: &str) -> Vec<String> {
    let log = std::fs::read_to_string(&w.gui_log).unwrap_or_default();
    log.get(from..)
        .unwrap_or_default()
        .lines()
        .filter(|l| l.starts_with(prefix))
        .map(str::to_string)
        .collect()
}

/// How long aterm's log is now: what a later [`said`] reads past.
fn log_len(w: &World) -> usize {
    std::fs::read_to_string(&w.gui_log).map_or(0, |log| log.len())
}

/// The number just before `before` on `line`.
fn number(line: &str, before: &str) -> u64 {
    line.split(before)
        .next()
        .and_then(|head| head.split_whitespace().last())
        .and_then(|n| n.parse().ok())
        .unwrap_or_else(|| panic!("a number before `{before}` in: {line}"))
}

/// Quit the instance the way a person does (`invoke Quit`) and wait for it,
/// then for its bridge, to exit: what the bridge said is in the log by then.
fn quit(w: &mut World) {
    let bridge = w.bridge_pid();
    let mut gui = w.take_gui().expect("the instance");
    let _ = w.ctl().request("invoke Quit");
    until("aterm to exit", || gui.try_wait().ok().flatten());
    until("its bridge to exit after it", || {
        (!alive(bridge)).then_some(())
    });
    let _ = std::fs::remove_file(w.state.join("pid"));
}

/// **A HANDOFF FLUSHES NOTHING, AND ITS SUCCESSOR'S CLEAN STOP FLUSHES WHAT
/// THE RUN WROTE.** Eight sessions in use leave sixteen torn-kind files
/// written with no flush; the instance hands off: its bridge flushes nothing
/// and leaves the run open, and its successor, on the same dir in the same
/// boot, establishes its own durable OPEN fence by two full flushes. The
/// successor then quits,
/// and its clean stop flushes every one of the sixteen it inherited where it is (nothing
/// rewritten), each by the barrier, and ends with one full flush.
#[test]
fn a_handoff_flushes_nothing_and_the_quit_after_it_flushes_the_run() {
    let mut w = World::boot_with("s6-handoff", &[], &[("ATERM_DEBUG_QUIT_BYE", "handoff")]);
    w.wait_ready();
    let (first, _) = w.two_sessions();
    for _ in 2..SESSIONS {
        let reply = w.verb("spawn");
        assert!(reply.ok(), "spawn: {}", reply.header());
    }
    let sids: Vec<String> = until("the bridge to see every session", || {
        let rows = w.sessions();
        (rows.len() >= SESSIONS).then(|| rows.into_iter().map(|(_, sid, _)| sid).collect())
    });
    for sid in &sids {
        let added = w.verb(&format!("@{sid} topic add {TOPIC}"));
        assert!(added.ok(), "topic add: {}", added.header());
        until("the bridge to hold the topic", || {
            harness::bridge_holds(&w, sid, TOPIC)
        });
    }
    let shout = w.verb(&format!("@{first} post to=say:{TOPIC} kind=note s6shout"));
    assert!(shout.ok(), "{}", shout.header());
    for sid in &sids {
        until("the broadcast's refill floor", || {
            std::fs::read_to_string(w.state.join(format!("unhandled/{sid}")))
                .ok()
                .filter(|floor| floor.starts_with(TOPIC))
                .map(|_| ())
        });
    }
    for (i, sid) in sids.iter().enumerate() {
        let posted = w.verb(&format!(
            "@{sid} post to=@{first} kind=note key=s6-k{i} --wait=60000 keyed"
        ));
        assert!(
            posted.ok() && posted.header().contains("off="),
            "keyed post: {}",
            posted.header()
        );
        assert!(w.state.join(format!("keys/{sid}")).exists());
    }

    let before = log_len(&w);
    quit(&mut w);
    let stopped = said(&w, before, "aterm-link: the run stopped cleanly:");
    let handed = said(&w, before, "aterm-link: handed off:");
    eprintln!("MEASURED handoff, the old bridge: {stopped:?} {handed:?}");
    assert!(stopped.is_empty(), "a handoff is not the run's end");
    assert_eq!(handed.len(), 1, "it says so once");
    its_first_start_is_marked(&w);

    let successor = log_len(&w);
    let gui = w.gui.insert(World::relaunch_gui_shipping_flushes(&w));
    w.token = World::wait_for_owned_token_of(&w.ctl_sock, gui, &w.gui_log);
    w.wait_ready();
    let began = said(
        &w,
        successor,
        "aterm-link: the run record says a bridge runs",
    );
    eprintln!("MEASURED handoff, the successor's start: {began:?}");
    assert_eq!(
        began.len(),
        1,
        "the successor establishes its own OPEN fence"
    );
    assert_eq!(
        (
            number(&began[0], "barriers"),
            number(&began[0], "full flushes")
        ),
        (0, 2),
        "over the open run it inherits, in full: {}",
        began[0]
    );
    assert!(ships(&began[0]), "timed as it ships: {}", began[0]);

    let before = log_len(&w);
    quit(&mut w);
    let stopped = said(&w, before, "aterm-link: the run stopped cleanly:");
    eprintln!("MEASURED quit after the handoff, its clean stop: {stopped:?}");
    let floor = said(
        &w,
        successor,
        "aterm-link: mirror: the floor of the node's total",
    );
    eprintln!("MEASURED handoff, the successor's first floor raise: {floor:?}");
    assert!(
        floor.iter().all(|l| ships(l)),
        "timed as it ships: {floor:?}"
    );
    let [line] = stopped.as_slice() else {
        panic!("one stop line, not {stopped:?}");
    };
    assert!(ships(line), "timed as it ships: {line}");
    let files = number(line, "state files");
    assert!(
        files >= 2 * SESSIONS as u64,
        "every session's floor and key, inherited: {line}"
    );
    assert_eq!(
        (number(line, "barriers"), number(line, "full flushes")),
        (files + number(line, "directories flushed") + 1, 1),
        "a barrier a file, a directory and the record's bytes, then one full flush: {line}"
    );
}

/// **A START OVER AN EARLIER BOOT'S CLEAN STOP IS ORDERED BY TWO BARRIERS**
/// (B07, `StateDir::begin_run`): the first start of a boot after a quit in the
/// boot before. The barrier orders a start over any earlier boot's record, an
/// open one (a crash or a power cut in the boot before) as well as a clean
/// one, and never over a record of this boot. A test cannot reboot, so the
/// clean stop's record is rewritten to name another boot before the relaunch;
/// `inc`'s lease still names this boot, so what the start's line times is the
/// fence alone.
#[test]
fn a_start_over_an_earlier_boots_clean_stop_is_ordered_by_two_barriers() {
    let mut w = World::boot("b07-reboot", &[]);
    w.wait_ready();
    let before = log_len(&w);
    quit(&mut w);
    let stopped = said(&w, before, "aterm-link: the run stopped cleanly:");
    assert_eq!(
        stopped.len(),
        1,
        "a quit stops the run cleanly: {stopped:?}"
    );
    its_first_start_is_marked(&w);
    let run = w.state.join("run");
    let record = std::fs::read_to_string(&run).expect("the clean stop's record");
    let boot = record
        .trim()
        .strip_prefix("clean boot=")
        .filter(|id| id.len() == 32 && id.bytes().all(|b| b.is_ascii_hexdigit()))
        .unwrap_or_else(|| panic!("a clean record naming a boot: {record:?}"));
    let earlier: String = boot
        .chars()
        .enumerate()
        .map(|(i, c)| match (i, c) {
            (0, '0') => '1',
            (0, _) => '0',
            (_, c) => c,
        })
        .collect();
    std::fs::write(&run, format!("clean boot={earlier}\n")).expect("an earlier boot's record");

    let start = log_len(&w);
    let gui = w.gui.insert(World::relaunch_gui_shipping_flushes(&w));
    w.token = World::wait_for_owned_token_of(&w.ctl_sock, gui, &w.gui_log);
    w.wait_ready();
    let began = said(&w, start, "aterm-link: the run record says a bridge runs");
    eprintln!("MEASURED a start over an earlier boot's clean stop: {began:?}");
    let [line] = began.as_slice() else {
        panic!("one start line, not {began:?}");
    };
    assert_eq!(
        (number(line, "barriers"), number(line, "full flushes")),
        (2, 0),
        "over an earlier boot's record, by the barrier: {line}"
    );
    assert!(ships(line), "timed as it ships: {line}");
    quit(&mut w);
}
