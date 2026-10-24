// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The GUI book's tests (the occupancy slice-2 plan's 2.5): every proof, refusal, park, answer,
//! level, K raise and carry on a scripted rig (virtual time, a fake process tree, a scripted
//! machine snapshot), each guard with its negative control; one pid level on a real stand-in.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicI64, AtomicU64, Ordering};

use aterm_agent::supervise::occupancy::fault::{self as agent_fault, Fault as AgentFault};
use aterm_agent::supervise::occupancy::wakes_seen::WakeDoor;
use aterm_vitals::model::SlotsView;

use super::*;
use crate::occupancy_host::Pace;
use crate::occupancy_host::fault::{self as gui_fault, Fault as GuiFault};

const T0: i64 = 1_760_000_000_000;
const A: &str = "s-0000000000000000000a";
const B: &str = "s-0000000000000000000b";
const C: &str = "s-0000000000000000000c";

/// A process tree the test edits: pid → (birth s, parent).
struct Tree(Mutex<BTreeMap<u32, ProcRead>>);

impl Tree {
    /// launchd, a login, and three tabs: each a shell (the PTY child), its agent and an `aterm
    /// ctl` under the agent. Tab A's agent also runs a job (500) with a child (550), and its shell
    /// a second child that is not the agent (450).
    fn machine() -> Self {
        let links = [
            (1, 1, 0),
            (100, 10, 1),
            (200, 20, 100),
            (300, 30, 200),
            (400, 40, 300),
            (450, 45, 200),
            (500, 50, 300),
            (550, 55, 500),
            (600, 60, 100),
            (700, 70, 600),
            (800, 80, 700),
            (900, 90, 100),
            (910, 91, 900),
            (920, 92, 910),
        ];
        Self(Mutex::new(
            links
                .into_iter()
                .map(|(pid, birth_s, ppid)| (pid, ProcRead::Alive(link(pid, birth_s, ppid))))
                .collect(),
        ))
    }

    fn kill(&self, pid: u32) {
        self.0
            .lock()
            .unwrap()
            .insert(pid, ProcRead::from_record(Ok(None)));
    }

    fn reborn(&self, pid: u32, birth_s: i64, ppid: u32) {
        self.0
            .lock()
            .unwrap()
            .insert(pid, ProcRead::Alive(link(pid, birth_s, ppid)));
    }

    fn unread(&self, pid: u32) {
        self.0.lock().unwrap().insert(
            pid,
            ProcRead::from_record(Err(std::io::ErrorKind::PermissionDenied.into())),
        );
    }
}

impl ProcTree for Tree {
    fn read(&self, pid: u32) -> ProcRead {
        self.0
            .lock()
            .unwrap()
            .get(&pid)
            .copied()
            .unwrap_or(ProcRead::Exited)
    }
}

fn link(pid: u32, birth_s: i64, ppid: u32) -> ProcLink {
    ProcLink {
        pid,
        birth_us: birth_s * 1_000_000,
        ppid,
    }
}

/// THE RIG: a scripted clock, mode, tree, machine snapshot, pace and loop words.
struct Rig {
    now: AtomicI64,
    mode: Mutex<Occupancy>,
    /// The modes the next reads see before [`Self::mode`]'s, oldest first: a verb's read made
    /// before a switch it then races.
    stale: Mutex<Vec<Occupancy>>,
    tree: Tree,
    kernel: bool,
    sys: Mutex<Option<SysRead>>,
    at_ns: AtomicU64,
    pace: Mutex<Pace>,
    said: Mutex<BTreeMap<String, Said>>,
}

impl Rig {
    fn new(mode: Occupancy) -> Self {
        Self {
            now: AtomicI64::new(T0),
            mode: Mutex::new(mode),
            stale: Mutex::new(Vec::new()),
            tree: Tree::machine(),
            kernel: false,
            sys: Mutex::new(None),
            at_ns: AtomicU64::new(1),
            pace: Mutex::new(Pace::default()),
            said: Mutex::new(BTreeMap::new()),
        }
    }

    #[cfg(target_os = "macos")]
    fn kernel(mode: Occupancy) -> Self {
        Self {
            kernel: true,
            ..Self::new(mode)
        }
    }

    fn advance(&self, ms: i64) {
        self.now.fetch_add(ms, Ordering::SeqCst);
    }

    fn set_mode(&self, mode: Occupancy) {
        *self.mode.lock().unwrap() = mode;
    }

    /// The next mode read sees `mode` (read before a switch), the reads after it the rig's own.
    fn stale_once(&self, mode: Occupancy) {
        self.stale.lock().unwrap().push(mode);
    }

    /// A fresh snapshot (a new stamp) with `n` build slots, `free` of them proven free.
    fn slots(&self, n: u16, free: u16) {
        let at_ns = self.at_ns.fetch_add(1_000_000_000, Ordering::SeqCst);
        *self.sys.lock().unwrap() = Some(SysRead {
            snap: Arc::new(Snapshot {
                at_ns,
                slots: Some(SlotsView::Read {
                    n,
                    held: n - free,
                    free,
                    pending: 0,
                    unknown: 0,
                    discounted: 0,
                }),
                ..Snapshot::default()
            }),
            fresh: true,
        });
    }

    fn hold(&self, sid: &str, since_ms: i64) {
        self.said
            .lock()
            .unwrap()
            .insert(sid.to_string(), (Some(since_ms), None));
    }
}

impl Env for Rig {
    fn now_ms(&self) -> i64 {
        self.now.load(Ordering::SeqCst)
    }

    fn local_offset_s(&self) -> i64 {
        0
    }

    fn tree(&self) -> &dyn ProcTree {
        if self.kernel { &Kernel } else { &self.tree }
    }

    fn mode(&self) -> Occupancy {
        let mut stale = self.stale.lock().unwrap();
        if stale.is_empty() {
            *self.mode.lock().unwrap()
        } else {
            stale.remove(0)
        }
    }

    fn sys(&self, _fresh: Option<Duration>) -> Option<SysRead> {
        self.sys.lock().unwrap().clone()
    }

    fn reserve(&self, sid: &str, age_ms: i64, usable: u16, now_ms: i64) -> Reserve {
        self.pace
            .lock()
            .unwrap()
            .reserve(sid, age_ms, usable, now_ms)
    }

    fn said(&self, sid: &str) -> Option<Said> {
        self.said.lock().unwrap().get(sid).copied()
    }

    fn hosted(&self, _sid: &str) -> bool {
        false
    }
}

/// A seat whose shell and agent are each a pid and a birth, seconds.
fn seat(sid: &str, local: u64, shell: (u32, i64), agent: (u32, i64)) -> Seat {
    Seat {
        sid: sid.to_string(),
        local,
        pty_child: Some(link(shell.0, shell.1, 0).id()),
        agent: Some(agent),
    }
}

/// Tab A: shell 200, agent 300; its `aterm ctl` is 400.
fn seat_a() -> Seat {
    seat(A, 1, (200, 20), (300, 30))
}

/// Tab B: shell 600, agent 700; its `aterm ctl` is 800.
fn seat_b() -> Seat {
    seat(B, 2, (600, 60), (700, 70))
}

/// Tab C: shell 900, agent 910; its `aterm ctl` is 920.
fn seat_c() -> Seat {
    seat(C, 3, (900, 90), (910, 91))
}

fn state(host: &WakeHost, sid: &str, ord: u16) -> Option<WakeState> {
    host.lock()
        .shelves
        .get(sid)
        .and_then(|shelf| shelf.book.get(ord))
        .map(|w| w.state)
}

fn declared(host: &WakeHost, sid: &str, ord: u16) -> DeclaredWake {
    host.lock()
        .shelves
        .get(sid)
        .and_then(|shelf| shelf.book.get(ord).cloned())
        .expect("declared")
}

fn parks(host: &WakeHost, sid: &str) -> Vec<u64> {
    host.lock()
        .shelves
        .get(sid)
        .map(|shelf| shelf.parks.parked().iter().map(|p| p.conn).collect())
        .unwrap_or_default()
}

fn denied_words(refusal: DeclarerRefusal) -> String {
    format!("ERR denied wake: {}\n", refusal.words())
}

/// EACH REFUSAL (S1): a peer of another session's tree, no kernel peer (a relay), a peer of the
/// tab's but not of its agent's, and the owner's `off`, which refuses every verb and books
/// nothing; a `pid:` atom outside the tree is refused too. NEGATIVE CONTROL: a descendant of the
/// agent (its job's child) is admitted, its wake declared for the agent itself.
#[test]
fn each_refusal_of_a_peer_outside_the_tree_and_a_descendant_admitted() {
    let rig = Rig::new(Occupancy::Observe);
    let host = WakeHost::default();
    let a = seat_a();
    let decl = "label=build when=in:10m";
    assert_eq!(
        host.wake(&rig, &a, Some(800), decl),
        denied_words(DeclarerRefusal::OtherSession)
    );
    assert_eq!(
        host.wake(&rig, &a, None, decl),
        "ERR denied wake: no kernel peer\n"
    );
    assert_eq!(
        host.wake(&rig, &a, Some(450), decl),
        "ERR denied wake: not in this session's tree\n",
        "the tab's shell's other child is not of its agent"
    );
    assert_eq!(
        host.wake(&rig, &a, Some(400), "label=x when=pid:800 exited"),
        "ERR denied wake: not in this session's tree\n",
        "a pid of another tab's"
    );
    rig.set_mode(Occupancy::Off);
    assert_eq!(host.wake(&rig, &a, Some(400), decl), OFF);
    assert_eq!(host.waits(&rig, &a, Some(400), ""), OFF);
    assert_eq!(host.park(&rig, &a, Some(400), 1), Err(OFF.to_string()));
    assert!(host.lock().shelves.is_empty(), "no refusal books anything");

    rig.set_mode(Occupancy::Observe);
    assert_eq!(
        host.wake(&rig, &a, Some(550), decl),
        "OK wake #1 await_only=1\n"
    );
    assert_eq!(
        declared(&host, A, 1).agent,
        AgentId {
            pid: 300,
            birth_us: 30_000_000
        },
        "declared for the agent, never the short-lived peer"
    );
    assert_eq!(
        host.wake(&rig, &a, Some(400), "label=job when=pid:500 exited"),
        "OK wake #2\n",
        "a pid of the agent's own job"
    );
}

/// `await wake`, `wake taken` and `wake rm` FROM OUTSIDE THE TREE (a manager-shaped peer of
/// another tab's) are refused, and the ready wake stays ready. NEGATIVE CONTROL
/// `AwaitFromAnyone`: the same peer's await consumes it, and its `rm` removes it.
#[test]
fn an_outside_await_take_or_rm_is_refused_and_the_wake_stays_ready() {
    let rig = Rig::new(Occupancy::Observe);
    let host = WakeHost::default();
    let a = seat_a();
    assert_eq!(
        host.wake(&rig, &a, Some(400), "label=t when=in:1m"),
        "OK wake #1 await_only=1\n"
    );
    rig.advance(60_000);
    host.tick(&rig, false);
    assert_eq!(state(&host, A, 1), Some(WakeState::Ready));
    let other = denied_words(DeclarerRefusal::OtherSession);
    assert_eq!(host.park(&rig, &a, Some(800), 7), Err(other.clone()));
    assert_eq!(host.wake(&rig, &a, Some(800), "taken #1"), other);
    assert_eq!(host.wake(&rig, &a, Some(800), "rm #1"), other);
    assert_eq!(state(&host, A, 1), Some(WakeState::Ready));
    assert!(parks(&host, A).is_empty());

    let consumed = agent_fault::with(AgentFault::AwaitFromAnyone, || {
        host.park(&rig, &a, Some(800), 7)
            .map(|()| host.pass(&rig, A, 7))
    });
    assert_eq!(
        consumed,
        Ok(Pass::Reply {
            reply: format!("OK wake #1 ready since={}\n", T0 + 60_000),
            ord: Some(1),
        }),
        "NEGATIVE CONTROL: a proof that admits anyone hands the wake to another tab"
    );
    assert!(matches!(
        state(&host, A, 1),
        Some(WakeState::Answering { .. })
    ));
    let removed = agent_fault::with(AgentFault::AwaitFromAnyone, || {
        host.wake(&rig, &a, Some(800), "rm #1")
    });
    assert_eq!(removed, "OK wake #1 rm\n");
}

/// ONE PARK A SESSION: a second gives `ERR busy`; a park from outside the tree is refused and
/// never takes the session's slot; with nothing open, the D4 usage answer.
#[test]
fn a_second_park_is_busy_and_an_outside_park_never_takes_the_slot() {
    let rig = Rig::new(Occupancy::Observe);
    let host = WakeHost::default();
    let a = seat_a();
    assert_eq!(
        host.park(&rig, &a, Some(400), 1),
        Err("ERR usage await wake: no wake pending\n".to_string())
    );
    host.wake(&rig, &a, Some(400), "label=t when=in:1h");
    assert_eq!(host.park(&rig, &a, Some(400), 1), Ok(()));
    assert_eq!(
        host.park(&rig, &a, Some(400), 2),
        Err("ERR busy await wake parked\n".to_string())
    );
    assert_eq!(
        host.park(&rig, &a, Some(800), 3),
        Err(denied_words(DeclarerRefusal::OtherSession))
    );
    assert_eq!(parks(&host, A), [1]);
    host.unpark(A, 1);
    assert_eq!(
        host.park(&rig, &a, Some(400), 3),
        Ok(()),
        "the session's slot is its own again"
    );
}

fn answered_once(host: &WakeHost, rig: &Rig, a: &Seat, conn: u64) -> Option<u16> {
    host.park(rig, a, Some(400), conn).expect("parks");
    match host.pass(rig, &a.sid, conn) {
        Pass::Reply { ord, .. } => ord,
        Pass::Wait => None,
    }
}

/// A REPLY THAT IS NOT DELIVERED LEAVES THE WAKE READY (S2): an answer written fails, or its
/// reply's latch is released unwritten (the peer closed), and the wake is ready for the next
/// wait; one delivered is told. NEGATIVE CONTROL `ToldBeforeWrite`: told on a failed write.
#[test]
fn a_reply_socket_closed_before_the_answer_leaves_the_wake_ready() {
    let rig = Arc::new(Rig::new(Occupancy::Observe));
    let host = Arc::new(WakeHost::default());
    let a = seat_a();
    host.wake(&*rig, &a, Some(400), "label=t when=in:1m");
    rig.advance(60_000);
    assert_eq!(answered_once(&host, &rig, &a, 1), Some(1));
    host.answered(&*rig, A, 1, false);
    assert_eq!(state(&host, A, 1), Some(WakeState::Ready));

    assert_eq!(answered_once(&host, &rig, &a, 2), Some(1));
    let (wire, written) = crate::control::ReplyWire::new();
    hang(
        Arc::clone(&host),
        Arc::clone(&rig) as Arc<dyn Env>,
        &wire,
        A.to_string(),
        1,
    );
    drop(written);
    assert_eq!(
        state(&host, A, 1),
        Some(WakeState::Ready),
        "released unwritten: ready again"
    );

    assert_eq!(answered_once(&host, &rig, &a, 3), Some(1));
    let (wire, written) = crate::control::ReplyWire::new();
    hang(
        Arc::clone(&host),
        Arc::clone(&rig) as Arc<dyn Env>,
        &wire,
        A.to_string(),
        1,
    );
    written.delivered();
    drop(written);
    assert_eq!(
        state(&host, A, 1),
        Some(WakeState::Told {
            door: WakeDoor::Await,
            at_ms: T0 + 60_000
        }),
        "delivered, in a session no loop of this build reads: told"
    );

    let host = WakeHost::default();
    host.wake(&*rig, &a, Some(400), "label=t when=in:1m");
    rig.advance(60_000);
    assert_eq!(answered_once(&host, &rig, &a, 1), Some(1));
    agent_fault::with(AgentFault::ToldBeforeWrite, || {
        host.answered(&*rig, A, 1, false);
    });
    assert!(
        matches!(state(&host, A, 1), Some(WakeState::Told { .. })),
        "NEGATIVE CONTROL: told before its write, a failed write loses the wake"
    );
}

/// `in:` ALONE IS AWAIT-ONLY (S13): declared so, unanswered before its time, answered by the
/// await at it; a wake with a source atom beside it is not.
#[test]
fn a_time_alone_wake_is_await_only_and_answered_by_await() {
    let rig = Rig::new(Occupancy::Observe);
    let host = WakeHost::default();
    let a = seat_a();
    assert_eq!(
        host.wake(&rig, &a, Some(400), "label=lunch when=in:30s"),
        "OK wake #1 await_only=1\n"
    );
    assert!(declared(&host, A, 1).await_only);
    assert_eq!(
        host.wake(
            &rig,
            &a,
            Some(400),
            "label=job when=in:30s and pid:500 exited"
        ),
        "OK wake #2\n"
    );
    assert!(!declared(&host, A, 2).await_only);
    host.park(&rig, &a, Some(400), 1).expect("parks");
    assert_eq!(host.pass(&rig, A, 1), Pass::Wait);
    rig.advance(30_000);
    assert_eq!(
        host.pass(&rig, A, 1),
        Pass::Reply {
            reply: format!("OK wake #1 ready since={}\n", T0 + 30_000),
            ord: Some(1),
        }
    );
}

/// `need=slots>=1` UNDER OBSERVE (S9): answered once the scripted snapshot shows a free slot,
/// with the slots it saw. NEGATIVE CONTROL `PaceOnAwait`: asked of the typed lines' pace, which
/// grants nothing under observe, it never answers.
#[test]
fn a_slots_need_under_observe_is_answered_when_a_slot_frees() {
    let run = || {
        let rig = Rig::new(Occupancy::Observe);
        let host = WakeHost::default();
        let a = seat_a();
        rig.slots(2, 0);
        assert_eq!(
            host.wake(&rig, &a, Some(400), "label=build when=in:1s need=slots>=1"),
            "OK wake #1 await_only=1\n"
        );
        rig.advance(1_000);
        host.park(&rig, &a, Some(400), 1).expect("parks");
        let none_free = host.pass(&rig, A, 1);
        rig.slots(2, 1);
        (none_free, host.pass(&rig, A, 1))
    };
    assert_eq!(
        run(),
        (
            Pass::Wait,
            Pass::Reply {
                reply: format!("OK wake #1 ready since={} slots=1/2\n", T0 + 1_000),
                ord: Some(1),
            }
        )
    );
    assert_eq!(
        gui_fault::with(GuiFault::PaceOnAwait, run),
        (Pass::Wait, Pass::Wait),
        "NEGATIVE CONTROL: through pace_on, an await under observe never answers"
    );
}

/// Three sessions' wakes, each needing a slot, parked on one free slot: the passes in the order
/// given, and the sessions answered.
fn three_on_one(order: [(&str, u64); 3]) -> (Vec<String>, Vec<String>, Vec<String>) {
    let rig = Rig::new(Occupancy::Observe);
    let host = WakeHost::default();
    let seats = [(seat_a(), 400), (seat_b(), 800), (seat_c(), 920)];
    for (age, (seat, peer)) in seats.iter().enumerate() {
        assert_eq!(
            host.wake(
                &rig,
                seat,
                Some(*peer),
                "label=build when=in:1s need=slots>=1"
            ),
            "OK wake #1 await_only=1\n"
        );
        rig.hold(
            &seat.sid,
            T0 - 3 * 3_600_000 + i64::try_from(age).unwrap() * 3_600_000,
        );
    }
    rig.advance(1_000);
    for (conn, (seat, peer)) in (1..).zip(seats.iter()) {
        host.park(&rig, seat, Some(*peer), conn).expect("parks");
    }
    let pass = |free: u16| {
        rig.slots(3, free);
        order
            .iter()
            .filter(|(sid, conn)| {
                matches!(
                    host.pass(&rig, sid, *conn),
                    Pass::Reply { ord: Some(_), .. }
                )
            })
            .map(|(sid, _)| (*sid).to_string())
            .collect::<Vec<_>>()
    };
    let none = pass(0);
    let one = pass(1);
    for (sid, _) in &order {
        host.answered(&rig, sid, 1, true);
    }
    let two = pass(2);
    (none, one, two)
}

/// THREE SESSIONS, ONE SLOT, THREE PARKED AWAITS (S9): while no slot is usable each waits in the
/// slots' line by its age; the one slot that frees is one answer, the oldest hold's, however the
/// passes fall (the youngest asks first here); the rest wait by age, so the next slot is the next
/// oldest's. NEGATIVE CONTROLS: `YoungestFirst` hands the slot to whoever asks first, and a
/// reserve bypass answers all three on the one slot.
#[test]
fn three_sessions_on_one_slot_get_one_answer_and_the_rest_wait_by_age() {
    let youngest_first = [(C, 3), (B, 2), (A, 1)];
    assert_eq!(
        three_on_one(youngest_first),
        (Vec::new(), vec![A.to_string()], vec![B.to_string()])
    );
    let (_, one, _) = gui_fault::with(GuiFault::YoungestFirst, || three_on_one(youngest_first));
    assert_eq!(
        one,
        [C.to_string()],
        "NEGATIVE CONTROL: counting no older wait, the youngest takes the slot"
    );
    let (_, one, _) = gui_fault::with(GuiFault::AwaitSkipsReserve, || three_on_one(youngest_first));
    assert_eq!(
        one.len(),
        3,
        "NEGATIVE CONTROL: with no reservation all three start builds on the one slot"
    );
}

/// The outs of each step: the declaration, the job's exit (a tick), then ten looks a minute apart.
fn k_run(rig: &Rig) -> Vec<Vec<Out>> {
    let host = WakeHost::default();
    let a = seat_a();
    assert_eq!(
        host.wake(rig, &a, Some(400), "label=job when=pid:500 exited"),
        "OK wake #1\n"
    );
    let mut steps = vec![host.take_outs()];
    rig.tree.kill(500);
    host.tick(rig, true);
    steps.push(host.take_outs());
    for _ in 0..K_LOOKS {
        rig.advance(LOOK_EVERY_MS);
        host.tick(rig, true);
        steps.push(host.take_outs());
    }
    assert_eq!(
        host.wake(rig, &a, Some(400), "taken #1"),
        "OK wake #1 taken\n"
    );
    rig.advance(LOOK_EVERY_MS);
    host.tick(rig, true);
    steps.push(host.take_outs());
    steps
}

/// THE WAKE K (F1): a ready wake no door told raises the tab's K Entry at the tenth look under
/// `prompt`, its event owed at its coming ready, and the Entry ends once the wake is taken.
/// NEGATIVE CONTROLS: under `observe` nothing is owed; a clock fed only from a break path (no
/// loop of this build runs one) never raises.
#[test]
fn a_ready_wake_untold_for_k_looks_raises_under_prompt_alone() {
    let prompt = k_run(&Rig::new(Occupancy::Prompt));
    let ready = Out::Ready {
        sid: A.to_string(),
        local: 1,
        ord: 1,
    };
    assert!(prompt[0].is_empty());
    assert_eq!(prompt[1], [ready]);
    for (look, outs) in prompt.iter().enumerate().skip(2).take(9) {
        assert!(outs.is_empty(), "look {}: {outs:?}", look - 1);
    }
    assert_eq!(
        prompt[11],
        [Out::Idle {
            sid: A.to_string(),
            idle: Some(WakeIdle {
                ord: 1,
                guard: None,
                looped: false,
                since_ms: T0,
                at_ms: T0 + K_LOOKS * LOOK_EVERY_MS,
            }),
        }]
    );
    assert_eq!(
        prompt[12],
        [Out::Idle {
            sid: A.to_string(),
            idle: None,
        }],
        "taken: it ends"
    );
    assert!(
        k_run(&Rig::new(Occupancy::Observe))
            .iter()
            .all(Vec::is_empty),
        "NEGATIVE CONTROL: observe owes no event and no Entry"
    );
    let break_only = gui_fault::with(GuiFault::KFromBreakOnly, || {
        k_run(&Rig::new(Occupancy::Prompt))
    });
    assert!(
        break_only
            .iter()
            .flatten()
            .all(|out| !matches!(out, Out::Idle { .. })),
        "NEGATIVE CONTROL: a K clock fed from the break path alone never raises: {break_only:?}"
    );
}

/// THE HANDOFF: a book rides its session record (a TOML round trip), and the successor's restore
/// reads it back, re-admits it under the current rules and reads its levels at once, so a job
/// that exited in the gap is ready before any tick, and ordinals go on. NEGATIVE CONTROL
/// `NoRearm`: a restore that arms no pid misses the exit.
#[test]
fn a_book_rides_the_session_record_and_its_restore_reads_levels_at_once() {
    let rig = Rig::new(Occupancy::Observe);
    let host = WakeHost::default();
    let a = seat_a();
    host.wake(&rig, &a, Some(400), "label=job when=pid:500 exited");
    host.wake(&rig, &a, Some(400), "label=later when=in:1h");
    let row = host.carry_row(A).expect("a book to carry");
    let base: crate::session_store::SessionRecord = aterm_toml::from_str(&format!(
        "local_id = 1\nsid = \"{A}\"\nstate = \"alive\"\ntitle = \"t\"\n"
    ))
    .expect("a bare record");
    let record = crate::session_store::SessionRecord {
        wakes: Some(row),
        ..base
    };
    let back: crate::session_store::SessionRecord =
        aterm_toml::from_str(&aterm_toml::to_string(&record).expect("serializes"))
            .expect("reads back");
    assert_eq!(back, record);

    rig.tree.kill(500);
    rig.advance(5_000);
    let bare = Seat {
        pty_child: None,
        agent: None,
        ..seat_a()
    };
    let carried = back.wakes.as_deref().expect("its row");
    let successor = WakeHost::default();
    assert_eq!(
        successor.restore(&rig, &bare, carried),
        Restored::Read {
            refused: 0,
            live: 2
        }
    );
    assert_eq!(state(&successor, A, 1), Some(WakeState::Ready));
    assert_eq!(state(&successor, A, 2), Some(WakeState::Pending));
    assert_eq!(
        successor.wake(&rig, &a, Some(400), "label=next when=in:1m"),
        "OK wake #3 await_only=1\n"
    );

    let blind = WakeHost::default();
    gui_fault::with(GuiFault::NoRearm, || blind.restore(&rig, &bare, carried));
    blind.tick(&rig, true);
    assert_eq!(
        state(&blind, A, 1),
        Some(WakeState::Pending),
        "NEGATIVE CONTROL: a pid not armed again misses its exit in the gap"
    );
}

/// RE-ADMISSION ON RESTORE: a carried wake the current rules refuse (a file atom, which this build
/// does not serve) ends `Expired why=admit`; a wake this build cannot read is dropped and the rest
/// stand; a row that is no book restores nothing; and a record whose `wakes` is no row still
/// reads, as the empty row (the record's lenient idiom), which restores nothing.
#[test]
fn a_restore_re_admits_under_the_current_rules_and_reads_leniently() {
    let rig = Rig::new(Occupancy::Observe);
    let agent = AgentId {
        pid: 300,
        birth_us: 30_000_000,
    };
    let mut book = WakeBook::default();
    for decl in ["label=f when=file:~/done", "label=t when=in:1h"] {
        book.declare(
            WakeReq::parse(decl).expect("the grammar reads it"),
            Declarer::Agent,
            agent,
            T0,
            0,
        )
        .expect("booked");
    }
    let mut wakes: Vec<aterm_json::Value> = book
        .wakes
        .iter()
        .map(|w| aterm_json::to_value(w).expect("a value"))
        .collect();
    wakes.push(aterm_json::Value::String(
        "a wake of a later build".to_string(),
    ));
    let row = aterm_json::to_string(&Carry {
        next_ord: book.next_ord,
        wakes,
        pids: Vec::new(),
        due: Vec::new(),
    })
    .expect("a row");
    let host = WakeHost::default();
    assert_eq!(
        host.restore(&rig, &seat_a(), &row),
        Restored::Read {
            refused: 1,
            live: 1
        }
    );
    assert_eq!(
        state(&host, A, 1),
        Some(WakeState::Expired {
            why: ExpireWhy::Admit
        })
    );
    assert_eq!(state(&host, A, 2), Some(WakeState::Pending));
    assert_eq!(host.lock().shelves[A].book.wakes.len(), 2);
    assert_eq!(
        WakeHost::default().restore(&rig, &seat_a(), "not a book"),
        Restored::Unread
    );
    let odd: crate::session_store::SessionRecord = aterm_toml::from_str(&format!(
        "local_id = 1\nsid = \"{A}\"\nstate = \"alive\"\ntitle = \"t\"\nwakes = 5\n"
    ))
    .expect("a record with an odd row still reads");
    assert_eq!(odd.wakes.as_deref(), Some(""), "read as the empty row");
    assert_eq!(
        WakeHost::default().restore(&rig, &seat_a(), ""),
        Restored::Unread,
        "which restores nothing"
    );
}

/// A PID ATOM READS ITS OWN PROCESS'S EXIT: a real stand-in of the session's tree (the test
/// process stands for its PTY child, pinned by the birth a spawn reads, which the kernel record a
/// proof walks meets), ended by SIGKILL, reads exited at the next tick.
#[cfg(target_os = "macos")]
#[test]
fn a_pid_atom_reads_a_real_stand_ins_exit() {
    let sleep = aterm_uds::deadline::stand_in(std::path::Path::new("/bin/sleep"));
    let mut child = aterm_uds::spawnfd::command(&sleep)
        .arg("600")
        .stdin(aterm_uds::spawnfd::Stdio::null())
        .stdout(aterm_uds::spawnfd::Stdio::null())
        .stderr(aterm_uds::spawnfd::Stdio::null())
        .spawn()
        .expect("the stand-in starts");
    let pid = child.id();
    let me = std::process::id();
    let rig = Rig::kernel(Occupancy::Observe);
    let shell = aterm_pty::record_spawned_shell(i32::try_from(me).expect("a pid"));
    let root = root_of(shell);
    assert!(
        root.is_some(),
        "PRECONDITION: the test process's birth reads"
    );
    let seat = Seat {
        sid: A.to_string(),
        local: 1,
        pty_child: root,
        agent: None,
    };
    let host = WakeHost::default();
    let declared = host.wake(
        &rig,
        &seat,
        Some(me),
        &format!("label=job when=pid:{pid} exited"),
    );
    host.tick(&rig, false);
    let before = state(&host, A, 1);
    child.kill().expect("SIGKILL the stand-in");
    child.wait().expect("reap it");
    host.tick(&rig, false);
    assert_eq!(declared, "OK wake #1\n");
    assert_eq!(before, Some(WakeState::Pending));
    assert_eq!(state(&host, A, 1), Some(WakeState::Ready));
}

/// A REUSED PID READS EXITED BY BIRTH: the pid alive again, born after the declaration, is not
/// the process the atom named.
#[test]
fn a_reused_pid_reads_exited_by_its_birth() {
    let rig = Rig::new(Occupancy::Observe);
    let host = WakeHost::default();
    let a = seat_a();
    host.wake(&rig, &a, Some(400), "label=job when=pid:500 exited");
    host.tick(&rig, false);
    assert_eq!(state(&host, A, 1), Some(WakeState::Pending));
    rig.tree.reborn(500, 99, 300);
    host.tick(&rig, false);
    assert_eq!(state(&host, A, 1), Some(WakeState::Ready));
}

/// A TAB'S ROOT IS THE SHELL ITS SESSION TOOK, NOT WHATEVER HOLDS ITS PID NOW: tab A's shell (200,
/// born 20 s) ends, a stranger (born 99 s) takes its pid and the stranger's child asks; the live
/// root's birth is not the pinned one, so it books nothing, reads the book redacted, parks
/// nothing, and its pids are not the tab's. A root gone is no root either. NEGATIVE CONTROL: a
/// child of the pinned shell is admitted, and in the reused tree the same peer proves for a seat
/// pinned to the stranger.
#[test]
fn a_reused_root_pid_proves_nothing() {
    let rig = Rig::new(Occupancy::Observe);
    let host = WakeHost::default();
    let tab = Seat {
        agent: None,
        ..seat_a()
    };
    let refused = denied_words(DeclarerRefusal::NotInTree);
    assert_eq!(
        host.wake(&rig, &tab, Some(450), "label=t when=in:1h"),
        "OK wake #1 await_only=1\n",
        "NEGATIVE CONTROL: a child of the pinned shell"
    );
    assert!(of_tree(&rig.tree, &tab, 450).is_some());

    rig.tree.kill(200);
    assert_eq!(
        host.wake(&rig, &tab, Some(450), "label=u when=in:1h"),
        refused,
        "a root gone"
    );

    rig.tree.reborn(200, 99, 1);
    rig.tree.reborn(210, 100, 200);
    assert_eq!(
        host.wake(&rig, &tab, Some(210), "label=u when=in:1h"),
        refused
    );
    let waits = host.waits(&rig, &tab, Some(210), "");
    assert!(
        waits.starts_with("OK 1\n") && !waits.contains("label="),
        "{waits}"
    );
    assert_eq!(host.park(&rig, &tab, Some(210), 1), Err(refused));
    assert_eq!(of_tree(&rig.tree, &tab, 210), None);
    assert_eq!(host.counts(A), (1, 0), "nothing booked past the first");

    let stranger = Seat {
        sid: B.to_string(),
        local: 2,
        pty_child: Some(link(200, 99, 1).id()),
        agent: None,
    };
    assert_eq!(
        host.wake(&rig, &stranger, Some(210), "label=u when=in:1h"),
        "OK wake #1 await_only=1\n",
        "NEGATIVE CONTROL: pinned to the process its pid names now, the same peer proves"
    );
}

/// A failed process read is neither a job's exit nor the declaring agent's death.
#[test]
fn an_unread_process_neither_readies_its_exit_nor_expires_its_agents_wakes() {
    let a = seat_a();
    let decl = "label=job when=pid:500 exited";
    let rig = Rig::new(Occupancy::Observe);
    let host = WakeHost::default();
    assert_eq!(host.wake(&rig, &a, Some(400), decl), "OK wake #1\n");
    rig.tree.unread(500);
    host.tick(&rig, false);
    assert_eq!(state(&host, A, 1), Some(WakeState::Pending));
    host.park(&rig, &a, Some(400), 1).expect("parks");
    assert_eq!(host.pass(&rig, A, 1), Pass::Wait);
    rig.tree.reborn(500, 50, 300);
    host.tick(&rig, false);
    assert_eq!(state(&host, A, 1), Some(WakeState::Pending));
    rig.tree.kill(500);
    assert!(matches!(
        host.pass(&rig, A, 1),
        Pass::Reply { ord: Some(1), .. }
    ));

    let rig = Rig::new(Occupancy::Observe);
    let host = WakeHost::default();
    assert_eq!(host.wake(&rig, &a, Some(400), decl), "OK wake #1\n");
    rig.tree.unread(300);
    host.tick(&rig, false);
    assert_eq!(state(&host, A, 1), Some(WakeState::Pending));
    let refused = host.wake(&rig, &a, Some(400), "label=unproved when=in:1h");
    assert!(
        refused.starts_with("ERR denied wake:"),
        "unread ancestry admits no new declaration: {refused}"
    );
    assert_eq!(
        host.wake(&rig, &a, Some(450), "label=sibling when=in:1h"),
        "ERR denied wake: not in this session's tree\n",
        "a readable sibling cannot bypass the unread hosted agent"
    );
    assert_eq!(host.counts(A), (1, 0));
    rig.tree.reborn(300, 30, 200);
    host.tick(&rig, false);
    assert_eq!(state(&host, A, 1), Some(WakeState::Pending));
    rig.tree.reborn(300, 99, 200);
    host.tick(&rig, false);
    assert_eq!(
        state(&host, A, 1),
        Some(WakeState::Expired {
            why: ExpireWhy::Agent
        }),
        "a known different birth still proves the declaring agent gone"
    );
}

/// The outs and the await's reply of one T-obs2 run under `mode`: a time wake awaited, and a job's
/// wake no one awaits, for K looks.
fn t_obs2(mode: Occupancy) -> (Pass, Vec<Out>) {
    let rig = Rig::new(mode);
    let host = WakeHost::default();
    let a = seat_a();
    host.wake(&rig, &a, Some(400), "label=t when=in:1m");
    host.wake(&rig, &a, Some(400), "label=job when=pid:500 exited");
    rig.advance(60_000);
    host.park(&rig, &a, Some(400), 1).expect("parks");
    let answered = host.pass(&rig, A, 1);
    host.answered(&rig, A, 1, true);
    rig.tree.kill(500);
    let mut outs = host.take_outs();
    for _ in 0..=K_LOOKS {
        host.tick(&rig, true);
        outs.extend(host.take_outs());
        rig.advance(LOOK_EVERY_MS);
    }
    (answered, outs)
}

/// T-obs2-gui: under `observe` the await is answered and nothing else is owed (no Entry, no event;
/// nothing here writes a task record's `fact=`). Under `prompt`: the events at each coming ready
/// and the Entry at K, for the wake no one awaited.
#[test]
fn t_obs2_gui_observe_answers_the_await_and_owes_nothing_else() {
    let answer = Pass::Reply {
        reply: format!("OK wake #1 ready since={}\n", T0 + 60_000),
        ord: Some(1),
    };
    assert_eq!(t_obs2(Occupancy::Observe), (answer.clone(), Vec::new()));
    let (answered, outs) = t_obs2(Occupancy::Prompt);
    assert_eq!(answered, answer);
    let ready = |ord| Out::Ready {
        sid: A.to_string(),
        local: 1,
        ord,
    };
    assert_eq!(&outs[..2], [ready(1), ready(2)]);
    assert!(
        matches!(
            &outs[2..],
            [Out::Idle {
                idle: Some(WakeIdle { ord: 2, .. }),
                ..
            }]
        ),
        "{outs:?}"
    );
}

/// THE AGENT GONE: its wakes end `Expired why=agent`, a parked wait hears it, and `waits why`
/// says so.
#[test]
fn an_agent_gone_expires_its_wakes_and_waits_why_says_so() {
    let rig = Rig::new(Occupancy::Observe);
    let host = WakeHost::default();
    let a = seat_a();
    host.wake(&rig, &a, Some(400), "label=t when=in:1h");
    host.park(&rig, &a, Some(400), 1).expect("parks");
    rig.tree.kill(300);
    host.tick(&rig, false);
    assert_eq!(
        state(&host, A, 1),
        Some(WakeState::Expired {
            why: ExpireWhy::Agent
        })
    );
    assert_eq!(
        host.pass(&rig, A, 1),
        Pass::Reply {
            reply: "EXPIRED wake #1 why=agent\n".to_string(),
            ord: None
        }
    );
    assert_eq!(
        host.waits(&rig, &a, Some(450), "why"),
        "OK 1\n#1 expired why=agent atoms=in:0 await_only=1\n"
    );
}

/// `waits` LISTS THE BOOK, and a peer outside the tree sees ordinals, states, priorities, kinds
/// and times alone (S12): no label and no need.
#[test]
fn waits_lists_the_book_redacted_outside_the_tree() {
    let rig = Rig::new(Occupancy::Observe);
    let host = WakeHost::default();
    let a = seat_a();
    host.wake(
        &rig,
        &a,
        Some(400),
        "label=build when=in:1m need=slots>=1 prio=critical by=+2h",
    );
    let need = SysWait::parse(&["slots>=1"]).expect("a machine term");
    let by = T0 + 2 * 3_600_000;
    assert_eq!(
        host.waits(&rig, &a, Some(400), ""),
        format!(
            "OK 1\n#1 pending prio=critical kinds=in declared={T0} by={by} since=- label=build\n"
        )
    );
    assert_eq!(
        host.waits(&rig, &a, Some(800), ""),
        format!("OK 1\n#1 pending prio=critical kinds=in declared={T0} by={by} since=-\n")
    );
    assert_eq!(
        host.waits(&rig, &a, Some(400), "why #1"),
        format!(
            "OK 1\n#1 pending atoms=in:0 need={} await_only=1\n",
            need.canonical()
        )
    );
    assert_eq!(
        host.waits(&rig, &a, None, "why"),
        "OK 1\n#1 pending atoms=in:0 await_only=1\n"
    );
    assert_eq!(
        host.waits(&rig, &a, Some(400), "why #2"),
        "ERR no such wake #2\n"
    );
    assert_eq!(host.waits(&rig, &a, Some(400), "all"), WAITS_USAGE);
    assert_eq!(host.counts(A), (1, 0));
}

/// THE OWNER'S `off` ENDS EVERY BOOK (`Expired why=off`), and a parked wait hears it; `wake rm`
/// of the last open wake ends a park as `why=rm`.
#[test]
fn off_disarms_every_book_and_a_parked_wait_hears_its_end() {
    let rig = Rig::new(Occupancy::Observe);
    let host = WakeHost::default();
    let a = seat_a();
    host.wake(&rig, &a, Some(400), "label=t when=in:1h");
    host.park(&rig, &a, Some(400), 1).expect("parks");
    host.note_mode(Occupancy::Off);
    assert_eq!(
        state(&host, A, 1),
        Some(WakeState::Expired {
            why: ExpireWhy::Off
        })
    );
    assert_eq!(
        host.pass(&rig, A, 1),
        Pass::Reply {
            reply: "EXPIRED wake #1 why=off\n".to_string(),
            ord: None
        }
    );
    host.wake(&rig, &a, Some(400), "label=u when=in:1h");
    host.park(&rig, &a, Some(400), 2).expect("parks");
    assert_eq!(host.wake(&rig, &a, Some(400), "rm #2"), "OK wake #2 rm\n");
    assert_eq!(
        host.pass(&rig, A, 2),
        Pass::Reply {
            reply: "EXPIRED wake #2 why=rm\n".to_string(),
            ord: None
        }
    );
    assert_eq!(
        host.wake(&rig, &a, Some(400), "rm #2"),
        "ERR no such wake #2\n"
    );
    assert_eq!(host.wake(&rig, &a, Some(400), "rm 2"), RM_USAGE);
}

/// FILE ATOMS ARE NOT SERVED YET (2.6 serves them): refused before any path is touched, in
/// `when=` and in `unless=`.
#[test]
fn a_file_atom_is_refused_until_it_is_served() {
    let rig = Rig::new(Occupancy::Observe);
    let host = WakeHost::default();
    let a = seat_a();
    for decl in [
        "label=f when=file:~/done",
        "label=f when=in:1m unless=gone:~/lock",
        "label=f when=file:~/log~/ok/",
    ] {
        assert_eq!(host.wake(&rig, &a, Some(400), decl), FILE_ATOMS, "{decl}");
    }
    assert_eq!(host.counts(A), (0, 0));
}

/// THE PARKED WAIT'S LOOP: an answer ready at once returns at once; a hang-up and a time run out
/// end the park unanswered.
#[test]
fn the_parked_wait_returns_its_answer_or_its_end_and_unparks() {
    let rig = Rig::new(Occupancy::Observe);
    let host = WakeHost::default();
    let a = seat_a();
    host.wake(&rig, &a, Some(400), "label=t when=in:1m");
    fn never() -> bool {
        false
    }
    fn ever() -> bool {
        true
    }
    fn unarmed() {}
    fn waits(deadline: Instant, hung_up: &'static dyn Fn() -> bool) -> Waits<'static, ()> {
        Waits {
            deadline,
            hung_up,
            exited: &never,
            arm_slots: &unarmed,
        }
    }
    let soon = Instant::now() + Duration::from_secs(60);
    assert_eq!(
        await_wake(&host, &rig, &a, Some(400), 1, &waits(soon, &ever)),
        Pass::Reply {
            reply: crate::control::HUNG_UP_REPLY.to_string(),
            ord: None
        }
    );
    assert!(parks(&host, A).is_empty());
    assert_eq!(
        await_wake(
            &host,
            &rig,
            &a,
            Some(400),
            2,
            &waits(Instant::now(), &never)
        ),
        Pass::Reply {
            reply: "OK timeout\n".to_string(),
            ord: None
        }
    );
    assert!(parks(&host, A).is_empty());
    rig.advance(60_000);
    assert_eq!(
        await_wake(&host, &rig, &a, Some(400), 3, &waits(soon, &never)),
        Pass::Reply {
            reply: format!("OK wake #1 ready since={}\n", T0 + 60_000),
            ord: Some(1)
        }
    );
    assert_eq!(status_fields(None), "waits=0 waits_ready=0");
}

/// A carried row of `wakes`, its next ordinal past theirs.
fn row_of(wakes: &[DeclaredWake]) -> String {
    aterm_json::to_string(&Carry {
        next_ord: wakes.iter().map(|w| w.ord + 1).max().unwrap_or(1),
        wakes: wakes
            .iter()
            .map(|w| aterm_json::to_value(w).expect("a value"))
            .collect(),
        pids: Vec::new(),
        due: Vec::new(),
    })
    .expect("a row")
}

/// `decls` booked for tab A's agent at [`T0`].
fn booked(decls: &[&str]) -> Vec<DeclaredWake> {
    let agent = AgentId {
        pid: 300,
        birth_us: 30_000_000,
    };
    let mut book = WakeBook::default();
    for decl in decls {
        book.declare(
            WakeReq::parse(decl).expect("the grammar reads it"),
            Declarer::Agent,
            agent,
            T0,
            0,
        )
        .expect("booked");
    }
    book.wakes
}

/// A BOOK CARRIED INTO THE OWNER'S `off` ARRIVES DISARMED (the 2.5 safety review's F1): restored
/// under `off`, its wakes end `Expired why=off` at once, a tick past their time reads nothing,
/// and once the mode leaves `off` no wait is answered by them. NEGATIVE CONTROL: the same restore
/// under `observe` stays open, comes ready at its time, and is answered.
#[test]
fn a_book_restored_under_off_is_disarmed_and_never_answered() {
    let run = |mode: Occupancy| {
        let rig = Rig::new(Occupancy::Observe);
        let a = seat_a();
        let parent = WakeHost::default();
        parent.wake(&rig, &a, Some(400), "label=t when=in:1m");
        let row = parent.carry_row(A).expect("a book to carry");
        rig.set_mode(mode);
        let bare = Seat {
            pty_child: None,
            agent: None,
            ..seat_a()
        };
        let successor = WakeHost::default();
        let restored = successor.restore(&rig, &bare, &row);
        rig.advance(120_000);
        successor.tick(&rig, true);
        let ticked = state(&successor, A, 1);
        rig.set_mode(Occupancy::Observe);
        let answer = successor
            .park(&rig, &a, Some(400), 1)
            .map(|()| successor.pass(&rig, A, 1));
        (restored, ticked, answer)
    };
    assert_eq!(
        run(Occupancy::Off),
        (
            Restored::Read {
                refused: 0,
                live: 0
            },
            Some(WakeState::Expired {
                why: ExpireWhy::Off
            }),
            Err("ERR usage await wake: no wake pending\n".to_string()),
        )
    );
    let (restored, ticked, answer) = run(Occupancy::Observe);
    assert_eq!(
        (restored, ticked),
        (
            Restored::Read {
                refused: 0,
                live: 1
            },
            Some(WakeState::Ready)
        ),
        "NEGATIVE CONTROL: restored under observe, it stays open"
    );
    assert!(
        matches!(answer, Ok(Pass::Reply { ord: Some(1), .. })),
        "NEGATIVE CONTROL: and is answered: {answer:?}"
    );
}

/// A VERB THAT READ THE MODE BEFORE A SWITCH TO `off` BOOKS AND ANSWERS NOTHING PAST IT (the 2.5
/// safety review's F2): the switch set the mode and disarmed every book between the verb's first
/// read and its lock, and the verb asks again under the lock, so a declaration and a park are
/// refused `off`; a pass and a tick that meet the mode set but the book not yet disarmed disarm
/// it themselves (the parked wait hears `EXPIRED why=off`; the exited job is not read ready).
/// NEGATIVE CONTROL: the same stale read with the mode still `observe` books.
#[test]
fn a_verb_that_read_the_mode_before_a_switch_to_off_books_and_answers_nothing() {
    let a = seat_a();
    let expired = Some(WakeState::Expired {
        why: ExpireWhy::Off,
    });

    let rig = Rig::new(Occupancy::Off);
    let host = WakeHost::default();
    host.note_mode(Occupancy::Off);
    rig.stale_once(Occupancy::Observe);
    assert_eq!(host.wake(&rig, &a, Some(400), "label=t when=in:1m"), OFF);
    assert_eq!(host.counts(A), (0, 0), "nothing booked past the switch");

    let rig = Rig::new(Occupancy::Observe);
    let host = WakeHost::default();
    host.wake(&rig, &a, Some(400), "label=t when=in:1h");
    rig.set_mode(Occupancy::Off);
    host.note_mode(Occupancy::Off);
    rig.stale_once(Occupancy::Observe);
    assert_eq!(host.park(&rig, &a, Some(400), 1), Err(OFF.to_string()));
    rig.stale_once(Occupancy::Observe);
    assert_eq!(host.wake(&rig, &a, Some(400), "rm #1"), OFF);

    let rig = Rig::new(Occupancy::Observe);
    let host = WakeHost::default();
    host.wake(&rig, &a, Some(400), "label=t when=in:1m");
    rig.advance(60_000);
    host.park(&rig, &a, Some(400), 1).expect("parks");
    rig.set_mode(Occupancy::Off);
    assert_eq!(
        host.pass(&rig, A, 1),
        Pass::Reply {
            reply: "EXPIRED wake #1 why=off\n".to_string(),
            ord: None
        },
        "the switch's mode is set and its disarm not yet run: the pass ends it"
    );
    assert_eq!(state(&host, A, 1), expired);

    let rig = Rig::new(Occupancy::Observe);
    let host = WakeHost::default();
    host.wake(&rig, &a, Some(400), "label=job when=pid:500 exited");
    rig.set_mode(Occupancy::Off);
    rig.tree.kill(500);
    host.tick(&rig, true);
    assert_eq!(
        state(&host, A, 1),
        expired,
        "the tick reads no level under off"
    );

    let rig = Rig::new(Occupancy::Observe);
    let host = WakeHost::default();
    rig.stale_once(Occupancy::Observe);
    assert_eq!(
        host.wake(&rig, &a, Some(400), "label=t when=in:1m"),
        "OK wake #1 await_only=1\n",
        "NEGATIVE CONTROL: no switch, the same read books"
    );
}

/// A PUBLISHED AGENT WHOSE PID ANOTHER PROCESS HOLDS IS NO AGENT: the harness's snapshot names pid
/// 500 started at 99 s, and the process at 500 was born at 50 s, so the proof does not narrow to
/// it, and a new agent of the tab's own tree (the shell's other child) is admitted for itself.
/// NEGATIVE CONTROL: with the snapshot's start the process's birth, the proof narrows to it, and
/// the same peer is refused.
#[test]
fn a_published_agent_pid_another_process_holds_narrows_nothing() {
    let rig = Rig::new(Occupancy::Observe);
    let host = WakeHost::default();
    let decl = "label=t when=in:1h";
    assert_eq!(
        host.wake(&rig, &seat(A, 1, (200, 20), (500, 99)), Some(450), decl),
        "OK wake #1 await_only=1\n"
    );
    assert_eq!(
        declared(&host, A, 1).agent,
        AgentId {
            pid: 450,
            birth_us: 45_000_000
        }
    );
    assert_eq!(
        host.wake(&rig, &seat(A, 1, (200, 20), (500, 50)), Some(450), decl),
        "ERR denied wake: not in this session's tree\n",
        "NEGATIVE CONTROL: the snapshot's own agent narrows the proof"
    );
}

/// AN AGENT WHOSE PID WAS REUSED IS GONE: its wakes end `Expired why=agent` though a process
/// holds its pid again. NEGATIVE CONTROL: the agent itself alive keeps them pending.
#[test]
fn an_agent_whose_pid_was_reused_is_gone() {
    let rig = Rig::new(Occupancy::Observe);
    let host = WakeHost::default();
    let a = seat_a();
    host.wake(&rig, &a, Some(400), "label=t when=in:1h");
    host.tick(&rig, false);
    assert_eq!(
        state(&host, A, 1),
        Some(WakeState::Pending),
        "NEGATIVE CONTROL: the agent alive"
    );
    rig.tree.reborn(300, 99, 200);
    host.tick(&rig, false);
    assert_eq!(
        state(&host, A, 1),
        Some(WakeState::Expired {
            why: ExpireWhy::Agent
        })
    );
}

/// RE-ADMISSION HOLDS THE CURRENT BOUNDS: a carried book of more live wakes than
/// [`MAX_WAKES`](aterm_agent::supervise::occupancy::wakes::MAX_WAKES) keeps the first of them and
/// ends the rest `Expired why=admit`, and a wake whose `by=` reaches past
/// [`BY_MAX_MS`](aterm_agent::supervise::occupancy::wakes::BY_MAX_MS) ends so too. NEGATIVE
/// CONTROL: a `by=` at the bound itself stands.
#[test]
fn a_restore_holds_the_live_cap_and_the_by_bound() {
    use aterm_agent::supervise::occupancy::wakes::{BY_MAX_MS, MAX_WAKES};
    let rig = Rig::new(Occupancy::Observe);
    let decls: Vec<String> = (0..MAX_WAKES)
        .map(|n| format!("label=w{n} when=in:1h"))
        .collect();
    let mut wakes = booked(&decls.iter().map(String::as_str).collect::<Vec<_>>());
    let over = u16::try_from(MAX_WAKES + 1).expect("an ordinal");
    wakes.push(DeclaredWake {
        ord: over,
        ..wakes[0].clone()
    });
    let host = WakeHost::default();
    assert_eq!(
        host.restore(&rig, &seat_a(), &row_of(&wakes)),
        Restored::Read {
            refused: 1,
            live: MAX_WAKES
        }
    );
    assert_eq!(
        state(&host, A, over),
        Some(WakeState::Expired {
            why: ExpireWhy::Admit
        })
    );

    let mut wakes = booked(&["label=far when=in:1h", "label=near when=in:1h"]);
    wakes[0].due_by_ms = wakes[0].declared_ms + BY_MAX_MS + 1;
    wakes[1].due_by_ms = wakes[1].declared_ms + BY_MAX_MS;
    let host = WakeHost::default();
    assert_eq!(
        host.restore(&rig, &seat_a(), &row_of(&wakes)),
        Restored::Read {
            refused: 1,
            live: 1
        }
    );
    assert_eq!(
        state(&host, A, 1),
        Some(WakeState::Expired {
            why: ExpireWhy::Admit
        })
    );
    assert_eq!(
        state(&host, A, 2),
        Some(WakeState::Pending),
        "NEGATIVE CONTROL: a by= at the bound stands"
    );
}

/// THE WINDOW'S DELIVERY RE-CHECKS THE OWNER'S `prompt` (W5): a wake's coming ready is recorded
/// as its tab's `wake-ready` row only while `prompt` is in force, and a K Entry's move reaches the
/// host as its note. NEGATIVE CONTROL: the same out delivered after a switch to `observe` records
/// no row.
#[test]
fn delivery_records_a_ready_row_under_prompt_alone() {
    let store = crate::session_store::new_store();
    let handle = crate::session_store::test_handle(1);
    let sid = handle.sid.as_str().to_string();
    let timeline = Arc::clone(&handle.ctx.timeline);
    store.write().unwrap().register(handle);
    let noted: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let wiring = Wiring {
        store,
        note: {
            let noted = Arc::clone(&noted);
            Arc::new(move |note| {
                if let crate::harness_host::HostNote::WakeIdle { sid } = note {
                    noted.lock().unwrap().push(sid);
                }
            })
        },
    };
    let rows_of = |mode: Occupancy| {
        crate::occupancy_host::set_mode(mode);
        let before = timeline.lock().unwrap().view_head();
        deliver_to(
            &wiring,
            vec![Out::Ready {
                sid: sid.clone(),
                local: 1,
                ord: 1,
            }],
        );
        let g = timeline.lock().unwrap();
        g.view_since(before)
            .rows
            .iter()
            .map(|e| (e.kind, e.payload.to_string()))
            .collect::<Vec<_>>()
    };
    assert_eq!(
        rows_of(Occupancy::Observe),
        [],
        "NEGATIVE CONTROL: owed under prompt, delivered under observe: no row"
    );
    assert_eq!(
        rows_of(Occupancy::Prompt),
        [(WAKE_READY, "#1 ready".to_string())]
    );
    deliver_to(
        &wiring,
        vec![Out::Idle {
            sid: sid.clone(),
            idle: None,
        }],
    );
    assert_eq!(*noted.lock().unwrap(), [sid]);
    crate::occupancy_host::set_mode(Occupancy::Observe);
}

/// A PARK'S MANIFEST CARRIES EACH SESSION'S BOOK: `SessionHandoff::from_store` writes the book the
/// host holds for a session as its record's `wakes`. NEGATIVE CONTROL: a session with no book
/// carries none.
#[test]
fn the_handoff_manifest_carries_each_sessions_book() {
    let store = crate::session_store::new_store();
    let booked_handle = crate::session_store::test_handle(1);
    let bare_handle = crate::session_store::test_handle(2);
    let (sid, bare) = (
        booked_handle.sid.as_str().to_string(),
        bare_handle.sid.as_str().to_string(),
    );
    store.write().unwrap().register(booked_handle);
    store.write().unwrap().register(bare_handle);
    let agent = AgentId {
        pid: 300,
        birth_us: 30_000_000,
    };
    assert_eq!(book_for_test(&sid, 1, agent, "label=t when=in:1h", T0), 1);
    let manifest = crate::session_store::SessionHandoff::from_store(&store.read().unwrap());
    let row = |of: &str| {
        manifest
            .sessions
            .iter()
            .find(|rec| rec.sid == of)
            .expect("its record")
            .wakes
            .clone()
    };
    assert_eq!(row(&sid).as_deref().and_then(carried_wakes), Some(1));
    assert_eq!(row(&bare), None, "NEGATIVE CONTROL: no book, no row");
    forget(&sid);
}

/// A NEW TAB'S SEAT NAMES ITS SHELL AT ONCE: the root a wake is proved under is the PTY child the
/// session was registered with, pinned by the birth its session read, so a tab no machine
/// snapshot names yet still has a tree (a process of it proves, an outside client is refused as
/// another session's); a session with no child, or whose shell's birth was never read, has none.
#[test]
fn a_seat_names_its_shell_before_any_machine_snapshot_names_the_tab() {
    let store = crate::session_store::new_store();
    let mut spawned = crate::session_store::test_handle(0x5ea7);
    spawned.shell_identity = aterm_pty::ShellIdentity::from_parts(4242, Some(42_000_000), true);
    let sid = spawned.sid.as_str().to_string();
    let childless = crate::session_store::test_handle(0x5ea8);
    let mut unread = crate::session_store::test_handle(0x5ea9);
    unread.shell_identity = aterm_pty::ShellIdentity::from_parts(4243, None, true);
    let mut book = store.write().unwrap_or_else(PoisonError::into_inner);
    book.register(spawned);
    book.register(childless);
    book.register(unread);
    drop(book);
    let seat = seat_of(&store, 0x5ea7).expect("a registered session's seat");
    assert_eq!(
        (seat.sid.as_str(), seat.pty_child),
        (sid.as_str(), Some(link(4242, 42, 0).id()))
    );
    let seat = seat_of(&store, 0x5ea8).expect("a registered session's seat");
    assert_eq!(seat.pty_child, None);
    let seat = seat_of(&store, 0x5ea9).expect("a registered session's seat");
    assert_eq!(seat.pty_child, None, "a bare pid pins nothing");
}
