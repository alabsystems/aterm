// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! REGISTRY WAKE (`docs/DESIGN-card-engine-2026-10-19.md` §3.10).

use std::io::Write as _;
use std::path::PathBuf;
use std::sync::mpsc::{Receiver, channel};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use super::{Asks, Command, FOLLOW_UPS, Job, LogWake, NO_TAP, REGISTRY_READ, Resolver, WATCH_FOR};
use crate::claude_registry::{RegistryWatch, Sink, row_of};
use crate::session_timeline::SessionTimeline;

const MUST_HAPPEN: Duration = aterm_spec::wait::MUST_HAPPEN;

const SESSION: u64 = 7;

const SESSION_ID: &str = "00000000-0000-4000-8000-000000000a51";

thread_local! {
    static OWNED_ROOT: aterm_tempfile::TempDir = aterm_tempfile::Builder::new()
        .prefix("aterm-gui-registry-wake-")
        .tempdir()
        .expect("exclusive resolver scratch");
}

fn root_of(pid: i32) -> PathBuf {
    OWNED_ROOT.with(|owned| {
        std::fs::canonicalize(owned.path())
            .expect("the owned temp dir")
            .join(pid.to_string())
    })
}

fn claude_of(pid: i32) -> PathBuf {
    root_of(pid).join("claude")
}

fn row_path(pid: i32) -> PathBuf {
    row_of(&claude_of(pid), u32::try_from(pid).expect("a positive pid"))
}

fn args_of(pid: i32) -> Option<atpkg::caller_shell::ProcArgs> {
    Some(atpkg::caller_shell::ProcArgs {
        argv: vec!["claude".into()],
        env: vec![
            format!("CLAUDE_CONFIG_DIR={}", claude_of(pid).display()),
            "HOME=/home/someone".into(),
        ],
        ..Default::default()
    })
}

fn no_roster() -> Option<aterm_agent::harness::accounts::Roster> {
    None
}

const ASKS: Asks = Asks {
    files: |_| None,
    tap: NO_TAP,
    args: args_of,
    roster: no_roster,
    home: super::rig::scratch_home,
    route: |_, _, _| aterm_agent::harness::accounts::AuthRoute::Stored,
};

fn lay(pid: i32, sessions: bool) {
    let root = root_of(pid);
    std::fs::create_dir(&root).expect("fresh process fixture");
    std::fs::create_dir(root.join("work")).unwrap();
    if sessions {
        std::fs::create_dir_all(claude_of(pid).join("sessions")).unwrap();
    }
}

fn row_text(pid: i32) -> String {
    format!(
        r#"{{"pid":{pid},"sessionId":"{SESSION_ID}","cwd":{},"startedAt":1790607848684,"version":"2.1.287"}}"#,
        aterm_json::to_string(&root_of(pid).join("work").display().to_string()).unwrap()
    )
}

fn rename_row_in(pid: i32) {
    let staged = root_of(pid).join("row.staged");
    std::fs::write(&staged, row_text(pid)).unwrap();
    std::fs::rename(&staged, row_path(pid)).unwrap();
}

fn resolver(
    open: fn() -> std::io::Result<atpkg::dirwatch::Watcher>,
) -> (Resolver, Receiver<Command>) {
    let (tx, rx) = channel();
    let sink: Sink = Arc::new(move |told| tx.send(Command::from(told)).is_ok());
    (Resolver::new(RegistryWatch::new(open, sink), ASKS), rx)
}

fn timeline_of(pid: i32) -> Arc<Mutex<SessionTimeline>> {
    let timeline = Arc::new(Mutex::new(SessionTimeline::default()));
    timeline.lock().unwrap().note_foreground_group(pid);
    timeline
}

fn ask(session: u64, timeline: &Arc<Mutex<SessionTimeline>>, pid: i32) -> Command {
    Command::Request(Job {
        session,
        timeline: Arc::clone(timeline),
        pgid: pid,
        wake: LogWake::default(),
    })
}

fn told(rx: &Receiver<Command>) -> Vec<Command> {
    let first = rx
        .recv_timeout(MUST_HAPPEN)
        .expect("the registry watch told nothing");
    std::iter::once(first)
        .chain(std::iter::from_fn(|| rx.try_recv().ok()))
        .collect()
}

fn version(timeline: &Arc<Mutex<SessionTimeline>>) -> Option<String> {
    timeline
        .lock()
        .unwrap()
        .claude_footer()
        .and_then(|facts| facts.version.clone())
}

fn reads(timeline: &Arc<Mutex<SessionTimeline>>) -> Vec<String> {
    timeline
        .lock()
        .unwrap()
        .since(None)
        .filter(|event| event.kind == REGISTRY_READ)
        .map(|event| {
            event
                .payload
                .split_whitespace()
                .filter(|word| word.starts_with("cause=") || word.starts_with("row="))
                .collect::<Vec<_>>()
                .join(" ")
        })
        .collect()
}

#[test]
fn a_row_created_after_the_ask_is_read_on_the_directory_event() {
    const PID: i32 = 2_000_000_101;
    lay(PID, true);
    let t0 = Instant::now();
    let (mut resolver, rx) = resolver(atpkg::dirwatch::Watcher::new);
    let timeline = timeline_of(PID);
    resolver.turn([ask(SESSION, &timeline, PID)], || t0);
    assert_eq!(version(&timeline), None, "no row yet");
    assert!(resolver.registry.is_live(), "the process is awaited");
    assert_eq!(
        resolver.registry.level_of(&claude_of(PID)),
        Some(claude_of(PID).join("sessions"))
    );
    rename_row_in(PID);
    resolver.turn(told(&rx), || t0);
    assert_eq!(version(&timeline).as_deref(), Some("2.1.287"));
    assert_eq!(
        reads(&timeline),
        ["cause=ask row=0", "cause=placed row=0", "cause=event row=1"]
    );
    assert!(!resolver.registry.is_live(), "found: no watch, no thread");
    assert_eq!(
        resolver.next(t0),
        Some(t0 + FOLLOW_UPS[0]),
        "no clock read before the first follow-up"
    );
    let _ = std::fs::remove_dir_all(root_of(PID));
}

#[test]
fn a_row_created_empty_then_written_is_read_on_the_file_event() {
    const PID: i32 = 2_000_000_102;
    lay(PID, true);
    let t0 = Instant::now();
    let (mut resolver, rx) = resolver(atpkg::dirwatch::Watcher::new);
    let timeline = timeline_of(PID);
    resolver.turn([ask(SESSION, &timeline, PID)], || t0);
    std::fs::File::create(row_path(PID)).unwrap();
    resolver.turn(told(&rx), || t0);
    assert_eq!(version(&timeline), None, "an empty row says nothing");
    assert!(resolver.registry.file_watched(&row_path(PID)));
    while rx.try_recv().is_ok() {}
    std::fs::OpenOptions::new()
        .write(true)
        .open(row_path(PID))
        .unwrap()
        .write_all(row_text(PID).as_bytes())
        .unwrap();
    resolver.turn(told(&rx), || t0);
    assert_eq!(version(&timeline).as_deref(), Some("2.1.287"));
    assert_eq!(
        reads(&timeline),
        [
            "cause=ask row=0",
            "cause=placed row=0",
            "cause=event row=0",
            "cause=placed row=0",
            "cause=event row=1"
        ]
    );
    assert!(!resolver.registry.is_live());
    let _ = std::fs::remove_dir_all(root_of(PID));
}

#[test]
fn sessions_made_after_the_ask_moves_the_watch_inward() {
    const PID: i32 = 2_000_000_103;
    lay(PID, false);
    let t0 = Instant::now();
    let (mut resolver, rx) = resolver(atpkg::dirwatch::Watcher::new);
    let timeline = timeline_of(PID);
    resolver.turn([ask(SESSION, &timeline, PID)], || t0);
    assert_eq!(
        resolver.registry.level_of(&claude_of(PID)),
        Some(root_of(PID)),
        "the nearest level there"
    );
    std::fs::write(root_of(PID).join(".claude.json"), b"{}").unwrap();
    resolver.turn(told(&rx), || t0);
    assert_eq!(
        reads(&timeline),
        ["cause=ask row=0", "cause=placed row=0"],
        "a write outside sessions/ opens no row"
    );
    std::fs::create_dir_all(claude_of(PID).join("sessions")).unwrap();
    while resolver.registry.level_of(&claude_of(PID)) != Some(claude_of(PID).join("sessions")) {
        resolver.turn(told(&rx), || t0);
    }
    while rx.try_recv().is_ok() {}
    rename_row_in(PID);
    resolver.turn(told(&rx), || t0);
    assert_eq!(version(&timeline).as_deref(), Some("2.1.287"));
    assert_eq!(
        reads(&timeline),
        [
            "cause=ask row=0",
            "cause=placed row=0",
            "cause=placed row=0",
            "cause=event row=1"
        ]
    );
    let _ = std::fs::remove_dir_all(root_of(PID));
}

#[test]
fn a_row_written_before_its_watch_is_read_when_the_watch_is_placed() {
    const PID: i32 = 2_000_000_104;
    lay(PID, true);
    let t0 = Instant::now();
    let mut clock = || t0;
    let (mut resolver, rx) = resolver(atpkg::dirwatch::Watcher::new);
    let timeline = timeline_of(PID);
    resolver.serve_commands([ask(SESSION, &timeline, PID)], &mut clock);
    rename_row_in(PID);
    resolver.settle(&mut clock);
    assert_eq!(version(&timeline).as_deref(), Some("2.1.287"));
    assert_eq!(reads(&timeline), ["cause=ask row=0", "cause=placed row=1"]);
    assert!(!resolver.registry.is_live());
    assert!(
        rx.try_recv().is_err(),
        "nothing to tell: the write came first"
    );
    let _ = std::fs::remove_dir_all(root_of(PID));
}

#[test]
fn the_watch_drops_when_nothing_awaits() {
    const PID: i32 = 2_000_000_105;
    const OTHER: i32 = 2_000_000_106;
    lay(PID, true);
    lay(OTHER, true);
    let t0 = Instant::now();
    let (mut resolver, _rx) = resolver(atpkg::dirwatch::Watcher::new);
    let (one, two) = (timeline_of(PID), timeline_of(OTHER));
    resolver.turn(
        [ask(SESSION, &one, PID), ask(SESSION + 1, &two, OTHER)],
        || t0,
    );
    assert!(resolver.registry.level_of(&claude_of(PID)).is_some());
    assert!(resolver.registry.level_of(&claude_of(OTHER)).is_some());
    resolver.turn(
        [Command::Stop {
            session: SESSION,
            pgid: None,
            timeline: None,
        }],
        || t0,
    );
    assert_eq!(resolver.registry.level_of(&claude_of(PID)), None);
    assert!(resolver.registry.level_of(&claude_of(OTHER)).is_some());
    assert!(resolver.registry.is_live(), "the other still awaits");
    resolver.retain(t0 + WATCH_FOR);
    assert!(
        !resolver.registry.is_live(),
        "nothing awaits: no watch, no thread"
    );
    for pid in [PID, OTHER] {
        let _ = std::fs::remove_dir_all(root_of(pid));
    }
}

#[test]
fn with_the_watch_refused_the_row_waits_for_the_first_follow_up() {
    const PID: i32 = 2_000_000_107;
    lay(PID, true);
    let t0 = Instant::now();
    let (mut resolver, rx) =
        resolver(|| Err(std::io::Error::from(std::io::ErrorKind::Unsupported)));
    let timeline = timeline_of(PID);
    resolver.turn([ask(SESSION, &timeline, PID)], || t0);
    assert!(!resolver.registry.is_live());
    rename_row_in(PID);
    assert_eq!(resolver.next(t0), Some(t0 + FOLLOW_UPS[0]));
    let before = t0 + FOLLOW_UPS[0] - Duration::from_millis(1);
    resolver.turn(std::iter::empty(), || before);
    assert_eq!(version(&timeline), None, "not read before the follow-up");
    assert!(rx.try_recv().is_err(), "nothing told: no watch");
    resolver.turn(std::iter::empty(), || t0 + FOLLOW_UPS[0]);
    assert_eq!(version(&timeline).as_deref(), Some("2.1.287"));
    assert_eq!(
        reads(&timeline),
        ["cause=ask row=0", "cause=follow-up row=1"]
    );
    let _ = std::fs::remove_dir_all(root_of(PID));
}

/// A LIVE AGENT NOBODY ASKS FOR IS READ ON THE IDLE CLOCK FOR AS LONG AS IT LIVES (S2): one ask,
/// then no command for an hour, on the resolver thread's own steps ([`Resolver::retain`], then
/// [`Resolver::next`], then [`Resolver::turn`] at the instant `next` named, as `run` takes them)
/// with the clock scripted. A session whose process lives (this test's own) is read at the ask,
/// at its two follow-ups, every 15 s until the ask is ten minutes old (39 reads) and every 60 s
/// from then to the hour (50 reads), each for the idle clock; one whose process is gone is read
/// the same until ten minutes and never after, its watch dropped. No row answers either process,
/// so every read writes its dev row and the counts are those rows. NEGATIVE CONTROL: with the
/// prune's `walled.contains(&session)` term restored the live session, whose footer shows no
/// wall, is read 0 times past ten minutes.
#[test]
fn a_live_session_nobody_asks_for_is_read_each_minute_and_a_dead_one_is_dropped() {
    const GONE: i32 = 2_000_000_108;
    let live = i32::try_from(std::process::id()).expect("a pid");
    let hour = Duration::from_secs(3_600);
    let idle_reads = |pid: i32| {
        lay(pid, true);
        let t0 = Instant::now();
        let (mut resolver, _rx) =
            resolver(|| Err(std::io::Error::from(std::io::ErrorKind::Unsupported)));
        let timeline = timeline_of(pid);
        resolver.turn([ask(SESSION, &timeline, pid)], || t0);
        let mut now = t0;
        let mut turns = 0;
        loop {
            resolver.retain(now);
            let Some(at) = resolver.next(now).filter(|at| *at <= t0 + hour) else {
                break;
            };
            assert!(at > now, "the clock a turn left owed is still to come");
            now = at;
            resolver.turn(std::iter::empty(), || now);
            turns += 1;
        }
        let watched = resolver.schedule.watched.contains_key(&SESSION);
        let _ = std::fs::remove_dir_all(root_of(pid));
        (reads(&timeline), turns, watched)
    };
    let near: Vec<&str> = ["cause=ask row=0"; 1]
        .into_iter()
        .chain(["cause=follow-up row=0"; 2])
        .chain(["cause=idle row=0"; 39])
        .collect();

    let (read, turns, watched) = idle_reads(live);
    assert_eq!(
        read[..near.len()],
        near[..],
        "until the ask is ten minutes old"
    );
    assert_eq!(
        read[near.len()..],
        ["cause=idle row=0"; 50],
        "then once a minute to the hour, while the process lives"
    );
    // One turn at the 15 s clock's last instant finds the 60 s clock not yet due and reads nothing.
    assert_eq!(turns, 2 + 39 + 1 + 50);
    assert!(watched, "a live process's watch stands");

    let (read, turns, watched) = idle_reads(GONE);
    assert_eq!(
        read, near,
        "a process that is gone: nothing past ten minutes"
    );
    assert_eq!(turns, 2 + 39 + 1);
    assert!(!watched, "and its watch is dropped");
}

/// AN ASK REACHES A RESOLVER THAT RUNS ([`super::Lane::deliver`], what
/// [`super::request_footer`] hands its job to): the first ask starts one resolver and the second
/// goes to it; once that resolver has ended as its thread ends, its receiver dropped and then
/// its [`super::Ended`], the next ask starts another, which takes that ask, and is told the new
/// epoch; an ask whose resolver cannot start is told nothing, and the one after is delivered.
/// Counts are the resolvers started and the requests each received.
/// NEGATIVE CONTROL: with one send for each ask (no second resolver started for an ask the
/// first send could not hand over), the ask after the resolver ended is told `None`.
#[test]
fn an_ask_after_the_resolver_ended_starts_another_which_takes_it() {
    use std::cell::{Cell, RefCell};
    let lane = super::Lane::new();
    let timeline = Arc::new(Mutex::new(SessionTimeline::for_test()));
    let job = || Job {
        session: SESSION,
        timeline: Arc::clone(&timeline),
        pgid: 42,
        wake: LogWake::default(),
    };
    // The resolvers started, each as its thread would hold it, and whether a start succeeds.
    let started = RefCell::new(Vec::new());
    let starts = Cell::new(true);
    let start = |rx, tx, ended| {
        if !starts.get() {
            return Err(std::io::Error::other("no thread"));
        }
        started.borrow_mut().push((rx, tx, ended));
        Ok(())
    };
    let requests = |nth: usize| {
        let started = started.borrow();
        let (rx, _, _): &(Receiver<Command>, _, _) = &started[nth];
        rx.try_iter()
            .filter(|command| matches!(command, Command::Request(_)))
            .count()
    };
    // What a resolver's thread does at its end: its receiver goes, then its `Ended`.
    let end = |nth: usize| {
        let (rx, tx, ended) = started.borrow_mut().remove(nth);
        drop(rx);
        drop(tx);
        drop(ended);
    };

    assert_eq!(lane.deliver(job(), start), Some(0), "the first ask");
    assert_eq!(lane.deliver(job(), start), Some(0), "the second");
    assert_eq!((started.borrow().len(), requests(0)), (1, 2));
    end(0);
    assert_eq!(lane.epoch(), 1, "the resolver ended");
    assert_eq!(
        lane.deliver(job(), start),
        Some(1),
        "the ask after it is taken by a resolver started for it"
    );
    assert_eq!((started.borrow().len(), requests(0)), (1, 1));
    end(0);
    starts.set(false);
    assert_eq!(lane.deliver(job(), start), None, "no resolver could start");
    assert_eq!(started.borrow().len(), 0);
    starts.set(true);
    let epoch = lane.epoch();
    assert_eq!(epoch, 3, "one more ended, and one that could not start");
    assert_eq!(lane.deliver(job(), start), Some(epoch));
    assert_eq!((started.borrow().len(), requests(0)), (1, 1));
}
