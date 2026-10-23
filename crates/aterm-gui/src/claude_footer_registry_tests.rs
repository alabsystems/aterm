// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! REGISTRY WAKE (`docs/DESIGN-card-engine-2026-10-19.md` §3.10): the resolver reads an awaited
//! Claude Code process's registry row when the kernel says its Claude dir moved, never on a timer.

use std::io::Write as _;
use std::path::PathBuf;
use std::sync::mpsc::{Receiver, channel};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use super::{Asks, Command, FOLLOW_UPS, Job, LogWake, REGISTRY_READ, Resolver, WATCH_FOR};
use crate::claude_registry::{RegistryWatch, Sink, row_of};
use crate::session_timeline::SessionTimeline;

/// A hang detector, never a budget.
const MUST_HAPPEN: Duration = aterm_spec::wait::MUST_HAPPEN;

/// The session every test watches (a second one is `SESSION + 1`).
const SESSION: u64 = 7;

/// The conversation the rows name.
const SESSION_ID: &str = "00000000-0000-4000-8000-000000000a51";

thread_local! {
    static OWNED_ROOT: aterm_tempfile::TempDir = aterm_tempfile::Builder::new()
        .prefix("aterm-gui-registry-wake-")
        .tempdir()
        .expect("exclusive resolver scratch");
}

/// The scratch root of process `pid`. Each test's pid is above every
/// kernel's `pid_max`, so no live process has it: the read finds no start
/// time, and no test reads another's row.
fn root_of(pid: i32) -> PathBuf {
    OWNED_ROOT.with(|owned| {
        std::fs::canonicalize(owned.path())
            .expect("the owned temp dir")
            .join(pid.to_string())
    })
}

/// `pid`'s Claude dir.
fn claude_of(pid: i32) -> PathBuf {
    root_of(pid).join("claude")
}

/// `pid`'s registry row.
fn row_path(pid: i32) -> PathBuf {
    row_of(&claude_of(pid), u32::try_from(pid).expect("a positive pid"))
}

/// The exec read of `pid`: its Claude dir, by `CLAUDE_CONFIG_DIR`.
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

/// What a read asks outside Claude Code's files, answered here.
const ASKS: Asks = Asks {
    args: args_of,
    roster: no_roster,
    home: super::rig::scratch_home,
    stored_login: |_, _, _| true,
};

/// A fresh scratch root for `pid`: its working directory, and its Claude
/// dir's `sessions/` when `sessions`, else no Claude dir at all.
fn lay(pid: i32, sessions: bool) {
    let root = root_of(pid);
    std::fs::create_dir(&root).expect("fresh process fixture");
    std::fs::create_dir(root.join("work")).unwrap();
    if sessions {
        std::fs::create_dir_all(claude_of(pid).join("sessions")).unwrap();
    }
}

/// The registry row Claude Code writes for `pid`.
fn row_text(pid: i32) -> String {
    format!(
        r#"{{"pid":{pid},"sessionId":"{SESSION_ID}","cwd":{},"startedAt":1790607848684,"version":"2.1.287"}}"#,
        aterm_json::to_string(&root_of(pid).join("work").display().to_string()).unwrap()
    )
}

/// Write `pid`'s row whole by a rename into `sessions/`: one directory
/// event, the row complete when it lands.
fn rename_row_in(pid: i32) {
    let staged = root_of(pid).join("row.staged");
    std::fs::write(&staged, row_text(pid)).unwrap();
    std::fs::rename(&staged, row_path(pid)).unwrap();
}

/// A resolver whose registry watch opens with `open` and tells a channel the
/// test reads.
fn resolver(
    open: fn() -> std::io::Result<atpkg::dirwatch::Watcher>,
) -> (Resolver, Receiver<Command>) {
    let (tx, rx) = channel();
    let sink: Sink = Arc::new(move |told| tx.send(Command::from(told)).is_ok());
    (Resolver::new(RegistryWatch::new(open, sink), ASKS), rx)
}

/// A session timeline with `pid` in front.
fn timeline_of(pid: i32) -> Arc<Mutex<SessionTimeline>> {
    let timeline = Arc::new(Mutex::new(SessionTimeline::default()));
    timeline.lock().unwrap().note_foreground_group(pid);
    timeline
}

/// The status sweep's ask for `pid` in `session`.
fn ask(session: u64, timeline: &Arc<Mutex<SessionTimeline>>, pid: i32) -> Command {
    Command::Request(Job {
        session,
        timeline: Arc::clone(timeline),
        pgid: pid,
        wake: LogWake::default(),
    })
}

/// The next batch the forwarder told: its first command (a hang detector),
/// then whatever is queued behind it.
fn told(rx: &Receiver<Command>) -> Vec<Command> {
    let first = rx
        .recv_timeout(MUST_HAPPEN)
        .expect("the registry watch told nothing");
    std::iter::once(first)
        .chain(std::iter::from_fn(|| rx.try_recv().ok()))
        .collect()
}

/// The build the session's footer names.
fn version(timeline: &Arc<Mutex<SessionTimeline>>) -> Option<String> {
    timeline
        .lock()
        .unwrap()
        .claude_footer()
        .and_then(|facts| facts.version.clone())
}

/// Each `registry-read` dev row's `cause=… row=…`, in order.
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

/// A ROW WRITTEN AFTER THE ASK IS READ ON THE DIRECTORY EVENT. Claude Code
/// is named before it writes its row: the ask's read misses, the process is
/// awaited, and its `sessions/` is watched (and read once more, the watch
/// being new). The row lands; the forwarder tells the Claude dir; the
/// event's read publishes the build. The clock never moved, so no clock
/// read could have found it, and none is owed before the first follow-up.
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

/// A ROW CREATED EMPTY, THEN WRITTEN IN PLACE, IS READ ON THE FILE EVENT.
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

/// `sessions/` MADE AFTER THE ASK: THE WATCH MOVES INWARD, NO TIMER. With no
/// Claude dir yet (a first launch), the watch is on the nearest level there;
/// what a first launch writes beside it opens no row (nothing outside
/// `sessions/` is one); the Claude dir and its `sessions/` made in one burst
/// are both found by the one event, the watch moving in a level at a time
/// (each registered before the next is looked for) and reading once where it
/// lands, and the row written next is read on its event.
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

/// REGISTER FIRST, THEN READ. A row written after the ask's read and before
/// its watch is placed reports no event (the watch did not exist); the read
/// the new watch owes finds it.
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

/// THE WATCH DROPS WHEN NOTHING AWAITS. Two sessions await their rows under
/// two Claude dirs: a stop of one lets its watch go and keeps the other's;
/// the other's watch lapsing ([`WATCH_FOR`] with no ask) takes the last
/// watch, the watcher and the forwarder with it.
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

/// CONTROL — NO HIDDEN LADDER. With the registry watch refused, a row
/// written after the ask is read at the first follow-up and not before: no
/// clock read is owed, nor taken at any instant, until then. So every read
/// the tests above saw before it was pushed.
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
