// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The probe's process reads on their lane ([`ProcsRead::Lane`]): a sweep the kernel holds
//! leaves every other figure published and names the read it is held in.

use std::sync::atomic::AtomicUsize;
use std::sync::{Condvar, LazyLock};

use aterm_spec::wait::{MUST_HAPPEN, until_within};
use aterm_vitals::sys::{Ctx, Freshness, SysQuery, Target};

use super::*;
use crate::sys_host::Shared;

/// A gate a stand-in sweep waits on: shut, it stands for a kernel call that never returns (a
/// process in the middle of an exec holds every read of the process table).
struct Gate {
    open: Mutex<bool>,
    opened: Condvar,
}

impl Gate {
    const fn shut() -> Self {
        Self {
            open: Mutex::new(false),
            opened: Condvar::new(),
        }
    }

    fn pass(&self) {
        let mut open = self.open.lock().unwrap();
        while !*open {
            open = self.opened.wait(open).unwrap();
        }
    }

    fn open(&self) {
        *self.open.lock().unwrap() = true;
        self.opened.notify_all();
    }
}

/// The lane's bound where its sweep never answers: what it times is a read that cannot return,
/// so its length is no latency budget.
const SHORT: Duration = Duration::from_millis(50);

// Freshness has one immutable clock authority across the reader and its workers. Native
// counter timestamps, Reader's elapsed CPU sample clock and lane hang detectors still move.
fn lane_instant() -> Instant {
    static AT: LazyLock<Instant> = LazyLock::new(Instant::now);
    *AT
}

fn lane_continuous() -> Option<u64> {
    Some(0)
}

/// The sweep's lane's stall at `snap`, if it stalled.
fn procs_stall(snap: &Snapshot) -> Option<aterm_vitals::lanes::LaneStall> {
    snap.stalled
        .iter()
        .find(|l| l.lane == aterm_vitals::lanes::PROCS)
        .cloned()
}

/// Whether the sweep's lane stalled at `snap`.
fn procs_stalled(snap: &Snapshot) -> bool {
    procs_stall(snap).is_some()
}

/// A reader whose sweep is `sweep`, read on a lane waited for `wait`.
fn reader_with(sweep: SweepFn, wait: Duration) -> Reader {
    let mut reader = Reader::new(Arc::default());
    reader.clock = lane_instant;
    reader.memory_probes.continuous_ns = lane_continuous;
    reader.split = SplitReader::new(
        CoalProbe {
            sweep,
            ..CoalProbe::INERT
        },
        Box::leak(Box::new(Arming::new())),
    );
    reader.procs = ProcsRead::lane(wait);
    reader
}

/// A probe thread over `reader`, its workers' copy `sys`, once its first reading landed (so the
/// workers' pulls reach it).
fn probe_over(reader: fn() -> Reader, sys: &'static Shared) -> VitalsHost {
    let mut host = VitalsHost::default();
    host.start(reader, || true, None, Some(sys));
    assert!(host.offer(Msg::Read(Box::default())));
    until_within(MUST_HAPPEN, "the first reading landed", || {
        host.adopt(Instant::now()).then_some(())
    });
    host
}

/// `snap`'s overview as `sys` prints it at `state`.
fn overview(snap: &Snapshot, state: Freshness) -> String {
    reply_text(snap, state, "overview")
}

/// `snap`'s reply to `sys <query>` as `sys` prints it at `state`.
fn reply_text(snap: &Snapshot, state: Freshness, query: &str) -> String {
    let machine = Machine::default();
    let target = Target::default();
    let ctx = Ctx {
        snap: Some(snap),
        epoch: 1,
        as_of_unix_ms: u64::try_from(snap.at_unix_ms).unwrap_or(0),
        age_ms: Some(0),
        state,
        machine: &machine,
        target: &target,
        tabs: &[],
        caller_panes: None,
        event_caller: None,
        owner_class: true,
        volume: None,
        roots: &[],
        disk: None,
        stores: &[],
        home: None,
        held: None,
        events: &[],
        gap: None,
        head_seq: snap.seq,
    };
    let q = SysQuery::parse(query).expect("the query");
    let reply = aterm_vitals::sys::render_text(&q, &ctx);
    let mut text = reply.header(&ctx, false);
    for line in reply.lines {
        text.push('\n');
        text.push_str(&line.text);
    }
    text
}

/// The workers' copy a test's probe publishes to: every pull sent (no gap), each waited for a
/// minute at most (a hang detector).
fn workers() -> Shared {
    Shared::paced_with_clocks(Duration::ZERO, MUST_HAPPEN, lane_instant, lane_continuous)
}

/// A sweep the kernel never answers still lets a pull's reading publish CPU, memory, swap, disk
/// and the network, partial, naming the read it is held in and since when, never warming.
/// CONTROL: the kernel's sweep answers, and the same pull names no stall of the sweep's lane.
#[test]
fn a_hung_sweep_still_publishes_cpu_memory_swap_disk_and_net_and_names_the_sweep() {
    static GATE: Gate = Gate::shut();
    static ENTERED: AtomicUsize = AtomicUsize::new(0);
    fn hung(
        _: aterm_sysprobe::SweepOpts,
        _: &mut aterm_sysprobe::coalitions::CoalitionIds,
    ) -> Option<aterm_sysprobe::Sweep> {
        aterm_sysprobe::lane::mark("proc_pid_rusage", Some(4321));
        ENTERED.fetch_add(1, Ordering::SeqCst);
        GATE.pass();
        None
    }
    static HUNG: LazyLock<Shared> = LazyLock::new(workers);
    let before = unix_ms_now();
    let mut host = probe_over(|| reader_with(hung, SHORT), &HUNG);
    // The pull below reads the lane once its sweep is in its call: a reading taken before the
    // sweep's thread ran its mark would name the lane, not the call, however long it waited.
    until_within(MUST_HAPPEN, "the sweep entered its call", || {
        (ENTERED.load(Ordering::SeqCst) == 1).then_some(())
    });
    let (held, state) = HUNG.fresh(Duration::ZERO, true);
    assert_eq!(state, Freshness::Partial);
    let snap = held.expect("the pull's reading").snap;
    assert!(snap.cpu.is_some(), "the CPU");
    assert!(snap.mem.is_some(), "memory and swap");
    assert!(snap.volume.is_some(), "the disk");
    assert!(snap.net.is_some(), "the network");
    assert!(snap.aterm.is_none(), "no sweep, no aterm figures");
    let stall = snap
        .stalled
        .iter()
        .find(|l| l.lane == aterm_vitals::lanes::PROCS)
        .expect("the sweep's lane is named stalled");
    assert_eq!((stall.call, stall.pid), ("proc_pid_rusage", Some(4321)));
    assert!(stall.since_unix_ms >= before, "{stall:?}");
    let text = overview(&snap, state);
    let header = text.lines().next().expect("the reply header");
    assert!(header.contains("state=partial stalled=procs"), "{text}");
    assert!(
        text.contains("stalled procs: proc_pid_rusage pid=4321 since "),
        "{text}"
    );
    GATE.open();
    assert!(host.stop_at_exit(MUST_HAPPEN), "the thread ended");

    // The negative control: the kernel's sweep answers.
    static ANSWERED: LazyLock<Shared> = LazyLock::new(workers);
    let mut host = probe_over(
        || reader_with(aterm_sysprobe::processes_keeping, MUST_HAPPEN),
        &ANSWERED,
    );
    let (held, state) = ANSWERED.fresh(Duration::ZERO, true);
    let snap = held.expect("the pull's reading").snap;
    // Every lane is judged: a test binary waits for the kernel's process table as its hang
    // detector (`aterm_sysprobe::kproc`), so no load stalls it.
    assert!(snap.stalled.is_empty(), "{:?}", snap.stalled);
    assert_eq!(state, Freshness::Fresh);
    assert!(snap.aterm.is_some());
    assert!(!overview(&snap, state).contains("stalled procs"));
    assert!(host.stop_at_exit(MUST_HAPPEN), "the thread ended");
}

/// However many pulls follow a sweep the kernel holds, it is asked once: each later reading
/// names it stalled without asking again or waiting for it.
#[test]
fn a_stalled_sweep_is_asked_once_however_many_pulls_follow() {
    static GATE: Gate = Gate::shut();
    static ENTERED: AtomicUsize = AtomicUsize::new(0);
    fn hung(
        _: aterm_sysprobe::SweepOpts,
        _: &mut aterm_sysprobe::coalitions::CoalitionIds,
    ) -> Option<aterm_sysprobe::Sweep> {
        ENTERED.fetch_add(1, Ordering::SeqCst);
        aterm_sysprobe::lane::mark("proc_listallpids", None);
        GATE.pass();
        None
    }
    static SYS: LazyLock<Shared> = LazyLock::new(workers);
    let mut host = probe_over(|| reader_with(hung, SHORT), &SYS);
    let mut since = None;
    for _ in 0..5 {
        let (held, state) = SYS.fresh(Duration::ZERO, true);
        assert_eq!(state, Freshness::Partial);
        let snap = held.expect("the pull's reading").snap;
        let stall = procs_stall(&snap).expect("the sweep stalled");
        assert_eq!(
            *since.get_or_insert(stall.since_unix_ms),
            stall.since_unix_ms
        );
    }
    assert_eq!(ENTERED.load(Ordering::SeqCst), 1, "one sweep asked");
    GATE.open();
    assert!(host.stop_at_exit(MUST_HAPPEN), "the thread ended");
}

/// A sweep that hangs after good readings leaves each later reading's clock moving and names the
/// stall since its ask, which stays while it lasts. CONTROL: while the sweep answers, before the
/// hang and after it, no reading names a stall.
#[test]
fn a_sweep_that_hangs_after_a_good_sample_keeps_as_of_moving_and_reports_since() {
    static GATE: Gate = Gate::shut();
    static HANG: AtomicBool = AtomicBool::new(false);
    static ENTERED: AtomicUsize = AtomicUsize::new(0);
    fn later(
        opts: aterm_sysprobe::SweepOpts,
        ids: &mut aterm_sysprobe::coalitions::CoalitionIds,
    ) -> Option<aterm_sysprobe::Sweep> {
        if HANG.load(Ordering::SeqCst) {
            aterm_sysprobe::lane::mark("proc_listallpids", None);
            ENTERED.fetch_add(1, Ordering::SeqCst);
            GATE.pass();
            return None;
        }
        aterm_sysprobe::processes_keeping(opts, ids)
    }
    let req = Request {
        self_pid: std::process::id(),
        ..Request::default()
    };
    let mut reader = reader_with(later, MUST_HAPPEN);
    let _ = reader.read(&req);
    let good = reader.read(&req);
    assert!(good.stalled.is_empty(), "{:?}", good.stalled);
    assert!(good.aterm.is_some());
    HANG.store(true, Ordering::SeqCst);
    reader.procs = ProcsRead::lane(SHORT);
    let asked = reader.read(&req);
    assert!(procs_stalled(&asked), "{:?}", asked.stalled);
    // The next reading's ask finds the call overdue once the sweep is in it, and names that
    // call: one taken before the sweep's thread ran its mark would name the lane.
    until_within(MUST_HAPPEN, "the sweep entered its call", || {
        (ENTERED.load(Ordering::SeqCst) == 1).then_some(())
    });
    let first = reader.read(&req);
    let stall = procs_stall(&first).expect("the sweep stalled");
    assert_eq!(stall.call, "proc_listallpids");
    assert!(stall.since_unix_ms >= good.at_unix_ms, "{stall:?}");
    assert!(first.cpu.is_some() && first.mem.is_some());
    let second = until_within(MUST_HAPPEN, "the wall clock moved", || {
        let next = reader.read(&req);
        (next.at_unix_ms > first.at_unix_ms).then_some(next)
    });
    let still = procs_stall(&second).expect("still stalled");
    assert_eq!(still.since_unix_ms, stall.since_unix_ms, "since its ask");
    assert!(still.for_ms >= stall.for_ms);
    assert!(second.seq > first.seq && second.cpu.is_some());
    HANG.store(false, Ordering::SeqCst);
    GATE.open();
    reader.procs = ProcsRead::lane(MUST_HAPPEN);
    let back = reader.read(&req);
    assert!(back.stalled.is_empty(), "{:?}", back.stalled);
    assert!(back.at_unix_ms >= second.at_unix_ms);
}

/// Ownership proof runs with the process sweep: a held shell-generation read cannot hold the
/// machine figures or publish a partially checked tree. Once it returns, the next whole sweep
/// checks the root again and can attribute it.
#[test]
fn a_held_ownership_proof_keeps_machine_readings_fresh_and_names_its_phase_and_pid() {
    static GATE: Gate = Gate::shut();
    static ENTERED: AtomicUsize = AtomicUsize::new(0);
    fn sweep(
        _: aterm_sysprobe::SweepOpts,
        _: &mut aterm_sysprobe::coalitions::CoalitionIds,
    ) -> Option<aterm_sysprobe::Sweep> {
        Some(aterm_sysprobe::Sweep {
            procs: vec![aterm_sysprobe::ProcessReading {
                pid: 4321,
                ppid: std::process::id(),
                start: Some(1),
                cpu_ns: Some(0),
                footprint: Some(1),
                ..aterm_sysprobe::ProcessReading::default()
            }],
            truncated: false,
        })
    }
    fn shell(identity: aterm_pty::ShellIdentity) -> Option<bool> {
        ENTERED.fetch_add(1, Ordering::SeqCst);
        GATE.pass();
        Some(identity.birth == Some(1))
    }
    let req = Request {
        self_pid: std::process::id(),
        tabs: vec![TabRoot {
            key: 7,
            tab: 7,
            tab_id: None,
            shell_pid: 4321,
            label: "fixture".into(),
            program: String::new(),
            agent_leader: None,
        }],
        shell_identities: vec![(
            7,
            aterm_pty::ShellIdentity::from_parts(4321, Some(1), false),
        )],
        ..Request::default()
    };
    let mut reader = reader_with(sweep, SHORT);
    reader.ownership.set_shell_verifier(shell);
    let asked = reader.read(&req);
    assert!(procs_stalled(&asked));
    until_within(MUST_HAPPEN, "the ownership proof entered its call", || {
        (ENTERED.load(Ordering::SeqCst) == 1).then_some(())
    });
    let first = reader.read(&req);
    for snap in [first.clone(), reader.read(&req), reader.read(&req)] {
        let stall = procs_stall(&snap).expect("the ownership proof stalled");
        assert_eq!((stall.call, stall.pid), ("shell_generation", Some(4321)));
        assert!(snap.cpu.is_some() && snap.mem.is_some());
        assert!(snap.aterm.is_none(), "no partially proved process scope");
        assert!(snap.seq > asked.seq && snap.at_unix_ms >= asked.at_unix_ms);
    }
    assert_eq!(ENTERED.load(Ordering::SeqCst), 1, "one proof in flight");
    GATE.open();
    if let ProcsRead::Lane { wait, .. } = &mut reader.procs {
        *wait = MUST_HAPPEN;
    }
    let recovered = until_within(MUST_HAPPEN, "a fresh proved sweep answered", || {
        let snap = reader.read(&req);
        (!procs_stalled(&snap) && snap.aterm.is_some()).then_some(snap)
    });
    assert!(recovered.cpu.is_some() && recovered.mem.is_some());
    assert!(
        recovered
            .tabs
            .iter()
            .any(|tab| tab.key == 7 && tab.scope.procs == 1)
    );
    assert!(
        ENTERED.load(Ordering::SeqCst) >= 2,
        "the overdue proof was not adopted"
    );
}

/// A RESTING READING ASKS ITS SWEEP FOR NO SCHEDULING (C4): a whole reading of a request no
/// reader of each process's scheduling made (no pull that prints it, no CPU lens) asks the sweep
/// for none, and so makes no `getpriority`; each reader asks it: a pull a query armed
/// ([`Request::scheduling`]), the CPU lens, and the model's CPU reports once a CPU policy is
/// recorded. Each reading's snapshot says whether its sweep read it. NEGATIVE CONTROL: the
/// sweep asked for scheduling at every reading, as before, asks it at rest.
#[test]
fn a_resting_reading_asks_its_sweep_for_no_scheduling() {
    static ASKED: Mutex<Vec<bool>> = Mutex::new(Vec::new());
    fn sweep(
        opts: aterm_sysprobe::SweepOpts,
        ids: &mut aterm_sysprobe::coalitions::CoalitionIds,
    ) -> Option<aterm_sysprobe::Sweep> {
        ASKED
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(opts.scheduling);
        aterm_sysprobe::processes_keeping(opts, ids)
    }
    let mut reader = reader_with(sweep, MUST_HAPPEN);
    let rest = Request {
        self_pid: std::process::id(),
        ..Request::default()
    };
    let read = |reader: &mut Reader, req: &Request| {
        let snap = reader.read(req);
        let asked = std::mem::take(&mut *ASKED.lock().unwrap_or_else(PoisonError::into_inner));
        (asked, snap.scheduling_read)
    };
    assert_eq!(read(&mut reader, &rest), (vec![false], false), "at rest");
    let pull = Request {
        scheduling: true,
        ..rest.clone()
    };
    assert_eq!(read(&mut reader, &pull), (vec![true], true), "a pull");
    let lens = |r| Request {
        lenses: vec![r],
        ..rest.clone()
    };
    let cpu = lens(aterm_vitals::Resource::Cpu);
    assert_eq!(read(&mut reader, &cpu), (vec![true], true), "the CPU lens");
    let memory = lens(aterm_vitals::Resource::Memory);
    assert_eq!(
        read(&mut reader, &memory),
        (vec![false], false),
        "another lens"
    );
    reader
        .vitals
        .record_cpu_policy(aterm_vitals::sys::json::Jv::obj([(
            "schema",
            aterm_vitals::sys::json::Jv::Int(1),
        )]));
    assert_eq!(
        read(&mut reader, &rest),
        (vec![true], true),
        "a CPU policy's report"
    );
}

/// A valid cut of this process's threads, its `n`th: stamped on `epoch`'s clock as it begins,
/// each counter a step past the last cut's, as aterm's own-thread reader cuts them.
fn own_cut(epoch: Instant, n: u64) -> aterm_vitals::self_cpu::OwnCpuCut {
    use aterm_vitals::self_cpu::{
        OwnCpuCounter, OwnCpuCoverage, OwnCpuCut, OwnCpuProcess, OwnCpuThread,
    };
    let begin_ns = u64::try_from(epoch.elapsed().as_nanos()).expect("ns");
    let counter = |ns| OwnCpuCounter {
        ns,
        precision_ns: 0,
    };
    OwnCpuCut {
        process: OwnCpuProcess {
            pid: std::process::id(),
            start: 1,
            version: 1,
        },
        begin_ns,
        end_ns: begin_ns + 1,
        process_before: counter(n * 1_000),
        process_after: counter(n * 1_000 + 1),
        threads: vec![OwnCpuThread {
            id: 1,
            name: Some("aterm-vitals".into()),
            cpu: Some(counter(n * 500)),
        }],
        coverage: OwnCpuCoverage {
            listed: 1,
            inspected: 1,
            unread: 0,
            capped: false,
        },
    }
}

/// A RESTING READING READS NEITHER ATERM'S OWN THREADS NOR THE NEURAL ENGINE: through counting
/// stand-ins for both readers, a whole reading at rest, one with the deck shown, one with the
/// CPU lens open, a machine-only one and a pull no query armed make no read of aterm's threads
/// and no read of the engine's power, each snapshot saying its threads went unread
/// (`not_sampled`) and carrying the engine's one reading from the reader's start, which matched
/// the engine and read its power once. NEGATIVE CONTROLS: aterm's threads read at every reading,
/// as before, read 5 at rest; the engine read at every reading, as before, reads its power 5
/// times.
#[test]
fn a_resting_reading_reads_no_self_cpu_and_no_neural_engine_power() {
    use aterm_sysprobe::neural_engine::{Entry, Power, Source as AneSource};
    use std::sync::atomic::Ordering;
    static THREADS: AtomicUsize = AtomicUsize::new(0);
    static ENTRY: AtomicUsize = AtomicUsize::new(0);
    static POWER: AtomicUsize = AtomicUsize::new(0);
    fn own_threads(epoch: Instant) -> Result<aterm_vitals::self_cpu::OwnCpuCut, &'static str> {
        let n = THREADS.fetch_add(1, Ordering::SeqCst) + 1;
        Ok(own_cut(epoch, u64::try_from(n).expect("count")))
    }
    fn entry() -> Option<Entry> {
        ENTRY.fetch_add(1, Ordering::SeqCst);
        Some(Entry { cores: Some(16) })
    }
    fn power() -> Power {
        POWER.fetch_add(1, Ordering::SeqCst);
        Power {
            current: Some(1),
            max: Some(4),
        }
    }
    let mut reader = Reader::new(Arc::default());
    reader.self_cpu_read = self_cpu::Probe {
        read: own_threads,
        reads: true,
    };
    reader.ane = ane_at_start(Some(aterm_sysprobe::neural_engine::Reader::on(AneSource {
        entry,
        power,
    })));
    let at_start = (ENTRY.load(Ordering::SeqCst), POWER.load(Ordering::SeqCst));
    let rest = Request {
        self_pid: std::process::id(),
        cadence_ms: 1_000,
        ..Request::default()
    };
    let deck = Request {
        deck: true,
        in_view: true,
        ..rest.clone()
    };
    let lens = Request {
        lenses: vec![aterm_vitals::Resource::Cpu],
        ..deck.clone()
    };
    let machine = Request {
        machine_only: true,
        ..rest.clone()
    };
    let mut snaps = vec![
        reader.reading(&rest),
        reader.reading(&deck),
        reader.reading(&lens),
        reader.reading(&machine),
    ];
    snaps.push(pull(
        &mut reader,
        &mut deck.clone(),
        Some(Instant::now()),
        false,
    ));
    assert_eq!(snaps.len(), 5);
    assert_eq!(
        THREADS.load(Ordering::SeqCst),
        0,
        "no read of aterm's threads at rest"
    );
    assert_eq!(
        POWER.load(Ordering::SeqCst) - at_start.1,
        0,
        "no read of the engine's power at rest"
    );
    let start = Some(Ok(aterm_vitals::AneSample {
        cores: 16,
        powered: true,
    }));
    for (i, snap) in snaps.iter().enumerate() {
        assert!(!snap.self_cpu_read, "reading {i}");
        assert_eq!(
            snap.self_cpu,
            aterm_vitals::self_cpu::OwnCpuView::Unavailable("not_sampled"),
            "reading {i}"
        );
        assert_eq!(snap.ane, start, "reading {i}: the start's reading");
    }
    assert_eq!(
        at_start,
        (1, 1),
        "the start matched the engine and read it once"
    );
}

/// A SELF-CPU READER THAT READS NOTHING ANSWERS EVERY READING (the native reader off macOS): with
/// a stand-in that reads nothing and answers `unsupported`, a resting reading and a pull no query
/// armed each say `unsupported` for aterm's threads, as every reading said before these reads
/// waited to be asked, and that no pull asked (`self_cpu_read` false). NEGATIVE CONTROL: a
/// reading that takes the reader's answer only where a pull asked says `not_sampled`.
#[test]
fn a_self_cpu_reader_that_reads_nothing_answers_every_reading() {
    use aterm_vitals::self_cpu::OwnCpuView;
    let mut reader = Reader::new(Arc::default());
    reader.self_cpu_read = self_cpu::Probe {
        read: |_| Err("unsupported"),
        reads: false,
    };
    let req = Request {
        self_pid: std::process::id(),
        cadence_ms: 1_000,
        deck: true,
        in_view: true,
        ..Request::default()
    };
    let rest = reader.reading(&req);
    let pulled = pull(&mut reader, &mut req.clone(), Some(Instant::now()), false);
    for (what, snap) in [("rest", &rest), ("pull", &pulled)] {
        assert!(!snap.self_cpu_read, "{what}: no pull asked");
        assert_eq!(
            snap.self_cpu,
            OwnCpuView::Unavailable("unsupported"),
            "{what}"
        );
    }
}

/// A PULL THAT PRINTS ATERM'S OWN THREADS READS ITS OWN PAIR: after a resting reading, which
/// reads none, a pull a `sys why` armed reads them twice, [`COLD_PAIR`] apart, though its whole
/// reading is warm, and its `sys why cpu` answer shows them observed from two cuts that both
/// began after the pull began. A resting reading between two such pulls clears the model's
/// continuity, so the second pull, inside [`COLD_AFTER`] of the first, reads its own pair too.
/// NEGATIVE CONTROLS: a pull that arms the read without the cold pair (`Source::self_cpu_cold`
/// never true) reads once, its answer warming; a cold test by time alone (a reading that reads
/// none leaves the last cut's time in place) reads once in the second round, its answer warming.
#[test]
fn a_pull_that_prints_self_cpu_reads_its_own_pair() {
    use aterm_vitals::self_cpu::OwnCpuView;
    use std::sync::atomic::Ordering;
    static THREADS: AtomicUsize = AtomicUsize::new(0);
    fn own_threads(epoch: Instant) -> Result<aterm_vitals::self_cpu::OwnCpuCut, &'static str> {
        let n = THREADS.fetch_add(1, Ordering::SeqCst) + 1;
        Ok(own_cut(epoch, u64::try_from(n).expect("count")))
    }
    let mut reader = Reader::new(Arc::default());
    reader.self_cpu_read = self_cpu::Probe {
        read: own_threads,
        reads: true,
    };
    let mut req = Request {
        self_pid: std::process::id(),
        cadence_ms: 1_000,
        deck: true,
        in_view: true,
        ..Request::default()
    };
    let armed = reader.self_cpu_armed.arm();
    let pair_ns = u64::try_from(COLD_PAIR.as_nanos()).expect("ns");
    for round in [
        "after a resting reading",
        "inside COLD_AFTER of the last pull",
    ] {
        let rest = reader.reading(&req);
        assert!(
            !rest.self_cpu_read,
            "{round}: a resting reading reads none, armed or not"
        );
        let reads = THREADS.load(Ordering::SeqCst);
        let began = u64::try_from(reader.epoch.elapsed().as_nanos()).expect("ns");
        let snap = pull(&mut reader, &mut req, Some(Instant::now()), false);
        assert!(snap.self_cpu_read, "{round}");
        let OwnCpuView::Observed(changes) = &snap.self_cpu else {
            panic!("{round}: {:?}", snap.self_cpu);
        };
        assert!(
            changes.before_begin_ns >= began && changes.after_begin_ns >= began,
            "{round}: both cuts began after the pull began at {began}: {changes:?}"
        );
        assert!(
            changes.after_begin_ns - changes.before_end_ns >= pair_ns,
            "{round}: COLD_PAIR apart: {changes:?}"
        );
        let answer = reply_text(&snap, Freshness::Fresh, "why cpu");
        assert!(
            answer.contains("self CPU published counter growth"),
            "{round}: {answer}"
        );
        assert_eq!(THREADS.load(Ordering::SeqCst) - reads, 2, "{round}: a pair");
        assert!(!reader.self_cpu_cold(), "{round}: the pair left a cut warm");
    }
    drop(armed);
}
