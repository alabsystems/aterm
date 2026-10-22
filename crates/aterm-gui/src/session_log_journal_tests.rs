// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The session journal's tests (phase 2B). The writer is driven pass by pass
//! with an injected clock and an in-memory directory with faults
//! ([`MemFs`]); the real directory's rules (pinned, exclusive, private, the
//! lock, retention among hostile entries) run against a scratch directory.
//! Every wait for something that must happen is a one-minute hang detector.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use aterm_slog::journal::{EndWhy, Frame, Inst, LossWhy, View, complete_lines, decode_line};
use aterm_slog::{
    Actor, Close, Conf, Cursor, Fields, GapWhy, Kind, Limits, Log, Mark, Node, Open, Out, RunOut,
    ShortId, Src, Stamp,
};

use super::fs::mem::{Fault, MemFs};
use super::fs::{JournalFs, Meta, list};
use super::read::{Budgets, Index, JournalRead, view_with};
use super::*;
use crate::session_timeline::SessionTimeline;

const HANG: Duration = Duration::from_secs(60);
const WALL0: u64 = 1_760_600_000_000;

/// The writer's clock, moved by the test.
#[derive(Default)]
pub(crate) struct TestClock {
    mono_ms: AtomicU64,
}

impl TestClock {
    fn advance(&self, d: Duration) {
        self.mono_ms
            .fetch_add(u64::try_from(d.as_millis()).unwrap_or(0), Ordering::Relaxed);
    }
}

impl Clock for TestClock {
    fn mono(&self) -> Duration {
        Duration::from_millis(self.mono_ms.load(Ordering::Relaxed))
    }

    fn wall_ms(&self) -> u64 {
        WALL0 + self.mono_ms.load(Ordering::Relaxed)
    }
}

type Timeline = Arc<Mutex<SessionTimeline>>;

/// The sessions a test's "store" holds, and whether its lock is busy.
#[derive(Clone, Default)]
pub(crate) struct Sessions {
    list: Arc<Mutex<Vec<(ShortId, Timeline)>>>,
    busy: Arc<AtomicBool>,
}

impl Targets for Sessions {
    fn try_targets(&self) -> Option<TargetSnapshot> {
        if self.busy.load(Ordering::Relaxed) {
            return None;
        }
        Some(TargetSnapshot {
            live: self.list.lock().unwrap_or_else(|p| p.into_inner()).clone(),
            ..TargetSnapshot::default()
        })
    }
}

impl Sessions {
    fn add(&self, sid: &str) -> Timeline {
        let tl: Timeline = Arc::new(Mutex::new(SessionTimeline::for_test()));
        let sid = ShortId::new(sid).expect("a sid");
        with_log(&tl, |log| {
            log.open_session(
                Kind::Spawn,
                Src::Aterm,
                Conf::Exact,
                Actor::Nobody,
                Fields::new(),
                at(0),
            )
            .expect("opens");
        });
        self.list
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .push((sid, Arc::clone(&tl)));
        tl
    }

    fn remove(&self, sid: &str) {
        self.list
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .retain(|(s, _)| s.as_str() != sid);
    }
}

fn at(ms: u64) -> Stamp {
    Stamp::new(WALL0 + ms, ms)
}

fn with_log<R>(tl: &Timeline, f: impl FnOnce(&mut Log) -> R) -> R {
    let mut g = tl.lock().unwrap_or_else(|p| p.into_inner());
    f(g.slog_mut().log_mut_for_journal())
}

fn mark(tl: &Timeline, ms: u64) -> u64 {
    with_log(tl, |log| {
        log.mark(Mark::new(Kind::Human, Src::Input, Conf::Exact), at(ms))
            .expect("a mark")
    })
}

fn cmd(tl: &Timeline, ms: u64) -> Node {
    with_log(tl, |log| {
        log.open(Open::new(Kind::Cmd, Src::Shell, Conf::Strong), at(ms))
            .expect("a run")
    })
}

fn close_session(tl: &Timeline, ms: u64) {
    with_log(tl, |log| {
        log.close_session(
            Close::new(
                Out::Session(aterm_slog::SessionOut::CtlClose),
                Src::Aterm,
                Conf::Exact,
            ),
            at(ms),
        )
        .expect("closes");
    });
}

fn inst(s: &str) -> Inst {
    Inst::new(s).expect("an instance")
}

struct Rig {
    fs: MemFs,
    sessions: Sessions,
    clock: Arc<TestClock>,
    report: Arc<Report>,
}

impl Rig {
    fn new() -> Rig {
        let fs = MemFs::default();
        fs.state().free = Some(u64::MAX);
        Rig {
            fs,
            sessions: Sessions::default(),
            clock: Arc::new(TestClock::default()),
            report: Arc::new(Report::default()),
        }
    }

    fn writer_as(&self, name: &str, limits: WriterLimits) -> Writer<Sessions> {
        let mut w = Writer::new(
            Arc::new(self.fs.clone()),
            self.sessions.clone(),
            inst(name),
            Arc::clone(&self.clock) as Arc<dyn Clock>,
            limits,
            Arc::clone(&self.report),
        );
        w.start();
        w
    }

    fn writer(&self) -> Writer<Sessions> {
        self.writer_as("p1-a1", WriterLimits::default())
    }

    /// Every complete line's frame, every segment in name order.
    fn frames(&self) -> Vec<Frame> {
        let fs = self.fs.clone();
        let mut names = fs.names();
        names.sort();
        names
            .iter()
            .filter(|n| n.ends_with(".slog"))
            .flat_map(|n| {
                complete_lines(&fs.bytes(n))
                    .filter_map(|l| decode_line(l).ok())
                    .collect::<Vec<_>>()
            })
            .collect()
    }

    fn view(&self, sid: &str) -> View {
        read_view(&self.fs, sid, Budgets::READER)
    }
}

/// A read that must find the session.
fn read_view(fs: &dyn JournalFs, sid: &str, b: Budgets) -> View {
    match view_with(
        fs,
        &Mutex::new(Index::default()),
        &ShortId::new(sid).expect("a sid"),
        b,
    ) {
        JournalRead::View(v) => *v,
        other => panic!("the journal holds {sid}: {other:?}"),
    }
}

/// Whether a view's records say `why=<why>` by a gap record.
fn says(v: &View, why: &str) -> bool {
    all(&v.log)
        .iter()
        .any(|r| r.op == aterm_slog::Op::Gap && r.f.get_word(aterm_slog::Key::Why) == Some(why))
}

fn recs_of(frames: &[Frame], sid: &str) -> Vec<aterm_slog::Rec> {
    frames
        .iter()
        .filter_map(|f| match f {
            Frame::Rec {
                sid: s,
                rec,
                restated: false,
            } if s.as_str() == sid => Some(rec.clone()),
            _ => None,
        })
        .collect()
}

fn all(log: &Log) -> Vec<aterm_slog::Rec> {
    log.read(Cursor::new(log.ep(), 0), usize::MAX)
        .recs
        .into_iter()
        .cloned()
        .collect()
}

fn host(r: &aterm_slog::Rec) -> aterm_slog::Rec {
    aterm_slog::decode(&aterm_slog::encode(r, aterm_slog::Tier::Host).expect("fits"))
        .expect("decodes")
}

// ---- the gate and the default ------------------------------------------

/// D3 ON: with no `[session_log]` table the
/// journal is asked for, and the gate opens on a window's desk and the lab's
/// seam, on macOS and Linux. It stays shut for an explicit `journal =
/// false`, a plain headless instance, `restore_session = false` and a
/// platform with no writer.
///
/// NEGATIVE CONTROLS: the old default (`unwrap_or(false)`) fails the
/// default's arms; a gate without its restore leg fails the
/// `restore_session = false` arm.
#[test]
fn the_gate_is_on_by_default_and_follows_the_desk() {
    use crate::app_config::{Config, SessionLogConfig};
    use crate::desk::Desk;
    let default = Config::default();
    let off = Config {
        session_log: Some(SessionLogConfig {
            journal: Some(false),
            clause: None,
        }),
        ..Config::default()
    };
    let no_restore = Config {
        restore_session: Some(false),
        ..Config::default()
    };
    assert!(default.session_log_journal(), "asked for by default");
    assert!(!off.session_log_journal(), "`journal = false` opts out");
    for desk in [Desk::Window, Desk::LabHeadless, Desk::Headless] {
        assert!(!gate(&off, desk), "`journal = false` ({desk:?})");
        assert!(
            !gate(&no_restore, desk),
            "follows restore_session ({desk:?})"
        );
    }
    for (requested, restore, desk, supported, expected) in [
        (false, true, Desk::Window, true, Some(Inactive::Disabled)),
        (true, true, Desk::Window, false, Some(Inactive::Unsupported)),
        (true, false, Desk::Window, true, Some(Inactive::RestoreOff)),
        (true, true, Desk::Headless, true, Some(Inactive::NoDesk)),
        (true, true, Desk::Window, true, None),
        (true, true, Desk::LabHeadless, true, None),
    ] {
        assert_eq!(eligibility(requested, restore, desk, supported), expected);
    }
    assert_eq!(gate(&default, Desk::Window), cfg!(unix), "on by default");
    assert_eq!(
        gate(&default, Desk::LabHeadless),
        cfg!(unix),
        "the lab's seam"
    );
    assert!(!gate(&default, Desk::Headless), "headless keeps no desk");
}

/// with `journal = false` (the opt-out since D3 is on) the lane
/// is inert on every path: no thread, no wake in any log, no read, no file;
/// and the reader's open creates nothing. NEGATIVE CONTROL: the writer's
/// open makes the directory, 0700.
#[cfg(unix)]
#[test]
fn journal_false_creates_nothing() {
    use std::os::unix::fs::PermissionsExt as _;
    let scratch_guard = tempdir("journal-false");
    let scratch = scratch_guard.path();
    let logs = scratch.join("logs");
    std::fs::create_dir_all(&logs).expect("logs");
    let off = crate::app_config::Config {
        session_log: Some(crate::app_config::SessionLogConfig {
            journal: Some(false),
            clause: None,
        }),
        ..crate::app_config::Config::default()
    };
    // The launch's own expression (`lib.rs`): the gate decides the folder.
    let mut lane = Lane::new(
        gate(&off, crate::desk::Desk::Window).then(|| logs.clone()),
        false,
    );
    lane.start(crate::session_store::new_store());
    lane.finish_at_quit(QUIT_BUDGET);
    assert!(lane.attention().is_none());
    assert!(SessionTimeline::default().slog().log().notify().is_none() || LANE.get().is_some());
    assert!(
        super::fs::PinnedJournalFs::open(&logs, false)
            .expect("an absent journal is no error")
            .is_none()
    );
    // Refusing the actual spawn path must not leave a phantom armed writer or
    // retain the store in a closure that never ran.
    let failed_shared = Shared::new();
    let store = crate::session_store::new_store();
    let mut failed = Lane {
        shared: Some(Arc::clone(&failed_shared)),
        logs: Some(logs.clone()),
        armed: false,
        started: false,
    };
    failed.start_with(Arc::clone(&store), |_writer| {
        Err(io::Error::other("injected spawn failure"))
    });
    assert_eq!(
        failed.status(),
        Status {
            phase: Some(Phase::Stopped),
            attention: Attention::Stopped(Cause::Spawn)
        }
    );
    assert_eq!(
        Arc::strong_count(&store),
        1,
        "failed spawn releases captured retirement/store state"
    );
    failed.start_with(Arc::clone(&store), |_| {
        panic!("one launch must not retry its failed spawn")
    });
    assert_eq!(
        lane.status().phase,
        None,
        "inert differs from a failed writer"
    );
    // An armed lane's thread (an update candidate) finished before its Commit.
    let shared = Shared::new();
    let thread = {
        let shared = Arc::clone(&shared);
        let logs = logs.clone();
        std::thread::spawn(move || {
            run(
                &shared,
                &logs,
                crate::session_store::new_store(),
                inst("p9-ff"),
                true,
            );
        })
    };
    let (tx, rx) = std::sync::mpsc::sync_channel(1);
    *shared.ack.lock().unwrap_or_else(|p| p.into_inner()) = Some(tx);
    let deadline = std::time::Instant::now() + HANG;
    while shared.wake.thread.get().is_none() {
        assert!(std::time::Instant::now() < deadline, "the thread never ran");
        std::thread::yield_now();
    }
    shared.request(REQ_FINISH);
    rx.recv_timeout(HANG)
        .expect("an armed lane answers its finish");
    thread.join().expect("the writer thread ends");
    let entries: Vec<_> = std::fs::read_dir(&logs)
        .expect("logs")
        .flatten()
        .map(|e| e.file_name())
        .collect();
    assert!(entries.is_empty(), "nothing created: {entries:?}");
    // NEGATIVE CONTROL: the writer's own open makes it.
    let _dir = super::fs::PinnedJournalFs::open(&logs, true)
        .expect("opens")
        .expect("made");
    let md = std::fs::metadata(logs.join("session-log")).expect("made");
    assert_eq!(md.permissions().mode() & 0o777, 0o700);
}

pub(crate) fn tempdir(stem: &str) -> aterm_tempfile::TempDir {
    let base = std::fs::canonicalize(std::env::temp_dir()).expect("tmp");
    aterm_tempfile::Builder::new()
        .prefix(&format!("aterm-slogj-{stem}-"))
        .tempdir_in(base)
        .expect("exclusive scratch")
}

// ---- the writer -------------------------------------------------------

#[test]
fn one_pass_journals_the_ring_at_the_host_tier() {
    let rig = Rig::new();
    let tl = rig.sessions.add("s-one");
    let r = cmd(&tl, 1);
    mark(&tl, 2);
    with_log(&tl, |log| {
        log.close(
            r,
            Close::new(Out::Run(RunOut::Fail), Src::Shell, Conf::Exact),
            at(3),
        )
        .expect("closes");
    });
    let mut w = rig.writer();
    assert!(w.pass() > 0);
    let frames = rig.frames();
    assert!(matches!(frames.first(), Some(Frame::Seg { n: 1, .. })));
    let live: Vec<_> = all(tl.lock().expect("tl").slog().log())
        .iter()
        .map(host)
        .collect();
    assert_eq!(
        recs_of(&frames, "s-one"),
        live,
        "exactly the ring's records"
    );
    // A second pass with nothing new writes nothing.
    assert_eq!(w.pass(), 0);
    // The lock was taken before the first segment.
    let names = rig.fs.names();
    assert!(names.contains(&"p1-a1.lock".to_string()), "{names:?}");
}

#[test]
fn every_segment_restates_every_session() {
    let rig = Rig::new();
    let a = rig.sessions.add("s-a");
    let b = rig.sessions.add("s-b");
    let run = cmd(&b, 1);
    let limits = WriterLimits {
        segment_bytes: 600,
        ..WriterLimits::default()
    };
    let mut w = rig.writer_as("p1-a1", limits);
    for i in 0..40 {
        mark(&a, 10 + i);
        w.pass();
    }
    let segs: Vec<String> = rig
        .fs
        .names()
        .into_iter()
        .filter(|n| n.ends_with(".slog"))
        .collect();
    assert!(segs.len() > 3, "rotated: {segs:?}");
    for name in &segs {
        // A snap restates the open path AS OF ITS HEAD: the first segment's,
        // at cursor 0, restates nothing (the opens follow as records).
        let first = name == "p1-a1.1.slog";
        let frames: Vec<Frame> = complete_lines(&rig.fs.bytes(name))
            .filter_map(|l| decode_line(l).ok())
            .collect();
        for sid in ["s-a", "s-b"] {
            assert!(
                frames
                    .iter()
                    .any(|f| matches!(f, Frame::Snap { sid: s, .. } if s.as_str() == sid)),
                "{name} restates {sid}"
            );
        }
        let restated: Vec<Node> = frames
            .iter()
            .filter_map(|f| match f {
                Frame::Rec {
                    sid,
                    rec,
                    restated: true,
                } if sid.as_str() == "s-b" => Some(rec.node),
                _ => None,
            })
            .collect();
        if first {
            assert!(restated.is_empty(), "{name}: {restated:?}");
            continue;
        }
        assert_eq!(restated, vec![Node::SESSION, run], "{name}: b's open path");
    }
}

#[test]
fn a_busy_lock_is_skipped_counted_and_written_next_pass() {
    let rig = Rig::new();
    let tl = rig.sessions.add("s-busy");
    let mut w = rig.writer();
    w.pass();
    mark(&tl, 1);
    let held = tl.lock().expect("held");
    w.pass();
    assert!(w.owed, "a skipped session is owed");
    assert_eq!(rig.report.busy_skips.load(Ordering::Relaxed), 1);
    drop(held);
    assert!(w.pass() > 0, "written the pass after");
    assert_eq!(recs_of(&rig.frames(), "s-busy").len(), 2);
    // A busy registry: the sessions already followed are written, a new one
    // waits for the pass that reads the registry.
    rig.sessions.busy.store(true, Ordering::Relaxed);
    let _new = rig.sessions.add("s-new");
    mark(&tl, 2);
    assert!(w.pass() > 0);
    assert!(w.owed, "the registry is owed");
    assert!(recs_of(&rig.frames(), "s-new").is_empty());
    rig.sessions.busy.store(false, Ordering::Relaxed);
    assert!(w.pass() > 0);
    assert_eq!(recs_of(&rig.frames(), "s-new").len(), 1);
}

#[test]
fn records_evicted_between_passes_read_as_one_counted_gap() {
    let rig = Rig::new();
    let tl = rig.sessions.add("s-ev");
    with_log(&tl, |log| {
        *log = Log::with_limits(1, Limits::new(4, 1 << 20).expect("a ring"));
        log.open_session(
            Kind::Spawn,
            Src::Aterm,
            Conf::Exact,
            Actor::Nobody,
            Fields::new(),
            at(0),
        )
        .expect("opens");
    });
    let mut w = rig.writer();
    w.pass();
    for i in 0..10 {
        mark(&tl, 1 + i);
    }
    w.pass();
    let gaps: Vec<(LossWhy, u64)> = rig
        .frames()
        .iter()
        .filter_map(|f| match f {
            Frame::Gap { why, lost, .. } => Some((*why, *lost)),
            _ => None,
        })
        .collect();
    assert_eq!(gaps.len(), 1, "{gaps:?}");
    assert_eq!(gaps[0].0, LossWhy::Evicted);
    let live = tl.lock().expect("tl").slog().log().len() as u64;
    assert_eq!(gaps[0].1, 10 - (live - 1), "the ring's own count");
    let v = rig.view("s-ev");
    let read = v.log.read(Cursor::new(v.log.ep(), 0), usize::MAX);
    assert_eq!(read.gap.map(|g| g.lost), Some(gaps[0].1));
}

/// a session that closes between passes, forty of them at once,
/// still gets its close journaled through the timeline the writer kept.
#[test]
fn sessions_closed_between_passes_still_get_their_closes() {
    let rig = Rig::new();
    let tls: Vec<(String, Timeline)> = (0..40)
        .map(|i| {
            let sid = format!("s-c{i}");
            let tl = rig.sessions.add(&sid);
            (sid, tl)
        })
        .collect();
    let limits = WriterLimits {
        pass_bytes: 64,
        ..WriterLimits::default()
    };
    let mut w = rig.writer_as("p1-a1", limits);
    w.pass();
    for (sid, tl) in &tls {
        mark(tl, 5);
        close_session(tl, 6);
        rig.sessions.remove(sid);
    }
    w.pass();
    let frames = rig.frames();
    for (sid, _) in &tls {
        let closes = recs_of(&frames, sid)
            .iter()
            .filter(|r| r.node == Node::SESSION && r.op == aterm_slog::Op::Close)
            .count();
        assert_eq!(closes, 1, "{sid}'s close");
    }
    assert!(
        w.tracks.is_empty(),
        "every retired track let go once written"
    );
}

/// The real registry must hand a closed timeline to the writer even when
/// no writer snapshot ever included that session. All fixtures are fd-less
/// SessionHandles and the actual Store adapter over an in-memory JournalFs.
#[test]
fn the_real_store_keeps_a_session_that_retires_before_its_first_writer_snapshot() {
    fn retired(observed_live: bool, prime_empty: bool, local: u64) -> (bool, usize) {
        let rig = Rig::new();
        let store = crate::session_store::new_store();
        let mut writer = Writer::new(
            Arc::new(rig.fs.clone()),
            Arc::clone(&store),
            inst("p1-a1"),
            Arc::clone(&rig.clock) as Arc<dyn Clock>,
            WriterLimits::default(),
            Arc::clone(&rig.report),
        );
        writer.start();
        if prime_empty {
            writer.pass();
        }
        let handle = crate::session_store::test_handle(local);
        let sid = handle.sid.as_str().to_string();
        let timeline = Arc::clone(&handle.ctx.timeline);
        store
            .write()
            .unwrap_or_else(|p| p.into_inner())
            .register(handle);
        {
            let mut guard = timeline.lock().unwrap_or_else(|p| p.into_inner());
            let at = guard.slog().now();
            guard
                .slog_mut()
                .log_mut_for_journal()
                .mark(Mark::new(Kind::Human, Src::Input, Conf::Exact), at)
                .expect("the live session accepts its input mark");
        }
        if observed_live {
            writer.pass();
            assert!(
                writer.tracks.contains_key(&sid),
                "the positive was seen live"
            );
        } else {
            assert!(
                !writer.tracks.contains_key(&sid),
                "no snapshot saw this session"
            );
        }
        let closed = store
            .write()
            .unwrap_or_else(|p| p.into_inner())
            .deregister_local_as(
                local,
                crate::session_store::ExitReason::CtlClose,
                crate::session_store::ExitActor::Human,
            );
        assert_eq!(closed.as_ref().map(|s| s.as_str()), Some(sid.as_str()));
        assert_eq!(
            store
                .read()
                .unwrap_or_else(|p| p.into_inner())
                .live_handles()
                .count(),
            0
        );
        let expected = with_log(&timeline, |log| {
            all(log).iter().map(host).collect::<Vec<_>>()
        });
        assert_eq!(
            expected
                .iter()
                .filter(|r| r.node == Node::SESSION && r.op == aterm_slog::Op::Close)
                .count(),
            1,
            "the actual deregister produced its session close",
        );
        drop(timeline);
        // No fixture-owned timeline survives to supply what Store dropped.
        // A second pass catches an accidentally live (rather than retired)
        // import and checks that a completed retire is no longer retained.
        writer.pass();
        writer.pass();
        let actual = recs_of(&rig.frames(), &sid);
        (actual == expected, writer.tracks.len())
    }

    assert_eq!(
        retired(true, false, 1),
        (true, 0),
        "previously observed retirement remains whole"
    );
    let observations = [retired(false, false, 2), retired(false, true, 3)];
    assert_eq!(
        observations,
        [(true, 0), (true, 0)],
        "both a first-pass session and one between empty snapshots retain every record and close",
    );
}

/// A full disk (ENOSPC after `k` bytes, for every `k` of a batch): only
/// whole lines are kept, the segment is abandoned, the writer backs off, the
/// paused attention is raised once, and the pass after the backoff writes the
/// rest into a new segment and clears it. The view loses nothing and shows
/// each seq once.
#[test]
fn a_full_disk_keeps_whole_lines_backs_off_and_resumes() {
    for k in 0..400u64 {
        let rig = Rig::new();
        let tl = rig.sessions.add("s-full");
        let mut w = rig.writer();
        w.pass();
        for i in 0..6 {
            mark(&tl, 1 + i);
        }
        rig.fs.fault(Some(Fault::After(k, libc::ENOSPC)));
        w.pass();
        let (gen1, a) = rig.report.attention();
        let short = matches!(a, Attention::Paused(Cause::Full));
        if short {
            assert!(w.wait().is_some(), "k={k}: backing off");
            rig.fs.fault(None);
            w.pass();
            assert_eq!(
                rig.report.attention().0,
                gen1,
                "k={k}: no write inside the backoff"
            );
            rig.clock.advance(BACKOFF_FIRST);
            assert!(w.pass() > 0, "k={k}: the rest written after the backoff");
            assert_eq!(rig.report.attention().1, Attention::None, "k={k}: cleared");
        } else {
            rig.fs.fault(None);
        }
        let v = rig.view("s-full");
        let recs = all(&v.log);
        let mut seqs: Vec<u64> = recs.iter().map(|r| r.seq).collect();
        seqs.dedup();
        assert_eq!(seqs.len(), recs.len(), "k={k}: a seq twice");
        assert_eq!(seqs, (1..=7).collect::<Vec<_>>(), "k={k}: nothing lost");
    }
}

/// NEVER SPINS: ten simulated minutes of dirty marks against a disk that is
/// always full make at most ⌈log₂(600/30)⌉ + 2 write attempts.
#[test]
fn a_full_disk_never_spins() {
    let rig = Rig::new();
    let tl = rig.sessions.add("s-spin");
    let mut w = rig.writer();
    w.pass();
    rig.fs.fault(Some(Fault::Always(libc::ENOSPC)));
    let before = rig.fs.state().writes;
    for i in 0..(600 * 4) {
        mark(&tl, 100 + i);
        w.pass();
        rig.clock.advance(COALESCE);
    }
    let attempts = rig.fs.state().writes - before;
    assert!(
        attempts <= 5 + 2,
        "{attempts} write attempts in ten minutes"
    );
}

#[test]
fn eio_eacces_eintr_and_eagain_each_do_what_the_table_says() {
    // EINTR: retried, the lines land.
    let rig = Rig::new();
    let tl = rig.sessions.add("s-err");
    let mut w = rig.writer();
    rig.fs.fault(Some(Fault::Eintr(2)));
    assert!(w.pass() > 0, "EINTR retried");
    // EAGAIN: owed, no escalation.
    mark(&tl, 1);
    rig.fs.fault(Some(Fault::Always(libc::EAGAIN)));
    w.pass();
    assert!(w.owed && matches!(w.state, State::Running));
    assert_eq!(rig.report.attention().1, Attention::None);
    // EIO three times: backoff, then stopped.
    rig.fs.fault(Some(Fault::Always(libc::EIO)));
    for _ in 0..3 {
        w.pass();
        rig.clock.advance(BACKOFF_MAX);
    }
    assert!(w.is_stopped(), "three EIOs stop it");
    assert_eq!(rig.report.attention().1, Attention::Stopped(Cause::Io));
    assert!(w.tracks.is_empty(), "a stopped writer holds no session");
    // EACCES: stopped at once.
    let rig = Rig::new();
    let _tl = rig.sessions.add("s-acc");
    let mut w = rig.writer();
    rig.fs.fault(Some(Fault::Always(libc::EACCES)));
    w.pass();
    assert!(w.is_stopped());
    assert_eq!(rig.report.attention().1, Attention::Stopped(Cause::Denied));
}

/// no `flock`, no journal: the lock is refused, nothing is
/// written, and the owner is told why.
#[test]
fn without_flock_there_is_no_journal() {
    let rig = Rig::new();
    let _tl = rig.sessions.add("s-nolock");
    rig.fs.state().no_flock = true;
    let mut w = rig.writer();
    assert_eq!(w.pass(), 0);
    assert!(w.is_stopped());
    assert!(rig.fs.names().is_empty(), "{:?}", rig.fs.names());
    assert_eq!(
        rig.report.attention().1,
        Attention::Stopped(Cause::Unsupported)
    );
}

#[test]
fn low_free_space_pauses_before_enospc() {
    let rig = Rig::new();
    let _tl = rig.sessions.add("s-low");
    rig.fs.state().free = Some(FREE_FLOOR - 1);
    let mut w = rig.writer();
    assert_eq!(w.pass(), 0);
    assert_eq!(rig.report.attention().1, Attention::Paused(Cause::Full));
    assert!(rig.fs.names().iter().all(|n| !n.ends_with(".slog")));
    rig.fs.state().free = Some(u64::MAX);
    rig.clock.advance(BACKOFF_FIRST);
    assert!(w.pass() > 0);
}

#[test]
fn a_panic_in_the_writer_stops_the_journal_and_producers_append_on() {
    let rig = Rig::new();
    let tl = rig.sessions.add("s-panic");
    let mut w = rig.writer();
    rig.fs.fault(Some(Fault::Panic));
    guarded(&mut w, |w| {
        let _ = w.pass();
    });
    assert!(w.is_stopped());
    assert_eq!(rig.report.attention().1, Attention::Stopped(Cause::Panic));
    for i in 0..100 {
        mark(&tl, 10 + i);
    }
    assert_eq!(w.pass(), 0);
}

/// NEVER BLOCKS: while the writer is stuck in a write, a producer appends a
/// thousand records and the writer holds no timeline lock. NEGATIVE CONTROL:
/// a writer that held the lock across its write is seen by the same probe.
#[test]
fn the_writer_never_blocks_a_producer() {
    let rig = Rig::new();
    let tl = rig.sessions.add("s-block");
    let mut w = rig.writer();
    w.pass();
    mark(&tl, 1);
    let barrier = Arc::new((Mutex::new(false), std::sync::Condvar::new()));
    rig.fs.fault(Some(Fault::Block(Arc::clone(&barrier))));
    let before = rig.fs.state().entered;
    let writer = std::thread::spawn(move || {
        let _ = w.pass();
        w
    });
    let deadline = std::time::Instant::now() + HANG;
    // The writer is inside its write once it has begun one.
    while rig.fs.state().entered == before {
        assert!(
            std::time::Instant::now() < deadline,
            "the writer never wrote"
        );
        std::thread::yield_now();
    }
    for i in 0..1000 {
        assert!(
            tl.try_lock().is_ok(),
            "the writer holds the timeline across I/O"
        );
        mark(&tl, 2 + i);
    }
    let (lock, cv) = &*barrier;
    *lock.lock().expect("barrier") = true;
    cv.notify_all();
    let w = writer.join().expect("the writer returns");
    drop(w);
    // NEGATIVE CONTROL: a lock held across I/O is what the probe sees.
    let held = Arc::clone(&tl);
    let gate = Arc::new((Mutex::new(false), std::sync::Condvar::new()));
    let g2 = Arc::clone(&gate);
    let bad = std::thread::spawn(move || {
        let _guard = held.lock().expect("held");
        let (lock, cv) = &*g2;
        let mut open = lock.lock().expect("gate");
        while !*open {
            open = cv.wait(open).expect("gate");
        }
    });
    let deadline = std::time::Instant::now() + HANG;
    while tl.try_lock().is_ok() {
        assert!(
            std::time::Instant::now() < deadline,
            "the bad writer never locked"
        );
        std::thread::yield_now();
    }
    let (lock, cv) = &*gate;
    *lock.lock().expect("gate") = true;
    cv.notify_all();
    bad.join().expect("joins");
}

/// content-tier fields written straight through the log never
/// reach the journal's bytes; a command's later argument never reaches its
/// `detail=`. NEGATIVE CONTROL: the same record with the title mistiered as
/// host carries it.
#[test]
fn the_content_tier_never_reaches_the_journal() {
    let rig = Rig::new();
    let tl = rig.sessions.add("s-leak");
    let title = Fields::new()
        .text(aterm_slog::Key::Title, "SECRET-TITLE-42")
        .expect("a title");
    with_log(&tl, |log| {
        log.mark(
            Mark::new(Kind::Title, Src::Status, Conf::Exact).fields(title),
            at(1),
        )
        .expect("a title mark");
    });
    let detail =
        crate::session_status::command_detail("printf %s x SECRET-ARG-42").unwrap_or_default();
    let f = Fields::new()
        .tok(aterm_slog::Key::Detail, &detail)
        .expect("a detail");
    with_log(&tl, |log| {
        log.open(
            Open::new(Kind::Cmd, Src::Shell, Conf::Strong).fields(f),
            at(2),
        )
        .expect("a run");
    });
    let mut w = rig.writer();
    w.pass();
    let bytes = String::from_utf8(rig.fs.all_bytes()).expect("ascii");
    assert!(!bytes.contains("SECRET-TITLE"), "{bytes}");
    assert!(!bytes.contains("SECRET-ARG"), "{bytes}");
    assert!(bytes.contains("detail=printf"), "{bytes}");
    let rec = tl
        .lock()
        .expect("tl")
        .slog()
        .log()
        .window()
        .find(|r| r.k == Kind::Title)
        .cloned()
        .expect("the mark");
    let mistiered: Vec<_> = aterm_slog::FIELDS
        .iter()
        .map(|&(k, t, c)| {
            (
                k,
                if k == aterm_slog::Key::Title {
                    aterm_slog::Tier::Host
                } else {
                    t
                },
                c,
            )
        })
        .collect();
    let leaked =
        aterm_slog::encode_with_table(&rec, aterm_slog::Tier::Host, &mistiered).expect("fits");
    assert!(leaked.contains("SECRET-TITLE-42"));
}

// ---- an update, the quit, the reader ---------------------------------

/// the outgoing writer drains each fenced log to its fence and
/// ends its chain `#handed`; the successor journals above the fence; a
/// reader shows each seq once across both. A drain cut short reads as `gap
/// why=handoff lost=<k>`. A rollback writes on in a new segment.
#[test]
fn an_update_hands_off_at_the_fence_and_the_successor_goes_on() {
    for cut in [false, true] {
        let rig = Rig::new();
        let tl = rig.sessions.add("s-up");
        let r = cmd(&tl, 1);
        let mut a = rig.writer_as("p1-a1", WriterLimits::default());
        a.pass();
        for i in 0..5 {
            mark(&tl, 2 + i);
        }
        if cut {
            // The disk stalls: A journals nothing more before its exit.
            rig.fs.fault(Some(Fault::Always(libc::EAGAIN)));
        }
        // The export: carry (with the journal's host line, as a running lane
        // writes it) and fence.
        let (lines, head) = {
            let mut g = tl.lock().expect("tl");
            let snap = g.slog().log().carry_snapshot();
            let out = snap
                .prepare()
                .encode(
                    aterm_slog::CarryBudget::SESSION,
                    &[crate::session_log::JOURNAL_HOST_LINE.to_string()],
                )
                .expect("carried");
            g.slog_mut().log_mut_for_journal().fence_reads();
            (out.lines, g.slog().log().last_seq())
        };
        a.begin_handing();
        a.pass();
        let frames = rig.frames();
        let handed = frames
            .iter()
            .any(|f| matches!(f, Frame::Handed { fence, .. } if *fence == head));
        let ended = frames.iter().any(|f| {
            matches!(
                f,
                Frame::End {
                    why: EndWhy::Handoff,
                    ..
                }
            )
        });
        assert_eq!(handed, !cut);
        assert_eq!(ended, !cut);
        rig.fs.fault(None);
        drop(a);
        // The successor: the carried log, its journal from the fence.
        let b_tl: Timeline = Arc::new(Mutex::new(SessionTimeline::for_test()));
        {
            let mut g = b_tl.lock().expect("b");
            let ids = crate::session_log::LogCarry {
                ep: 1,
                ids: aterm_slog::IdCounters {
                    run: 1,
                    turn: 0,
                    step: 0,
                },
            };
            g.slog_mut()
                .adopt_carried(&lines, ids, head + 65536, None, at(100))
                .expect("adopts");
            assert_eq!(g.slog().journal_start(), Some(head));
        }
        rig.sessions.remove("s-up");
        rig.sessions
            .list
            .lock()
            .expect("list")
            .push((ShortId::new("s-up").expect("sid"), Arc::clone(&b_tl)));
        // B's producers close the carried run (no producer state crossed).
        assert!(!b_tl.lock().expect("b").slog().log().is_open(r));
        mark(&b_tl, 110);
        let mut b = rig.writer_as("p2-b2", WriterLimits::default());
        b.pass();
        let v = rig.view("s-up");
        let recs = all(&v.log);
        let mut seqs: Vec<u64> = recs.iter().map(|r| r.seq).collect();
        seqs.dedup();
        assert_eq!(seqs.len(), recs.len(), "cut={cut}: each seq once");
        let read = v.log.read(Cursor::new(v.log.ep(), 0), usize::MAX);
        let g = read.gap.expect("the overlap is said");
        assert_eq!(g.why, GapWhy::Handoff);
        assert_eq!(g.lost, if cut { 5 } else { 0 }, "cut={cut}");
        assert!(!v.log.is_open(r));
        assert!(!v.crashed);
    }
    // A rollback: the writer goes on in a new segment.
    let rig = Rig::new();
    let tl = rig.sessions.add("s-back");
    let mut a = rig.writer();
    a.pass();
    tl.lock().expect("tl").slog_mut().carry_source();
    a.begin_handing();
    a.pass();
    assert!(a.handed_done);
    tl.lock().expect("tl").slog_mut().lift_read_fence();
    a.stood_down();
    mark(&tl, 9);
    a.pass();
    let segs: Vec<String> = rig
        .fs
        .names()
        .into_iter()
        .filter(|n| n.ends_with(".slog"))
        .collect();
    assert_eq!(segs.len(), 2, "{segs:?}");
    let newest = complete_lines(&rig.fs.bytes("p1-a1.2.slog"))
        .filter_map(|l| decode_line(l).ok())
        .any(|f| matches!(f, Frame::End { .. }));
    assert!(!newest, "the newest segment holds no trailer");
}

/// D3 ON ACROSS AN UPDATE: a predecessor
/// that kept no journal (`journal = false`, or an older build that shipped it
/// off) carries no `journal v=1` host line, so a successor on the default
/// config, whose gate is open, journals each carried log itself from its
/// first record, and only once its Commit lands: its armed lane (an update
/// candidate's, `lib.rs`) creates nothing before. The read shows every
/// carried seq once, from the first, losing none, and the run the
/// predecessor opened.
///
/// NEGATIVE CONTROLS: the predecessor's `journal = false` shuts the gate the
/// default opens (the old default fails the second assertion); a carry that
/// claims the journal (`journal v=1`) would start the successor's journal
/// after the head and lose the first records (the `journal_start` arm); a
/// lane that ignores `armed` starts its writer before the Commit (the
/// `Phase::Armed` assertion, read only once the lane's first pass decided).
#[cfg(unix)]
#[test]
fn a_journal_off_predecessor_hands_to_a_default_on_successor() {
    use crate::app_config::{Config, SessionLogConfig};
    use crate::desk::Desk;
    let off = Config {
        session_log: Some(SessionLogConfig {
            journal: Some(false),
            clause: None,
        }),
        ..Config::default()
    };
    assert!(
        !gate(&off, Desk::Window),
        "the predecessor keeps no journal"
    );
    assert!(
        gate(&Config::default(), Desk::Window),
        "the successor's default keeps one"
    );
    // The predecessor: no lane, a command and four marks, then its take.
    let a: Timeline = Arc::new(Mutex::new(SessionTimeline::for_test()));
    with_log(&a, |log| {
        log.open_session(
            Kind::Spawn,
            Src::Aterm,
            Conf::Exact,
            Actor::Nobody,
            Fields::new(),
            at(0),
        )
        .expect("opens");
    });
    let carried_run = cmd(&a, 1);
    for i in 0..4 {
        mark(&a, 2 + i);
    }
    let (lines, head, ids) = {
        let mut g = a.lock().expect("a");
        let out = g
            .slog_mut()
            .carry_source()
            .encode(aterm_slog::CarryBudget::SESSION)
            .expect("carried");
        let log = g.slog().log();
        (out.lines, log.last_seq(), log.id_counters())
    };
    assert!(
        !lines
            .iter()
            .any(|l| l == crate::session_log::JOURNAL_HOST_LINE),
        "a lane-less predecessor claims no journal: {lines:?}"
    );
    // The successor: a session of the real store adopts the carry.
    let handle = crate::session_store::test_handle(7);
    let sid = handle.sid.as_str().to_string();
    {
        let mut g = handle.ctx.timeline.lock().expect("b");
        g.slog_mut()
            .adopt_carried(
                &lines,
                crate::session_log::LogCarry { ep: 1, ids },
                head + 65536,
                None,
                at(100),
            )
            .expect("adopts");
        assert_eq!(g.slog().journal_start(), None, "from the first record");
    }
    let store = crate::session_store::new_store();
    store
        .write()
        .unwrap_or_else(|p| p.into_inner())
        .register(handle);
    let scratch_guard = tempdir("off-to-on");
    let scratch = scratch_guard.path();
    let logs = scratch.join("logs");
    std::fs::create_dir_all(&logs).expect("logs");
    let shared = Shared::new();
    // Join this lane before its scratch guard drops, including assertion
    // unwind. The finish request also wakes an armed or parked lane.
    struct Worker {
        shared: Arc<Shared>,
        thread: Option<std::thread::JoinHandle<()>>,
    }
    impl Drop for Worker {
        fn drop(&mut self) {
            if let Some(thread) = self.thread.take() {
                self.shared.request(REQ_FINISH);
                let _ = thread.join();
            }
        }
    }
    let mut thread = Worker {
        shared: Arc::clone(&shared),
        thread: Some({
            let (shared, logs, store) = (Arc::clone(&shared), logs.clone(), Arc::clone(&store));
            std::thread::spawn(move || run(&shared, &logs, store, inst("p2-b2"), true))
        }),
    };
    // A hang detector: the lane's first pass decides, either to wait for
    // the Commit (the armed lane's park) or to start a writer.
    let decided = aterm_spec::wait::within(HANG, Duration::from_millis(5), || {
        (shared.armed_waits.load(Ordering::Acquire) > 0 || shared.phase() != Phase::Armed)
            .then_some(())
    });
    assert!(decided.is_some(), "the lane never ran");
    assert_eq!(
        shared.phase(),
        Phase::Armed,
        "an armed successor starts no writer before its Commit"
    );
    assert_eq!(
        std::fs::read_dir(&logs).expect("logs").count(),
        0,
        "an armed successor writes nothing before its Commit"
    );
    // The Commit lands (`commit_landed`): the writer starts.
    shared.request(REQ_COMMIT);
    // A hang detector: the pass the Commit asks for must come.
    let view = aterm_spec::wait::within(HANG, Duration::from_millis(20), || {
        let fs = super::fs::PinnedJournalFs::open(&logs, false).ok()??;
        match view_with(
            &fs,
            &Mutex::new(Index::default()),
            &ShortId::new(&sid).expect("a sid"),
            Budgets::READER,
        ) {
            JournalRead::View(v) if v.log.last_seq() >= head => Some(*v),
            _ => None,
        }
    })
    .expect("the successor journals the carried log after its Commit");
    let (tx, rx) = std::sync::mpsc::sync_channel(1);
    *shared.ack.lock().unwrap_or_else(|p| p.into_inner()) = Some(tx);
    shared.request(REQ_FINISH);
    rx.recv_timeout(HANG)
        .expect("the writer answers its finish");
    thread
        .thread
        .take()
        .expect("the lane is owned")
        .join()
        .expect("the writer thread ends");
    let read = view.log.read(Cursor::new(view.log.ep(), 0), usize::MAX);
    // The epoch the adoption took is said as a handoff, and nothing is lost.
    assert!(
        read.gap.is_none_or(|g| g.lost == 0),
        "nothing lost: {:?}",
        read.gap
    );
    let seqs: Vec<u64> = read.recs.iter().map(|r| r.seq).collect();
    assert_eq!(seqs.first(), Some(&1), "from the first record: {seqs:?}");
    let mut once = seqs.clone();
    once.dedup();
    assert_eq!(once, seqs, "each seq once");
    assert!(
        (1..=head).all(|n| seqs.contains(&n)),
        "every carried record: {seqs:?} through {head}"
    );
    assert!(
        read.recs
            .iter()
            .any(|r| r.node == carried_run && r.op == aterm_slog::Op::Open),
        "the predecessor's run, from the carry"
    );
}

/// a clean quit writes each open session's closes and `#fin`; the
/// next read finds no crash. A deadline that passes first writes no `#fin`,
/// and the chain then reads as a crash.
#[test]
fn a_clean_quit_closes_its_journal_and_a_late_one_reads_as_a_crash() {
    let rig = Rig::new();
    let tl = rig.sessions.add("s-quit");
    let r = cmd(&tl, 1);
    let mut w = rig.writer();
    w.pass();
    let deadline = rig.clock.mono() + QUIT_BUDGET;
    w.finish(deadline);
    drop(w);
    let frames = rig.frames();
    assert!(matches!(
        frames.last(),
        Some(Frame::End {
            why: EndWhy::Quit,
            ..
        })
    ));
    assert!(
        !rig.fs.names().contains(&"p1-a1.lock".to_string()),
        "its lock removed"
    );
    let v = rig.view("s-quit");
    assert!(!v.crashed);
    let last = all(&v.log).last().cloned().expect("records");
    assert_eq!(last.out(), Some("app-quit"));
    assert!(!v.log.is_open(r));
    assert!(
        tl.lock().expect("tl").slog().log().is_open(r),
        "the live log unchanged"
    );
    // Late: the deadline already passed.
    let rig = Rig::new();
    let tl = rig.sessions.add("s-late");
    let _ = cmd(&tl, 1);
    let mut w = rig.writer();
    w.pass();
    w.finish(rig.clock.mono());
    drop(w);
    assert!(!rig.frames().iter().any(|f| matches!(f, Frame::End { .. })));
    let v = rig.view("s-late");
    assert!(v.crashed, "a quit cut short reads as a crash");
}

/// THE FINISH NEVER HOLDS THE EXIT: a lane whose writer is stuck in a write
/// answers its quit within the budget's wait (the hang detector bounds the
/// test, never a latency).
#[test]
fn a_stuck_writer_never_holds_the_exit() {
    let shared = Shared::new();
    let lane = Lane {
        shared: Some(Arc::clone(&shared)),
        logs: None,
        armed: false,
        started: true,
    };
    // No thread answers at all: the wait ends at its budget.
    let _ = shared.wake.thread.set(std::thread::current());
    let t = std::time::Instant::now();
    lane.finish_at_quit(Duration::from_millis(10));
    assert!(t.elapsed() < HANG);
}

#[test]
fn retention_keeps_the_budget_oldest_first_and_spares_the_live() {
    let rig = Rig::new();
    // Three dead instances' old segments, and a live one's.
    for (i, name) in ["p7-aa.1.slog", "p7-aa.2.slog", "p8-bb.1.slog"]
        .iter()
        .enumerate()
    {
        rig.fs.tick(10);
        rig.fs.put(name, &vec![b'x'; 400 + i], false, false);
    }
    let live = rig
        .fs
        .lock_instance(&inst("p9-cc"))
        .expect("a live instance");
    rig.fs.put("p9-cc.1.slog", &[b'y'; 500], false, false);
    rig.fs.put("p7-aa.lock", b"", false, false);
    rig.fs.put("notes.txt", &[b'z'; 5000], false, false);
    let limits = WriterLimits {
        budget_bytes: 1200,
        ..WriterLimits::default()
    };
    let w = rig.writer_as("p1-a1", limits);
    let names = rig.fs.names();
    assert!(
        !names.contains(&"p7-aa.1.slog".to_string()),
        "the oldest went: {names:?}"
    );
    assert!(
        names.contains(&"p9-cc.1.slog".to_string()),
        "the live one stays"
    );
    assert!(
        names.contains(&"notes.txt".to_string()),
        "a foreign file is never counted"
    );
    assert!(
        names.contains(&"p8-bb.1.slog".to_string()),
        "stopped at the budget"
    );
    drop(w);
    drop(live);
}

/// The reader's bounds: a decode budget cut reads `why=unread`; a segment
/// over the read limit is not read and reads `why=unread` too; a partial
/// last line and a newer format are skipped; lines appended after a read are
/// found by the next, which reads only the tail.
#[test]
fn the_reader_is_bounded_and_skips_what_is_not_a_line() {
    let rig = Rig::new();
    let tl = rig.sessions.add("s-read");
    let limits = WriterLimits {
        segment_bytes: 400,
        ..WriterLimits::default()
    };
    let mut w = rig.writer_as("p1-a1", limits);
    for i in 0..30 {
        mark(&tl, 1 + i);
        w.pass();
    }
    let index = Mutex::new(Index::default());
    let sid = ShortId::new("s-read").expect("sid");
    let tight = Budgets {
        decode: 1200,
        min_segments: 1,
        ..Budgets::READER
    };
    let JournalRead::View(small) = view_with(&rig.fs, &index, &sid, tight) else {
        panic!("a view");
    };
    let read = small.log.read(Cursor::new(small.log.ep(), 0), usize::MAX);
    assert_eq!(read.gap.map(|g| g.why), Some(GapWhy::Unread));
    let whole = read_view(&rig.fs, "s-read", Budgets::READER);
    assert_eq!(all(&whole.log).len(), 31);
    // Appended later: found.
    mark(&tl, 99);
    w.pass();
    let JournalRead::View(again) = view_with(&rig.fs, &index, &sid, Budgets::READER) else {
        panic!("a view");
    };
    assert_eq!(all(&again.log).len(), 32);
    // A partial last line and a newer format add nothing.
    let mut torn = rig.fs.bytes("p1-a1.1.slog");
    torn.extend_from_slice(b"s1\ts-read\t999 mark - human src=input conf=exact t=1");
    rig.fs.put("p3-dd.1.slog", &torn, false, false);
    let newer = b"s1\t*\t#seg\tv=2\tinst=p4-ee\tn=1\tt=1\tbuild=x\tend=\ns1\ts-read\t#snap\thead=0\tep=1\tbase=1\tfirst=1\topen=0\tt=1\tend=\ns1\ts-read\t998 mark - human src=input conf=exact t=1\tend=\n";
    rig.fs.put("p4-ee.1.slog", newer, false, false);
    let v = read_view(&rig.fs, "s-read", Budgets::READER);
    assert!(
        all(&v.log).iter().all(|r| r.seq < 998),
        "nothing torn or newer"
    );
    assert!(!says(&v, "unread"), "a whole read says nothing unread");
    // A segment past the read limit: not read, and said unread.
    let big = vec![b'\n'; usize::try_from(aterm_slog::journal::SEGMENT_READ_MAX).unwrap_or(0) + 1];
    rig.fs.put("p5-ff.1.slog", &big, false, false);
    let v = read_view(&rig.fs, "s-read", Budgets::READER);
    assert!(says(&v, "unread"), "an oversized segment is unread");
    let _ = Meta {
        size: 0,
        mtime_ms: 0,
        dev: 0,
        ino: 0,
        private: true,
    };
}

// ---- the real directory -------------------------------------------------

/// The real directory's file rules: a segment is created exclusive (an
/// existing name, a symlink and a FIFO refused) at 0600; the instance lock
/// probes live while held and dead once let go (and gone); a symlinked
/// `session-log` is refused with its target untouched, and a foreign mode is
/// reset only by the writer.
#[cfg(unix)]
#[test]
fn the_real_directory_keeps_its_rules() {
    use std::os::unix::fs::{PermissionsExt as _, symlink};
    let scratch_guard = tempdir("real");
    let scratch = scratch_guard.path();
    let logs = scratch.join("logs");
    std::fs::create_dir_all(&logs).expect("logs");
    let dir = super::fs::PinnedJournalFs::open(&logs, true)
        .expect("opens")
        .expect("made");
    let journal = logs.join("session-log");
    // Exclusive, private, appendable.
    let mut seg = dir.create_segment("p1-aa.1.slog").expect("a segment");
    assert_eq!(seg.write(b"x\n").expect("writes"), 2);
    let md = std::fs::metadata(journal.join("p1-aa.1.slog")).expect("made");
    assert_eq!(md.permissions().mode() & 0o777, 0o600);
    assert!(
        dir.create_segment("p1-aa.1.slog").is_err(),
        "an existing name"
    );
    symlink(scratch.join("victim"), journal.join("p1-aa.2.slog")).expect("a symlink");
    assert!(dir.create_segment("p1-aa.2.slog").is_err(), "a symlink");
    assert!(!scratch.join("victim").exists(), "nothing made behind it");
    let fifo = std::ffi::CString::new(journal.join("p1-aa.3.slog").to_string_lossy().as_bytes())
        .expect("a path");
    // SAFETY: a NUL-terminated path; mkfifo makes one node.
    assert_eq!(unsafe { libc::mkfifo(fifo.as_ptr(), 0o600) }, 0);
    assert!(dir.create_segment("p1-aa.3.slog").is_err(), "a FIFO");
    // The lock: live while held, dead once let go.
    let me = inst("p1-aa");
    let held = dir.lock_instance(&me).expect("locks");
    assert_eq!(dir.probe("p1-aa.lock"), Liveness::Live);
    assert!(
        !journal.join(".p1-aa.lock.pending").exists(),
        "renamed into place"
    );
    drop(held);
    assert_eq!(dir.probe("p1-aa.lock"), Liveness::Dead);
    let held = dir.lock_instance(&inst("p2-bb")).expect("locks");
    dir.unlock_instance(&inst("p2-bb"), held);
    assert!(
        !journal.join("p2-bb.lock").exists(),
        "the clean end removes it"
    );
    assert_eq!(dir.probe("p2-bb.lock"), Liveness::Dead);
    // A wider mode is the writer's to reset; a reader changes nothing.
    std::fs::set_permissions(&journal, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    let _reader = super::fs::PinnedJournalFs::open(&logs, false)
        .expect("opens")
        .expect("exists");
    let mode = |p: &std::path::Path| std::fs::metadata(p).expect("md").permissions().mode() & 0o777;
    assert_eq!(mode(&journal), 0o755, "the reader changed nothing");
    let _writer = super::fs::PinnedJournalFs::open(&logs, true)
        .expect("opens")
        .expect("exists");
    assert_eq!(mode(&journal), 0o700, "the writer resets it");
    // A symlinked `session-log`: refused, its target untouched.
    let other = scratch.join("other-logs");
    std::fs::create_dir_all(&other).expect("other");
    let target = scratch.join("target");
    std::fs::create_dir_all(&target).expect("target");
    symlink(&target, other.join("session-log")).expect("a symlink");
    assert!(super::fs::PinnedJournalFs::open(&other, true).is_err());
    assert!(super::fs::PinnedJournalFs::open(&other, false).is_err());
    assert_eq!(std::fs::read_dir(&target).expect("target").count(), 0);
}

/// THE HOSTILE DIRECTORY: retention, with a 1 KiB budget, removes
/// only real, private segments of dead instances, oldest first, and stops at
/// the budget. A symlink, a directory, a hard link, a FIFO, a 0644 file and
/// foreign names that match loose patterns are all left as they are, and
/// nothing outside the directory is touched; a live instance's segments
/// stay; a dead instance's lock goes once its segments are gone.
#[cfg(unix)]
#[test]
fn retention_in_a_hostile_directory_removes_only_what_it_may() {
    use std::os::unix::fs::{PermissionsExt as _, symlink};
    let scratch_guard = tempdir("hostile");
    let scratch = scratch_guard.path();
    let logs = scratch.join("logs");
    std::fs::create_dir_all(&logs).expect("logs");
    let dir = super::fs::PinnedJournalFs::open(&logs, true)
        .expect("opens")
        .expect("made");
    let j = logs.join("session-log");
    let write = |name: &str, bytes: usize| {
        let mut f = dir.create_segment(name).expect("a segment");
        let _ = f.write(&vec![b'x'; bytes]).expect("writes");
    };
    std::fs::write(scratch.join("victim"), b"victim").expect("victim");
    std::fs::write(scratch.join("outside"), b"outside").expect("outside");
    symlink(scratch.join("victim"), j.join("p1-aa.1.slog")).expect("symlink");
    std::fs::create_dir(j.join("p2-bb.1.slog")).expect("a dir");
    std::fs::hard_link(scratch.join("outside"), j.join("p3-cc.1.slog")).expect("a hard link");
    let fifo = std::ffi::CString::new(j.join("p4-dd.1.slog").to_string_lossy().as_bytes())
        .expect("a path");
    // SAFETY: a NUL-terminated path; mkfifo makes one node.
    assert_eq!(unsafe { libc::mkfifo(fifo.as_ptr(), 0o600) }, 0);
    std::fs::write(j.join("p5-ee.1.slog"), vec![b'w'; 3000]).expect("a 0644 file");
    std::fs::set_permissions(
        j.join("p5-ee.1.slog"),
        std::fs::Permissions::from_mode(0o644),
    )
    .expect("0644");
    std::fs::write(j.join("notes.txt"), vec![b'n'; 100_000]).expect("notes");
    std::fs::write(j.join("p6-ff.1.slog.bak"), vec![b'b'; 3000]).expect("bak");
    // Real old segments of a dead instance (and its lock file), oldest first.
    for n in 1..=3 {
        write(&format!("p7-aa.{n}.slog"), 900);
    }
    let dead = dir.lock_instance(&inst("p7-aa")).expect("locks");
    drop(dead);
    // A live instance's segment.
    let live = dir.lock_instance(&inst("p8-bb")).expect("locks");
    write("p8-bb.1.slog", 900);
    let clock = Arc::new(TestClock::default());
    let mut w = Writer::new(
        Arc::new(
            super::fs::PinnedJournalFs::open(&logs, true)
                .expect("opens")
                .expect("there"),
        ) as Arc<dyn JournalFs>,
        Sessions::default(),
        inst("p9-cc"),
        clock as Arc<dyn Clock>,
        WriterLimits {
            budget_bytes: 1024,
            ..WriterLimits::default()
        },
        Arc::new(Report::default()),
    );
    w.retention();
    let exists = |name: &str| j.join(name).symlink_metadata().is_ok();
    for gone in ["p7-aa.1.slog", "p7-aa.2.slog", "p7-aa.3.slog", "p7-aa.lock"] {
        assert!(!exists(gone), "{gone} removed");
    }
    for kept in [
        "p1-aa.1.slog",
        "p2-bb.1.slog",
        "p3-cc.1.slog",
        "p4-dd.1.slog",
        "p5-ee.1.slog",
        "notes.txt",
        "p6-ff.1.slog.bak",
        "p8-bb.1.slog",
        "p8-bb.lock",
        "prune.lock",
    ] {
        assert!(exists(kept), "{kept} kept");
    }
    assert_eq!(
        std::fs::read(scratch.join("victim")).expect("victim"),
        b"victim"
    );
    assert_eq!(
        std::fs::read(scratch.join("outside")).expect("outside"),
        b"outside"
    );
    // Stops at the budget: two dead segments of 900 B, a 1 KiB budget, the
    // oldest goes and the newer stays.
    for n in 1..=2 {
        write(&format!("p6-dd.{n}.slog"), 900);
    }
    let mut w2 = Writer::new(
        Arc::new(
            super::fs::PinnedJournalFs::open(&logs, true)
                .expect("opens")
                .expect("there"),
        ) as Arc<dyn JournalFs>,
        Sessions::default(),
        inst("p9-cc"),
        Arc::new(TestClock::default()) as Arc<dyn Clock>,
        WriterLimits {
            budget_bytes: 1024 + 900,
            ..WriterLimits::default()
        },
        Arc::new(Report::default()),
    );
    w2.retention();
    assert!(!exists("p6-dd.1.slog") && exists("p6-dd.2.slog"));
    assert!(exists("p8-bb.1.slog"), "still live");
    drop(live);
}

/// VERSION SKEW: a config with a key this build does not know under
/// `[session_log]` reads (the key ignored); a carry without the journal's
/// host line (an older outgoing build, or a journal off or stopped) starts
/// the successor's journal at the log's first record.
#[test]
fn version_skew_both_ways() {
    let cfg: crate::app_config::Config =
        aterm_toml::from_str("[session_log]\njournal = true\nnewer_key = 3\n")
            .expect("an unknown key is skipped");
    assert!(cfg.session_log_journal());
    let mut log = Log::new(1);
    log.open_session(
        Kind::Spawn,
        Src::Aterm,
        Conf::Exact,
        Actor::Nobody,
        Fields::new(),
        at(0),
    )
    .expect("opens");
    let out = log
        .carry_snapshot()
        .prepare()
        .encode(aterm_slog::CarryBudget::SESSION, &[])
        .expect("carried");
    let mut tl = SessionTimeline::for_test();
    let ids = crate::session_log::LogCarry {
        ep: 1,
        ids: aterm_slog::IdCounters::default(),
    };
    tl.slog_mut()
        .adopt_carried(&out.lines, ids, log.last_seq() + 65536, None, at(5))
        .expect("adopts");
    assert_eq!(
        tl.slog().journal_start(),
        None,
        "no host line: from the first record"
    );
    // An older build's config reads a table it does not know the same way:
    // skipped, not refused; a config without `[session_log]` asks for the
    // journal (D3 on).
    let older: crate::app_config::Config =
        aterm_toml::from_str("[a_future_table]\nanything = true\n").expect("skipped");
    assert!(older.session_log_journal());
}

/// A writer that runs long stays within the budget: it removes its own
/// segments it has rotated past, oldest first, never the one it writes.
#[test]
fn a_long_running_writer_keeps_its_own_segments_within_the_budget() {
    let rig = Rig::new();
    let tl = rig.sessions.add("s-long");
    let limits = WriterLimits {
        segment_bytes: 500,
        budget_bytes: 2000,
        ..WriterLimits::default()
    };
    let mut w = rig.writer_as("p1-a1", limits);
    for i in 0..400 {
        mark(&tl, 1 + i);
        w.pass();
    }
    let total: u64 = rig
        .fs
        .names()
        .iter()
        .filter(|n| n.ends_with(".slog"))
        .map(|n| rig.fs.bytes(n).len() as u64)
        .sum();
    assert!(total <= 2000 + 500 + 1024, "{total} bytes of segments");
    let current = w.seg.as_ref().map(|s| s.n).expect("a segment");
    assert!(rig.fs.names().contains(&format!("p1-a1.{current}.slog")));
    assert!(
        !rig.fs.names().contains(&"p1-a1.1.slog".to_string()),
        "the oldest went"
    );
}

// ---- the fixer's findings: bounds, a rollback, the quit, the backstop -----

/// RETENTION BOUNDS THE COUNT AS WELL AS THE BYTES: a journal of
/// 5000 small dead segments (far under the byte budget, far over a
/// listing's 4096 names, some of the oldest visited last) is pruned, oldest
/// first, to within `journal::FILES_MAX` files and the byte budget, so one
/// listing sees the whole journal again.
#[test]
fn retention_keeps_the_count_budget_and_sees_past_a_listing() {
    let rig = Rig::new();
    for n in 1..=5000u32 {
        rig.fs.tick(1);
        rig.fs
            .put(&format!("p7-aa.{n}.slog"), &[b'x'; 100], false, false);
    }
    rig.fs.put("p7-aa.lock", b"", false, false);
    // The oldest are where a readdir may put them: past 4096 names.
    for n in 1..=50u32 {
        rig.fs.state().listed_last.insert(format!("p7-aa.{n}.slog"));
    }
    let limits = WriterLimits {
        budget_bytes: 300_000,
        ..WriterLimits::default()
    };
    let _w = rig.writer_as("p1-a1", limits);
    let names = rig.fs.names();
    let ours: Vec<&String> = names
        .iter()
        .filter(|n| {
            aterm_slog::journal::parse_segment_name(n).is_some()
                || aterm_slog::journal::parse_lock_name(n).is_some()
        })
        .collect();
    assert!(
        ours.len() <= aterm_slog::journal::FILES_MAX,
        "{} files of the journal's own",
        ours.len()
    );
    assert!(
        !names.contains(&"p7-aa.1.slog".to_string()),
        "the oldest went"
    );
    assert!(
        names.contains(&"p7-aa.5000.slog".to_string()),
        "the newest stays"
    );
    let bytes: usize = names
        .iter()
        .filter(|n| n.ends_with(".slog"))
        .map(|n| rig.fs.bytes(n).len())
        .sum();
    assert!(bytes <= 300_000, "{bytes} bytes");
    assert!(
        list(&rig.fs).expect("lists").truncated.not(),
        "a listing sees it all"
    );
}

use std::ops::Not as _;

/// A CUT LISTING DERIVES NO CRASH: a chain that quit cleanly
/// across two segments, its second (holding the closes and `#fin`) past a
/// listing's 4096 names, reads `gap why=unread`, never a crash. CONTROL: the
/// same chain listed whole reads its `out=app-quit` close.
#[test]
fn a_cut_listing_reads_unread_and_never_a_crash() {
    let rig = Rig::new();
    let tl = rig.sessions.add("s-cut");
    let limits = WriterLimits {
        segment_bytes: 300,
        ..WriterLimits::default()
    };
    let mut w = rig.writer_as("p9-ab", limits);
    w.pass();
    for i in 0..6 {
        mark(&tl, 1 + i);
        w.pass();
    }
    w.finish(rig.clock.mono() + QUIT_BUDGET);
    drop(w);
    let segs: Vec<String> = rig
        .fs
        .names()
        .into_iter()
        .filter(|n| n.ends_with(".slog"))
        .collect();
    assert!(segs.len() >= 2, "{segs:?}");
    let control = rig.view("s-cut");
    assert!(!control.crashed);
    assert_eq!(
        all(&control.log).last().and_then(|r| r.out()),
        Some("app-quit")
    );
    let newest = segs.iter().max().cloned().expect("a segment");
    rig.fs.state().listed_last.insert(newest);
    for i in 0..aterm_slog::journal::LIST_MAX {
        // Names a readdir lists before the chain's newest segment.
        rig.fs.put(&format!("zz-junk-{i:05}"), b"", false, false);
    }
    assert!(list(&rig.fs).expect("lists").truncated);
    let v = rig.view("s-cut");
    assert!(!v.crashed, "a cut listing never reads as a crash");
    assert!(says(&v, "unread"), "the cut is said");
}

/// PRESENCE IS NEVER GUESSED FROM A DECODE BUDGET: a session
/// whose segments are all older than a decode budget's reach is still read
/// (the index knows where it is, and its newest segments are decoded
/// whatever their size), the same on every request; a read whose index
/// could not reach it says it was cut, never `ERR no such session`; a sid in
/// no segment of a whole read is absent.
#[test]
fn a_session_past_the_decode_budget_is_found_and_a_cut_is_said() {
    let rig = Rig::new();
    let old = rig.sessions.add("s-old");
    let limits = WriterLimits {
        segment_bytes: 300,
        ..WriterLimits::default()
    };
    let mut w = rig.writer_as("p1-a1", limits);
    w.pass();
    mark(&old, 1);
    w.pass();
    close_session(&old, 2);
    rig.sessions.remove("s-old");
    w.pass();
    let busy = rig.sessions.add("s-busy");
    for i in 0..60 {
        mark(&busy, 10 + i);
        w.pass();
    }
    let index = Mutex::new(Index::default());
    let sid = ShortId::new("s-old").expect("sid");
    let tight = Budgets {
        decode: 1,
        ..Budgets::READER
    };
    for _ in 0..3 {
        let JournalRead::View(v) = view_with(&rig.fs, &index, &sid, tight) else {
            panic!("found every time");
        };
        assert!(!v.log.is_open(aterm_slog::Node::SESSION));
    }
    let cut = Budgets {
        index: 1,
        ..Budgets::READER
    };
    assert!(matches!(
        view_with(&rig.fs, &Mutex::new(Index::default()), &sid, cut),
        JournalRead::Cut
    ));
    let unknown = ShortId::new("s-nobody").expect("sid");
    assert!(matches!(
        view_with(&rig.fs, &index, &unknown, Budgets::READER),
        JournalRead::Absent
    ));
}

/// NO SEGMENT A READER SKIPS: a pass over many retired sessions with
/// backlogs (retired ones are exempt from the pass's byte bound) stops at
/// the segment's rotation size, so no segment passes it by more than one
/// take's lines; every session's close is still written.
#[test]
fn a_pass_never_writes_past_the_rotation_size_by_more_than_one_take() {
    let rig = Rig::new();
    let tls: Vec<(String, Timeline)> = (0..60)
        .map(|i| {
            let sid = format!("s-r{i:02}");
            let tl = rig.sessions.add(&sid);
            (sid, tl)
        })
        .collect();
    let limits = WriterLimits {
        segment_bytes: 4096,
        ..WriterLimits::default()
    };
    let mut w = rig.writer_as("p1-a1", limits);
    w.pass();
    for (sid, tl) in &tls {
        for i in 0..40 {
            mark(tl, 1 + i);
        }
        close_session(tl, 50);
        rig.sessions.remove(sid);
    }
    for _ in 0..200 {
        w.pass();
        if !w.owed {
            break;
        }
    }
    let one_take = 64 * 1024;
    for name in rig.fs.names().iter().filter(|n| n.ends_with(".slog")) {
        let size = rig.fs.bytes(name).len();
        assert!(size <= 4096 + one_take, "{name}: {size} bytes");
    }
    let frames = rig.frames();
    for (sid, _) in &tls {
        assert!(
            recs_of(&frames, sid)
                .iter()
                .any(|r| r.node == Node::SESSION && r.op == aterm_slog::Op::Close),
            "{sid}'s close"
        );
    }
}

/// A transient error (EAGAIN, or EINTR past its retries) wrote nothing: the
/// segment is kept, never a new file per retry, and a run of them backs off.
/// A hundred retry ticks of EAGAIN make one segment and a bounded number of
/// write attempts.
#[test]
fn transient_errors_keep_the_segment_and_back_off() {
    let rig = Rig::new();
    let tl = rig.sessions.add("s-again");
    let mut w = rig.writer();
    w.pass();
    rig.fs.fault(Some(Fault::Always(libc::EAGAIN)));
    let created = rig.fs.state().created;
    let before = rig.fs.state().writes;
    for i in 0..100 {
        mark(&tl, 10 + i);
        w.pass();
        rig.clock.advance(RETRY_TICK);
    }
    assert_eq!(rig.fs.state().created, created, "no new segment");
    let attempts = rig.fs.state().writes - before;
    assert!(attempts <= 20, "{attempts} write attempts");
    rig.fs.fault(None);
    rig.clock.advance(BACKOFF_MAX);
    assert!(w.pass() > 0, "written once the disk takes it");
    let v = rig.view("s-again");
    assert_eq!(all(&v.log).len(), 101, "nothing lost");
}

/// an idle session that
/// wrote its `#handed` while a busy one held the drain back is restated
/// when the update stands down, so a later crash reads it closed.
#[test]
fn a_rollback_mid_drain_then_a_crash_reads_closed() {
    let rig = Rig::new();
    let idle = rig.sessions.add("s-idle");
    let _ = cmd(&idle, 1);
    let busy = rig.sessions.add("s-busy");
    let mut w = rig.writer();
    w.pass();
    for tl in [&idle, &busy] {
        tl.lock().expect("tl").slog_mut().carry_source();
    }
    w.begin_handing();
    {
        let _held = busy.lock().expect("busy");
        w.pass();
    }
    assert!(!w.handed_done, "the drain is not whole");
    assert!(
        rig.frames()
            .iter()
            .any(|f| matches!(f, Frame::Handed { sid, .. } if sid.as_str() == "s-idle")),
        "the idle session handed"
    );
    for tl in [&idle, &busy] {
        tl.lock().expect("tl").slog_mut().lift_read_fence();
    }
    w.stood_down();
    mark(&busy, 5);
    w.pass();
    drop(w);
    let v = rig.view("s-idle");
    assert!(v.crashed, "a crash after the rollback is read as one");
    assert!(!v.log.is_open(Node::SESSION));
}

/// A SESSION CLOSED IN AN UPDATE'S OVERLAP, THEN A ROLLBACK: its close lies
/// past the fence, so its take is not done and its track is kept; after the
/// rollback the close is journaled.
#[test]
fn a_session_closed_in_the_overlap_gets_its_close_after_a_rollback() {
    let rig = Rig::new();
    let tl = rig.sessions.add("s-closes");
    let mut w = rig.writer();
    w.pass();
    tl.lock().expect("tl").slog_mut().carry_source();
    w.begin_handing();
    close_session(&tl, 3);
    rig.sessions.remove("s-closes");
    w.pass();
    tl.lock().expect("tl").slog_mut().lift_read_fence();
    w.stood_down();
    w.pass();
    w.pass();
    w.finish(rig.clock.mono() + QUIT_BUDGET);
    let closes = recs_of(&rig.frames(), "s-closes")
        .iter()
        .filter(|r| r.node == Node::SESSION && r.op == aterm_slog::Op::Close)
        .count();
    assert_eq!(closes, 1, "the close journaled");
    assert!(!rig.view("s-closes").log.is_open(Node::SESSION));
}

/// THE QUIT'S CLOSES MEASURE THEIR NODES on each session's own clock: a
/// session open 4.2 s and a run open 4.1 s say so, not `dur_ms=0`.
#[test]
fn the_quit_closes_say_how_long_each_node_lived() {
    let rig = Rig::new();
    let tl = rig.sessions.add("s-dur");
    let r = cmd(&tl, 100);
    tl.lock()
        .expect("tl")
        .slog_mut()
        .set_clock(crate::session_log::Clock::reading(|| at(4200)));
    let mut w = rig.writer();
    w.pass();
    w.finish(rig.clock.mono() + QUIT_BUDGET);
    let recs = recs_of(&rig.frames(), "s-dur");
    let dur = |node: Node| {
        recs.iter()
            .find(|x| x.node == node && x.op == aterm_slog::Op::Close)
            .and_then(|x| match x.f.get(aterm_slog::Key::DurMs) {
                Some(aterm_slog::Val::U(n)) => Some(*n),
                _ => None,
            })
    };
    assert_eq!(dur(r), Some(4100), "{recs:?}");
    assert_eq!(dur(Node::SESSION), Some(4200));
}

/// A QUIT WHILE BACKING OFF from a full disk tries once more: the disk has
/// room again, so the closes and `#fin` land and no crash is read.
#[test]
fn a_quit_while_backing_off_tries_once_more() {
    let rig = Rig::new();
    let tl = rig.sessions.add("s-bq");
    let _ = cmd(&tl, 1);
    let mut w = rig.writer();
    w.pass();
    mark(&tl, 2);
    rig.fs.fault(Some(Fault::Always(libc::ENOSPC)));
    w.pass();
    assert!(w.wait().is_some(), "backing off");
    rig.fs.fault(None);
    w.finish(rig.clock.mono() + QUIT_BUDGET);
    assert!(rig.frames().iter().any(|f| matches!(
        f,
        Frame::End {
            why: EndWhy::Quit,
            ..
        }
    )));
    drop(w);
    assert!(!rig.view("s-bq").crashed);
}

/// THE BACKSTOP SAYS EVERY SESSION IT LETS GO: past
/// `retired_max`, a retired session gives up its timeline but keeps what its
/// `#gap why=dropped` says until that is written whole, a snap first, so a
/// write that fails meanwhile loses nothing and the reader places the loss.
#[test]
fn the_backstop_says_every_session_it_lets_go() {
    let rig = Rig::new();
    let tls: Vec<(String, Timeline)> = (0..6)
        .map(|i| {
            let sid = format!("s-d{i}");
            (sid.clone(), rig.sessions.add(&sid))
        })
        .collect();
    let limits = WriterLimits {
        retired_max: 2,
        ..WriterLimits::default()
    };
    let mut w = rig.writer_as("p1-a1", limits);
    w.pass();
    rig.fs.fault(Some(Fault::Always(libc::ENOSPC)));
    for (sid, tl) in &tls {
        for i in 0..3 {
            mark(tl, 1 + i);
        }
        close_session(tl, 5);
        rig.sessions.remove(sid);
    }
    w.pass();
    assert_eq!(rig.report.dropped.load(Ordering::Relaxed), 4);
    rig.fs.fault(None);
    rig.clock.advance(BACKOFF_MAX);
    for _ in 0..10 {
        w.pass();
        if !w.owed {
            break;
        }
    }
    assert!(w.tracks.is_empty(), "every track let go once said");
    for (sid, _) in &tls {
        let v = rig.view(sid);
        let read = v.log.read(Cursor::new(v.log.ep(), 0), usize::MAX);
        let closed = !v.log.is_open(Node::SESSION);
        let lost = read.gap.map_or(0, |g| g.lost);
        assert!(closed || lost == 4, "{sid}: closed={closed} lost={lost}");
    }
    let dropped: Vec<String> = rig
        .frames()
        .iter()
        .filter_map(|f| match f {
            Frame::Gap {
                sid,
                why: LossWhy::Dropped,
                ..
            } => Some(sid.as_str().to_string()),
            _ => None,
        })
        .collect();
    assert_eq!(dropped.len(), 4, "{dropped:?}");
}

/// A BURST BETWEEN TWO PASSES, smaller than the ring, writes no `#gap
/// why=evicted`: the ring evicts what the writer already holds first.
#[test]
fn a_burst_between_passes_writes_no_evicted_gap() {
    let rig = Rig::new();
    let tl = rig.sessions.add("s-burst");
    with_log(&tl, |log| {
        *log = Log::with_limits(1, Limits::new(64, 1 << 20).expect("a ring"));
        log.open_session(
            Kind::Spawn,
            Src::Aterm,
            Conf::Exact,
            Actor::Nobody,
            Fields::new(),
            at(0),
        )
        .expect("opens");
    });
    let run = |ms: u64| {
        let r = cmd(&tl, ms);
        with_log(&tl, |log| {
            log.close(
                r,
                Close::new(Out::Run(RunOut::Ok), Src::Shell, Conf::Exact),
                at(ms + 1),
            )
            .expect("closes");
        });
    };
    for i in 0..30 {
        run(2 + 2 * i);
    }
    let mut w = rig.writer();
    w.pass();
    for i in 0..25 {
        run(200 + 2 * i);
    }
    w.pass();
    let gaps = rig
        .frames()
        .iter()
        .filter(|f| matches!(f, Frame::Gap { .. }))
        .count();
    assert_eq!(gaps, 0, "nothing the writer could take was evicted");
}

/// VERSION SKEW: a segment whose first line names another format is opaque,
/// even when that line does not decode (a field this build does not know).
#[test]
fn a_segment_of_another_format_is_opaque_however_its_header_reads() {
    let rig = Rig::new();
    let tl = rig.sessions.add("s-skew");
    let mut w = rig.writer();
    w.pass();
    mark(&tl, 1);
    w.pass();
    let newer = b"s1\t*\t#seg\tv=2\tsha256=ab12\tend=\ns1\ts-skew\t#snap\thead=0\tep=1\tbase=1\tfirst=1\topen=0\tt=1\tend=\ns1\ts-skew\t997 mark - human src=input conf=exact t=1\tend=\n";
    rig.fs.put("p4-ee.1.slog", newer, false, false);
    let v = rig.view("s-skew");
    assert!(
        all(&v.log).iter().all(|r| r.seq < 997),
        "the newer segment unread"
    );
    assert_eq!(
        aterm_slog::journal::segment_version(b"s1\t*\t#seg\tv=2\tsha256=ab12\tend="),
        Some(2)
    );
}

/// THE READER'S INDEX LOCK IS A LEAF: never held while a chain's lock is
/// probed (a slow or hung filesystem stalls only its own read).
#[test]
fn the_reader_probes_with_its_index_lock_free() {
    struct Spy<'a> {
        inner: MemFs,
        index: &'a Mutex<Index>,
        probes: AtomicU64,
        held: AtomicBool,
    }
    impl JournalFs for Spy<'_> {
        fn lock_instance(&self, i: &Inst) -> std::io::Result<Box<dyn super::fs::Held>> {
            self.inner.lock_instance(i)
        }
        fn unlock_instance(&self, i: &Inst, h: Box<dyn super::fs::Held>) {
            self.inner.unlock_instance(i, h);
        }
        fn create_segment(&self, n: &str) -> std::io::Result<Box<dyn super::fs::Appender>> {
            self.inner.create_segment(n)
        }
        fn scan(&self, max: usize, visit: &mut dyn FnMut(&str)) -> std::io::Result<bool> {
            self.inner.scan(max, visit)
        }
        fn meta(&self, n: &str) -> std::io::Result<Meta> {
            self.inner.meta(n)
        }
        fn read(&self, n: &str, from: u64, max: u64) -> std::io::Result<(Vec<u8>, Meta)> {
            self.inner.read(n, from, max)
        }
        fn remove(&self, n: &str, m: Meta) -> std::io::Result<()> {
            self.inner.remove(n, m)
        }
        fn probe(&self, n: &str) -> aterm_slog::journal::Liveness {
            self.probes.fetch_add(1, Ordering::Relaxed);
            if self.index.try_lock().is_err() {
                self.held.store(true, Ordering::Relaxed);
            }
            self.inner.probe(n)
        }
        fn try_prune_lock(&self) -> std::io::Result<Option<Box<dyn super::fs::Held>>> {
            self.inner.try_prune_lock()
        }
        fn sync_dir(&self) -> std::io::Result<()> {
            Ok(())
        }
        fn free_bytes(&self) -> Option<u64> {
            None
        }
    }
    let rig = Rig::new();
    let tl = rig.sessions.add("s-spy");
    let _ = cmd(&tl, 1);
    let mut w = rig.writer();
    w.pass();
    drop(w);
    let index = Mutex::new(Index::default());
    let spy = Spy {
        inner: rig.fs.clone(),
        index: &index,
        probes: AtomicU64::new(0),
        held: AtomicBool::new(false),
    };
    let sid = ShortId::new("s-spy").expect("sid");
    assert!(matches!(
        view_with(&spy, &index, &sid, Budgets::READER),
        JournalRead::View(_)
    ));
    assert!(spy.probes.load(Ordering::Relaxed) > 0, "a probe ran");
    assert!(!spy.held.load(Ordering::Relaxed), "the index lock was free");
    // NEGATIVE CONTROL: the spy sees a held lock.
    let _guard = index.lock().expect("index");
    let _ = spy.probe("p1-a1.lock");
    assert!(spy.held.load(Ordering::Relaxed));
}

/// A QUIT WHOSE DRAIN OUTLASTS ITS PASSES ENDS NOTHING: every
/// session is journaled and snapped, then each gains a backlog, more
/// sessions than [`FINISH_PASSES`] passes reach (one session a pass). The
/// quit writes no closes and no `#fin`, so each session reads as a crash
/// after what its chain holds, none with a range said pruned that was only
/// never written. CONTROL: the same backlog within the passes drains, ends
/// the chain with `#fin` and closes every session `out=app-quit` over all
/// its records.
#[test]
fn a_quit_whose_drain_outlasts_its_passes_reads_as_a_crash() {
    let run = |sessions: usize| -> (Rig, Vec<String>) {
        let rig = Rig::new();
        let names: Vec<String> = (0..sessions).map(|i| format!("s-q{i:04}")).collect();
        let tls: Vec<Timeline> = names.iter().map(|n| rig.sessions.add(n)).collect();
        let limits = WriterLimits {
            pass_bytes: 1,
            ..WriterLimits::default()
        };
        let mut w = rig.writer_as("p1-a1", limits);
        // Every session snapped with its open, one a pass.
        for _ in 0..=sessions {
            w.pass();
        }
        assert!(!w.owed, "all journaled");
        for tl in &tls {
            for i in 0..3 {
                mark(tl, 1 + i);
            }
        }
        // The deadline never passes: only the pass cap can cut the drain.
        w.finish(rig.clock.mono() + HANG);
        drop(w);
        (rig, names)
    };
    let fin = |rig: &Rig| rig.frames().iter().any(|f| matches!(f, Frame::End { .. }));
    let pruned = |v: &View| v.log.holes_why().any(|h| h.2 == GapWhy::Pruned);
    let (rig, names) = run(FINISH_PASSES + 16);
    assert!(!fin(&rig), "an unfinished drain ends nothing");
    for name in &names {
        let v = rig.view(name);
        assert!(v.crashed, "{name} reads as a crash");
        assert!(!pruned(&v), "{name}: nothing said pruned");
        assert!(
            all(&v.log).iter().all(|r| r.out() != Some("app-quit")),
            "{name}: no quit close"
        );
    }
    // CONTROL.
    let (rig, names) = run(FINISH_PASSES / 2);
    assert!(fin(&rig));
    for name in &names {
        let v = rig.view(name);
        assert!(!v.crashed && !pruned(&v), "{name}");
        let recs = all(&v.log);
        assert_eq!(recs.len(), 5, "{name}: open, three marks, the close");
        assert_eq!(recs.last().and_then(|r| r.out()), Some("app-quit"));
    }
}

/// A QUIT NEVER ENDS A CHAIN OVER A SESSION BEHIND ITS LOG: a
/// session busy at the drain's last pass, or one a producer appended to
/// after it, is behind its head, so no closes and no `#fin` are written
/// while it is; once the drain catches it up they are.
#[test]
fn a_quit_never_ends_a_chain_over_a_session_behind_its_log() {
    let rig = Rig::new();
    let tl = rig.sessions.add("s-behind");
    let mut w = rig.writer();
    w.pass();
    mark(&tl, 1);
    assert!(w.quit_lines().is_none(), "a record past the cursor");
    w.pass();
    assert!(w.quit_lines().is_some(), "caught up");
    let guard = tl.lock().expect("tl");
    assert!(w.quit_lines().is_none(), "busy");
    drop(guard);
    w.finish(rig.clock.mono() + QUIT_BUDGET);
    drop(w);
    let v = rig.view("s-behind");
    assert!(!v.crashed);
    assert_eq!(all(&v.log).len(), 3, "open, the mark, the quit close");
}

/// A COLD READ DECODES WHAT A WARM ONE DOES: the index refresh
/// reads the whole journal but decodes none of it, so the first read after
/// launch decodes the newest segments holding the session within the decode
/// budget, exactly as a later one does, and answers the same.
#[test]
fn a_cold_read_decodes_no_more_than_a_warm_one() {
    let rig = Rig::new();
    let tl = rig.sessions.add("s-cold");
    let limits = WriterLimits {
        segment_bytes: 4096,
        ..WriterLimits::default()
    };
    let mut w = rig.writer_as("p1-a1", limits);
    for i in 0..800 {
        mark(&tl, 1 + i);
        if i % 20 == 0 {
            w.pass();
        }
    }
    w.pass();
    drop(w);
    let segs: Vec<u64> = rig
        .fs
        .names()
        .iter()
        .filter(|n| n.ends_with(".slog"))
        .map(|n| rig.fs.bytes(n).len() as u64)
        .collect();
    assert!(segs.len() >= 10, "{segs:?}");
    let biggest = segs.iter().copied().max().unwrap_or(0);
    let b = Budgets {
        decode: 4096,
        min_segments: 2,
        ..Budgets::READER
    };
    let bound = 2 * biggest + b.decode;
    let sid = ShortId::new("s-cold").expect("sid");
    let index = Mutex::new(Index::default());
    let read = |cost: &mut super::read::ReadCost| match super::read::view_costed(
        &rig.fs, &index, &sid, b, cost,
    ) {
        JournalRead::View(v) => all(&v.log),
        other => panic!("found: {other:?}"),
    };
    let mut cold = super::read::ReadCost::default();
    let cold_recs = read(&mut cold);
    let mut warm = super::read::ReadCost::default();
    let warm_recs = read(&mut warm);
    assert_eq!(cold_recs, warm_recs);
    assert_eq!(cold.decoded, warm.decoded, "{cold:?} {warm:?}");
    assert!(cold.decoded <= bound, "{cold:?} past {bound}");
    assert!(
        cold.indexed >= segs.iter().sum::<u64>(),
        "the refresh read it all"
    );
    assert_eq!(warm.indexed, 0, "nothing new to index");
    // CONTROL: the counter counts; a whole budget decodes every segment.
    let mut whole = super::read::ReadCost::default();
    let _ = super::read::view_costed(&rig.fs, &index, &sid, Budgets::READER, &mut whole);
    assert!(whole.decoded > bound, "{whole:?}");
}

/// A complete malformed trailer cannot suppress the actual reader's crash close.
#[test]
fn a_malformed_complete_fin_does_not_hide_a_dead_chains_crash() {
    let clean = Rig::new();
    let tl = clean.sessions.add("s-fin");
    let run = cmd(&tl, 1);
    let mut w = clean.writer();
    w.pass();
    w.finish(clean.clock.mono() + QUIT_BUDGET);
    drop(w);
    assert!(matches!(clean.frames().last(), Some(Frame::End { .. })));
    let v = clean.view("s-fin");
    assert!(!v.crashed, "the writer's valid FIN ends its chain");
    assert!(!v.log.is_open(run));

    let dead = Rig::new();
    let tl = dead.sessions.add("s-fin");
    let run = cmd(&tl, 1);
    let mut w = dead.writer();
    w.pass();
    drop(w);
    let v = dead.view("s-fin");
    assert!(v.crashed, "the same reader observes the unended dead chain");
    assert!(!v.log.is_open(run));
    let segments: Vec<_> = dead
        .fs
        .names()
        .into_iter()
        .filter(|n| n.ends_with(".slog"))
        .collect();
    assert_eq!(segments.len(), 1);
    let name = &segments[0];
    let prefix = dead.fs.bytes(name);
    let kept: Vec<bool> = [
        b"s1\t*\t#fin\twhy=quit\tt=9\n".as_slice(),
        b"s1\t*\t#fin\twhy=quit\tt=9\tend=glued\n".as_slice(),
        b"s1\t*\t#fin\twhy=quit\tt=9\tend=\r\n".as_slice(),
    ]
    .into_iter()
    .map(|trailer| {
        assert!(decode_line(&trailer[..trailer.len() - 1]).is_err());
        let mut bytes = prefix.clone();
        bytes.extend_from_slice(trailer);
        dead.fs.put(name, &bytes, false, false);
        let v = dead.view("s-fin");
        v.crashed && !v.log.is_open(run)
    })
    .collect();
    assert_eq!(
        kept,
        vec![true; 3],
        "all malformed complete FIN lines retain crash reporting"
    );
}

fn actual_store_writer(
    rig: &Rig,
    store: &crate::session_store::Store,
    limits: WriterLimits,
) -> Writer<crate::session_store::Store> {
    let mut writer = Writer::new(
        Arc::new(rig.fs.clone()),
        Arc::clone(store),
        inst("p1-a1"),
        Arc::clone(&rig.clock) as Arc<dyn Clock>,
        limits,
        Arc::clone(&rig.report),
    );
    writer.start();
    writer
}

/// Close through the actual registry, then let go of every fixture timeline Arc.
fn actual_store_retire(
    store: &crate::session_store::Store,
    local: u64,
) -> (
    String,
    std::sync::Weak<Mutex<SessionTimeline>>,
    Vec<aterm_slog::Rec>,
) {
    let handle = crate::session_store::test_handle(local);
    let sid = handle.sid.as_str().to_string();
    let timeline = Arc::clone(&handle.ctx.timeline);
    let weak = Arc::downgrade(&timeline);
    store
        .write()
        .unwrap_or_else(|p| p.into_inner())
        .register(handle);
    let closed = store
        .write()
        .unwrap_or_else(|p| p.into_inner())
        .deregister_local_as(
            local,
            crate::session_store::ExitReason::CtlClose,
            crate::session_store::ExitActor::Human,
        );
    assert_eq!(closed.as_ref().map(|s| s.as_str()), Some(sid.as_str()));
    let expected = with_log(&timeline, |log| all(log).iter().map(host).collect());
    drop(timeline);
    (sid, weak, expected)
}

#[test]
fn the_real_store_bounds_unseen_retirements_and_says_the_overflow() {
    let rig = Rig::new();
    let store = crate::session_store::new_store();
    // Shrink the actual staging bounds; Writer setup must not reset this arm.
    store
        .write()
        .unwrap_or_else(|p| p.into_inner())
        .arm_session_journal(None, 1, 1);
    let mut writer = actual_store_writer(
        &rig,
        &store,
        WriterLimits {
            retired_max: 1,
            ..WriterLimits::default()
        },
    );
    let (whole, whole_weak, expected) = actual_store_retire(&store, 1);
    let (forgotten, forgotten_weak, _) = actual_store_retire(&store, 2);
    let (dropped, dropped_weak, dropped_expected) = actual_store_retire(&store, 3);
    assert!(
        whole_weak.upgrade().is_some(),
        "one full timeline is retained"
    );
    assert!(forgotten_weak.upgrade().is_none());
    assert!(
        dropped_weak.upgrade().is_none(),
        "compact handoff holds no timeline Arc"
    );
    writer.pass();
    writer.pass();
    let frames = rig.frames();
    assert_eq!(recs_of(&frames, &whole), expected);
    assert!(recs_of(&frames, &forgotten).is_empty());
    assert!(frames.iter().any(|frame| matches!(frame,
        Frame::Gap { sid, why: LossWhy::Dropped, lost, .. }
            if sid.as_str() == dropped && *lost == dropped_expected.len() as u64
    )));
    assert_eq!(rig.report.dropped.load(Ordering::Relaxed), 1);
    assert_eq!(rig.report.forgotten.load(Ordering::Relaxed), 1);
    assert!(writer.tracks.is_empty());
    assert!(
        whole_weak.upgrade().is_none(),
        "completed retirements are released"
    );
}

#[test]
fn a_compact_store_handoff_keeps_the_existing_writer_cursor() {
    let rig = Rig::new();
    let store = crate::session_store::new_store();
    store
        .write()
        .unwrap_or_else(|p| p.into_inner())
        .arm_session_journal(None, 1, 1);
    let mut writer = actual_store_writer(
        &rig,
        &store,
        WriterLimits {
            retired_max: 1,
            ..WriterLimits::default()
        },
    );
    let handle = crate::session_store::test_handle(1);
    let observed = handle.sid.as_str().to_string();
    let timeline = Arc::clone(&handle.ctx.timeline);
    store
        .write()
        .unwrap_or_else(|p| p.into_inner())
        .register(handle);
    writer.pass();
    let (unseen, _, unseen_expected) = actual_store_retire(&store, 2);
    store
        .write()
        .unwrap_or_else(|p| p.into_inner())
        .deregister_local_as(
            1,
            crate::session_store::ExitReason::CtlClose,
            crate::session_store::ExitActor::Human,
        );
    let observed_expected = with_log(&timeline, |log| {
        all(log).iter().map(host).collect::<Vec<_>>()
    });
    drop(timeline);
    writer.pass();
    writer.pass();
    let frames = rig.frames();
    assert_eq!(
        recs_of(&frames, &observed),
        observed_expected,
        "no replay, duplicate or lost close"
    );
    assert_eq!(recs_of(&frames, &unseen), unseen_expected);
    assert!(!frames.iter().any(
        |f| matches!(f, Frame::Gap { sid, why: LossWhy::Dropped, .. }
        if sid.as_str() == observed)
    ));
    assert_eq!(
        rig.report.dropped.load(Ordering::Relaxed),
        0,
        "the stronger existing Arc wins"
    );
    assert!(writer.tracks.is_empty());
}

#[test]
fn the_store_handoff_is_inert_off_and_releases_only_its_current_owner() {
    let rig = Rig::new();
    let store = crate::session_store::new_store();
    let (_, off, _) = actual_store_retire(&store, 1);
    assert!(off.upgrade().is_none(), "an unarmed store retains nothing");
    let old_guard = RetirementLease::arm(Arc::clone(&store), aterm_slog::Notify::new(|| {}));
    let mut old_writer = actual_store_writer(&rig, &store, WriterLimits::default());
    let (_, stopped, _) = actual_store_retire(&store, 2);
    assert!(stopped.upgrade().is_some());
    old_writer.stop(Cause::Io);
    assert!(
        stopped.upgrade().is_none(),
        "stop detaches all pending timelines"
    );
    // Stop deliberately keeps the old instance lock until Writer drops.
    // The separate old guard remains, so its late-drop ownership is tested.
    drop(old_writer);
    // A stopped instance leaves its lock as a crash marker. The replacement is a
    // new instance, and must actually start before this tests retirement ownership.
    let mut new_writer = Writer::new(
        Arc::new(rig.fs.clone()),
        Arc::clone(&store),
        inst("p2-a2"),
        Arc::clone(&rig.clock) as Arc<dyn Clock>,
        WriterLimits::default(),
        Arc::clone(&rig.report),
    );
    new_writer.start();
    assert!(
        !new_writer.is_stopped(),
        "the replacement writer actually started"
    );
    let (sid, current, expected) = actual_store_retire(&store, 3);
    assert!(
        current.upgrade().is_some(),
        "the new writer owns the retirement before the old guard drops"
    );
    drop(old_guard);
    assert!(
        current.upgrade().is_some(),
        "an old guard cannot disarm the new writer"
    );
    new_writer.pass();
    new_writer.pass();
    assert_eq!(recs_of(&rig.frames(), &sid), expected);
    assert!(current.upgrade().is_none());
    drop(new_writer);
    let guard = RetirementLease::arm(Arc::clone(&store), aterm_slog::Notify::new(|| {}));
    let (_, unwritten, _) = actual_store_retire(&store, 4);
    assert!(unwritten.upgrade().is_some());
    drop(guard);
    assert!(
        unwritten.upgrade().is_none(),
        "the unstarted/armed-exit guard drains without a writer"
    );
}

#[test]
fn pending_store_retirements_fence_the_final_markers_and_busy_snapshots_retry() {
    for handing in [false, true] {
        let rig = Rig::new();
        let store = crate::session_store::new_store();
        let mut writer = actual_store_writer(&rig, &store, WriterLimits::default());
        writer.pass();
        let (sid, _, expected) = actual_store_retire(&store, 1);
        assert!(
            writer.quit_lines().is_none(),
            "pending unseen retirement fences quit"
        );
        if handing {
            writer.begin_handing();
            writer.maybe_hand_off();
            assert!(
                !writer.handed_done,
                "pending unseen retirement fences handoff"
            );
        }
        let busy = store.write().unwrap_or_else(|p| p.into_inner());
        writer.pass();
        assert!(writer.owed, "busy snapshot owes another pass");
        assert!(!rig.frames().iter().any(|f| matches!(f, Frame::End { .. })));
        drop(busy);
        writer.pass();
        if !handing {
            writer.finish(rig.clock.mono() + QUIT_BUDGET);
        }
        assert_eq!(recs_of(&rig.frames(), &sid), expected);
        let why = if handing {
            EndWhy::Handoff
        } else {
            EndWhy::Quit
        };
        assert!(
            rig.frames()
                .iter()
                .any(|f| matches!(f, Frame::End { why: w, .. } if *w == why))
        );
    }
}

#[test]
fn queuing_a_retirement_wakes_even_when_its_close_is_already_recorded() {
    let rig = Rig::new();
    let store = crate::session_store::new_store();
    let rings = Arc::new(AtomicU64::new(0));
    let counted = Arc::clone(&rings);
    let guard = RetirementLease::arm(
        Arc::clone(&store),
        aterm_slog::Notify::new(move || {
            counted.fetch_add(1, Ordering::Relaxed);
        }),
    );
    let mut writer = actual_store_writer(&rig, &store, WriterLimits::default());
    let handle = crate::session_store::test_handle(1);
    let timeline = Arc::clone(&handle.ctx.timeline);
    store
        .write()
        .unwrap_or_else(|p| p.into_inner())
        .register(handle);
    {
        let mut tl = timeline.lock().unwrap_or_else(|p| p.into_inner());
        let at = tl.slog().now();
        tl.slog_mut().close_session(
            crate::session_log::exit_out(crate::session_store::ExitReason::CtlClose),
            crate::session_log::exit_actor(&crate::session_store::ExitActor::Human),
            "-",
            None,
            at,
        );
    }
    writer.pass();
    rings.store(0, Ordering::Relaxed);
    store
        .write()
        .unwrap_or_else(|p| p.into_inner())
        .deregister_local_as(
            1,
            crate::session_store::ExitReason::CtlClose,
            crate::session_store::ExitActor::Human,
        );
    assert!(
        rings.load(Ordering::Relaxed) > 0,
        "the queue owns its own wake"
    );
    drop(timeline);
    writer.pass();
    assert!(writer.tracks.is_empty());
    drop(guard);
}

/// The real journal's flushes are only ordered (aterm-durable's counting seam): a
/// segment's sync (rotation, handoff and quit `#fin`), the instance lock's entry and
/// the directory flush the rotation, the quit and retention take. A reader drops a
/// torn tail and reads a missing `#fin` as a crash, segments are created exclusively
/// and retention runs again at start, and liveness is a flock no reboot keeps.
#[cfg(unix)]
#[test]
fn the_real_journals_flushes_are_only_ordered() {
    use aterm_durable::{Flush, seam::Recording};
    let scratch_guard = tempdir("flush-tiers");
    let scratch = scratch_guard.path();
    let logs = scratch.join("logs");
    std::fs::create_dir_all(&logs).expect("logs");
    let dir = super::fs::PinnedJournalFs::open(&logs, true)
        .expect("opens")
        .expect("made");
    let recording = Recording::start();
    let mut seg = dir.create_segment("p1-aa.1.slog").expect("a segment");
    assert_eq!(seg.write(b"x\n").expect("writes"), 2);
    seg.sync().expect("syncs");
    assert_eq!(recording.take(), [Flush::Ordered], "a segment's sync");
    let held = dir.lock_instance(&inst("p1-aa")).expect("locks");
    assert_eq!(
        recording.take(),
        [Flush::Ordered],
        "the instance lock's entry"
    );
    dir.sync_dir().expect("syncs");
    assert_eq!(
        recording.take(),
        [Flush::Ordered],
        "the directory's entries"
    );
    drop(held);
}
