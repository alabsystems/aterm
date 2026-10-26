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
    use_sessions(&w);

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

/// Put [`SESSIONS`] sessions to use, as a person's day does: each opts into [`TOPIC`] and
/// holds a refill floor on it (`unhandled/<sid>`), and each makes a keyed post (`keys/<sid>`).
/// Returns their sids.
fn use_sessions(w: &World) -> Vec<String> {
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
            harness::bridge_holds(w, sid, TOPIC)
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
    sids
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
    name_an_earlier_boot(&w);

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

/// Rewrite the clean stop's run record, and the incarnation lease its last attach wrote, to name
/// another boot: a test cannot reboot, so this is what the first start of the next boot finds.
fn name_an_earlier_boot(w: &World) {
    name_an_earlier_boot_in(w, true);
}

/// [`name_an_earlier_boot`], the lease moved too where `lease`. A lease left naming this boot is
/// an attach of this boot, as an older build's, which writes no run record, leaves it, and no
/// reboot does (`StateDir::last_ran_in_an_earlier_boot`).
fn name_an_earlier_boot_in(w: &World, lease: bool) {
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
    let lease_path = w.state.join("inc-lease");
    let leased = std::fs::read_to_string(&lease_path).expect("the run's attach's lease");
    let this_boot = format!("boot={boot}");
    assert_eq!(
        leased
            .split_whitespace()
            .filter(|t| *t == this_boot)
            .count(),
        1,
        "the run's last attach leased in its boot: {leased:?}"
    );
    if lease {
        std::fs::write(
            &lease_path,
            leased.replace(&this_boot, &format!("boot={earlier}")),
        )
        .expect("an earlier boot's lease");
    }
    std::fs::write(&run, format!("clean boot={earlier}\n")).expect("an earlier boot's record");
}

/// The torn-kind and per-session files of the state dir that name `sid` (`keys/<sid>`,
/// `unhandled/<sid>`, `topics/<sid>`, `seen/<sid>`, `acks/<sid>.<rid>`).
fn files_of(w: &World, sid: &str) -> Vec<String> {
    let mut out = Vec::new();
    for place in ["keys", "acks", "unhandled", "seen", "topics"] {
        let Ok(entries) = std::fs::read_dir(w.state.join(place)) else {
            continue;
        };
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            if name == sid
                || name
                    .strip_prefix(sid)
                    .is_some_and(|rest| rest.starts_with('.'))
            {
                out.push(format!("{place}/{name}"));
            }
        }
    }
    out
}

/// **THE FIRST START OF A BOOT REMOVES THE FILES OF THE SESSIONS THE BOOT
/// BEFORE ENDED, SO THE QUIT AFTER A LATER HANDOFF FLUSHES THIS BOOT'S ALONE**
/// (B20, `StateDir::sweep_sessions_of_an_earlier_boot`). A run puts eight
/// sessions to use (a key and a refill floor each) and quits; its record and
/// its lease are rewritten to name another boot ([`name_an_earlier_boot`]),
/// and the next start, a reboot's shape, finds every file of those sessions
/// gone. Eight new sessions are put to use, the instance hands off, and its
/// successor's quit flushes in place what this boot's sessions wrote, never
/// the earlier boot's sixteen. CONTROL: the earlier boot's files stood until
/// that start, and a start before it whose record alone named another boot,
/// its lease an attach of this boot (an older build's shape), removed none.
#[test]
fn a_reboots_first_start_removes_the_earlier_boots_sessions_files() {
    let mut w = World::boot("b20-reboot", &[]);
    w.wait_ready();
    let earlier = use_sessions(&w);
    let before = log_len(&w);
    quit(&mut w);
    assert_eq!(
        said(&w, before, "aterm-link: the run stopped cleanly:").len(),
        1,
        "a quit stops the run cleanly"
    );
    let stand = |w: &World, after: &str| {
        for sid in &earlier {
            assert!(
                files_of(w, sid).len() >= 2,
                "CONTROL: {sid}'s key and floor stand {after}: {:?}",
                files_of(w, sid)
            );
        }
    };
    stand(&w, "after the clean stop");

    name_an_earlier_boot_in(&w, false);
    let start = log_len(&w);
    let gui = w.gui.insert(World::relaunch_gui(&w));
    w.token = World::wait_for_owned_token_of(&w.ctl_sock, gui, &w.gui_log);
    w.wait_ready();
    quit(&mut w);
    assert_eq!(
        said(
            &w,
            start,
            "aterm-link: the last bridge ran in an earlier boot:"
        ),
        Vec::<String>::new(),
        "CONTROL: a lease of this boot is an attach of this boot, and its start sweeps nothing"
    );
    stand(&w, "past a start whose lease named this boot");
    name_an_earlier_boot(&w);

    let start = log_len(&w);
    let gui = w.gui.insert(World::relaunch_gui_shipping_flushes_with(
        &w,
        &[("ATERM_DEBUG_QUIT_BYE", "handoff")],
    ));
    w.token = World::wait_for_owned_token_of(&w.ctl_sock, gui, &w.gui_log);
    w.wait_ready();
    let swept = said(
        &w,
        start,
        "aterm-link: the last bridge ran in an earlier boot:",
    );
    eprintln!("MEASURED a reboot's first start, its sweep: {swept:?}");
    for sid in &earlier {
        assert_eq!(
            files_of(&w, sid),
            Vec::<String>::new(),
            "no file of {sid}, a session the earlier boot ended, is left"
        );
    }
    let [line] = swept.as_slice() else {
        panic!("one sweep line, not {swept:?}");
    };
    assert!(
        number(line, "files") >= 2 * SESSIONS as u64,
        "every earlier session's key and floor: {line}"
    );

    let now = use_sessions(&w);
    assert!(
        now.iter().all(|sid| !earlier.contains(sid)),
        "a start mints new sids"
    );
    let before = log_len(&w);
    quit(&mut w);
    assert_eq!(
        said(&w, before, "aterm-link: handed off:").len(),
        1,
        "the instance hands off"
    );
    let gui = w.gui.insert(World::relaunch_gui_shipping_flushes(&w));
    w.token = World::wait_for_owned_token_of(&w.ctl_sock, gui, &w.gui_log);
    w.wait_ready();
    let ours = now
        .iter()
        .flat_map(|sid| files_of(&w, sid))
        .filter(|rel| !rel.starts_with("seen/") && !rel.starts_with("topics/"))
        .count();
    let before = log_len(&w);
    quit(&mut w);
    let stopped = said(&w, before, "aterm-link: the run stopped cleanly:");
    eprintln!("MEASURED the quit after the handoff, past a reboot's sweep: {stopped:?}");
    let [line] = stopped.as_slice() else {
        panic!("one stop line, not {stopped:?}");
    };
    let files = number(line, "state files");
    assert!(
        files >= 2 * SESSIONS as u64,
        "every session's floor and key of this boot, inherited: {line}"
    );
    assert!(
        files < (ours + 2 * SESSIONS) as u64,
        "this boot's {ours} keys, receipts and floors and fewer of the node's own, never the \
         earlier boot's {}: {line}",
        2 * SESSIONS
    );
}
