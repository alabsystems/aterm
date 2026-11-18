// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Memory consumers retain the process engine's original census and identities.

use super::*;
use aterm_agent::harness::native_act::BootStamp;
use aterm_agent::harness::relaunch::ProcessLifetime;
use aterm_session::{LaunchNonce, SessionId};
use aterm_sysprobe::{ProcessReading, Sweep};
use aterm_uds::process::{BootClock, ProcessBirth, ProcessStart};
use std::cell::RefCell;

const SID: &str = "t0000000001";
const MIB: u64 = 1 << 20;
const HANG: Duration = Duration::from_secs(60);

#[derive(Clone, Default)]
struct Evidence {
    continuous_ns: Option<u64>,
    context: Option<memory::Context>,
    roots: Vec<memory::AgentRoot>,
    start: Option<ProcessStart>,
}

thread_local! {
    static EVIDENCE: RefCell<Evidence> = RefCell::new(Evidence::default());
}

struct Restore(Evidence);
impl Drop for Restore {
    fn drop(&mut self) {
        EVIDENCE.with(|slot| *slot.borrow_mut() = self.0.clone());
    }
}

fn evidence(value: Evidence) -> Restore {
    Restore(EVIDENCE.with(|slot| slot.replace(value)))
}

fn roots() -> Vec<memory::AgentRoot> {
    EVIDENCE.with(|slot| slot.borrow().roots.clone())
}

const PROBES: memory::Probes = memory::Probes {
    continuous_ns: || EVIDENCE.with(|slot| slot.borrow().continuous_ns),
    context: || EVIDENCE.with(|slot| slot.borrow().context.clone()),
    roots,
    start: |_| EVIDENCE.with(|slot| slot.borrow().start),
    consoles: |rows, roots| {
        Some(aterm_sysprobe::windows::collect_console_facts(
            rows,
            roots,
            |row| row.start.map(|start| (start, None)).ok_or(()),
        ))
    },
};

fn context() -> memory::Context {
    memory::Context {
        scope: BootStamp {
            host: "macos:11111111111111111111111111111111".into(),
            boot: "22222222222222222222222222222222".into(),
        },
        namespace: None,
    }
}

fn lifetime(pid: u32, birth: u64) -> ProcessLifetime {
    let scope = context().scope;
    aterm_json::from_str(&format!(
        r#"{{"host":"{}","boot":"{}","pid":{pid},"birth":{birth}}}"#,
        scope.host, scope.boot
    ))
    .unwrap()
}

fn known() -> Evidence {
    Evidence {
        continuous_ns: Some(0),
        context: Some(context()),
        roots: vec![memory::AgentRoot {
            sid: SID.into(),
            lifetime: lifetime(100, 500),
        }],
        ..Evidence::default()
    }
}

fn census() -> Sweep {
    Sweep {
        procs: vec![
            ProcessReading {
                pid: 100,
                ppid: 1,
                pgid: 100,
                start: Some(1000),
                birth: Some(500),
                footprint: Some(3 * MIB),
                ..ProcessReading::default()
            },
            ProcessReading {
                pid: 101,
                // A POSIX group retains its adopted member after its parent exits.
                ppid: if cfg!(windows) { 100 } else { 1 },
                pgid: 100,
                start: Some(1010),
                birth: Some(510),
                footprint: Some(5 * MIB),
                ..ProcessReading::default()
            },
            ProcessReading {
                pid: 999,
                ppid: 1,
                pgid: 999,
                start: Some(9990),
                birth: Some(999),
                footprint: Some(17 * MIB),
                ..ProcessReading::default()
            },
        ],
        truncated: false,
    }
}

fn stamp() -> memory::ObservedAt {
    memory::ObservedAt::capture(Instant::now, PROBES.continuous_ns)
}

fn receipt(sweep: &Sweep, seq: u64, panes: Vec<memory::PaneSource>) -> memory::Receipt {
    receipt_at(sweep, seq, panes, stamp())
}

fn receipt_at(
    sweep: &Sweep,
    seq: u64,
    panes: Vec<memory::PaneSource>,
    observed_at: memory::ObservedAt,
) -> memory::Receipt {
    memory::Pending::begin(PROBES, observed_at)
        .finish(sweep)
        .publish(
            &Snapshot {
                seq,
                mem: Some(aterm_vitals::model::MemView {
                    sample: aterm_vitals::model::MemSample {
                        total: (8 + seq) << 30,
                        available: (2 + seq) << 30,
                        app: 1 << 30,
                        wired: 2 << 30,
                        pressure: aterm_vitals::Pressure::Warn,
                        ..aterm_vitals::model::MemSample::default()
                    },
                    rates: Some(aterm_vitals::model::MemRates {
                        swapin_bps: f64::from(u32::try_from(seq).unwrap()) * 1024.0,
                        swapout_bps: 2048.0,
                        ..aterm_vitals::model::MemRates::default()
                    }),
                    ..aterm_vitals::model::MemView::default()
                }),
                ..Snapshot::default()
            },
            panes,
        )
}

#[test]
fn one_engine_processes_keeps_the_census_when_later_or_display_readings_differ() {
    let _restore = evidence(known());
    let first = receipt(&census(), 1, Vec::new());
    assert_eq!(
        first.agent(SID, &lifetime(100, 500)).unwrap().footprint_mib,
        8
    );
    let paired = first.agent(SID, &lifetime(100, 500)).unwrap();
    assert_eq!(paired.physical_mib, Some(9 * 1024));
    assert_eq!(paired.available_mib, Some(3 * 1024));
    assert_eq!(paired.used_mib, Some(3 * 1024));
    assert_eq!(
        paired.pressure,
        Some(aterm_agent::supervise::memory::Pressure::Warn)
    );
    assert_eq!(paired.swap_kib_s, Some(3));

    let mut later = census();
    later.procs[1].footprint = Some(19 * MIB);
    let second = receipt(&later, 2, Vec::new());
    assert_eq!(
        second
            .agent(SID, &lifetime(100, 500))
            .unwrap()
            .footprint_mib,
        22
    );
    assert_eq!(
        first.agent(SID, &lifetime(100, 500)).unwrap().footprint_mib,
        8
    );
    assert_eq!(
        first.agent(SID, &lifetime(100, 500)).unwrap(),
        paired,
        "a later engine reading cannot mix its machine memory or swap into the original receipt"
    );
    let changed = second.agent(SID, &lifetime(100, 500)).unwrap();
    assert_eq!(changed.physical_mib, Some(10 * 1024));
    assert_eq!(changed.available_mib, Some(4 * 1024));
    assert_eq!(changed.swap_kib_s, Some(4));

    let display = Snapshot {
        seq: 3,
        procs: Vec::new(),
        ..Snapshot::default()
    };
    let retained = memory::Pending::begin(PROBES, stamp())
        .finish(&census())
        .publish(&display, Vec::new());
    assert_eq!(
        retained
            .agent(SID, &lifetime(100, 500))
            .unwrap()
            .footprint_mib,
        8
    );
    let missing = retained.agent(SID, &lifetime(100, 500)).unwrap();
    assert_eq!(
        (
            missing.physical_mib,
            missing.available_mib,
            missing.used_mib,
            missing.pressure,
            missing.swap_kib_s
        ),
        (None, None, None, None, None),
        "missing machine counters never start an independent native fallback"
    );
    let mut zero = census();
    for row in &mut zero.procs {
        row.footprint = Some(0);
    }
    assert_eq!(
        receipt(&zero, 4, Vec::new())
            .agent(SID, &lifetime(100, 500))
            .unwrap()
            .footprint_mib,
        0
    );
}

#[test]
fn one_engine_processes_requires_exact_lifetimes_and_complete_cohort_evidence() {
    let _restore = evidence(known());
    for defect in 0..7 {
        let mut sweep = census();
        match defect {
            0 => sweep.procs[0].birth = Some(501),
            1 => sweep.procs[0].start = None,
            2 => sweep.procs[1].footprint = None,
            3 => sweep.procs[1].start = None,
            4 => sweep.truncated = true,
            5 => sweep.procs.push(sweep.procs[0].clone()),
            6 => {
                sweep.procs.remove(0);
            }
            _ => unreachable!(),
        };
        assert!(
            receipt(&sweep, 1, Vec::new())
                .agent(SID, &lifetime(100, 500))
                .is_none(),
            "defect {defect}"
        );
    }
    assert!(
        receipt(&census(), 2, Vec::new())
            .agent(SID, &lifetime(100, 501))
            .is_none()
    );
    for defect in 0..4 {
        let mut input = known();
        match defect {
            0 => input.context = None,
            1 => {
                input.context.as_mut().unwrap().scope.boot =
                    "33333333333333333333333333333333".into()
            }
            2 => input.context.as_mut().unwrap().namespace = Some("7".into()),
            3 => input.roots.push(input.roots[0].clone()),
            _ => unreachable!(),
        }
        let _restore = evidence(input);
        assert!(
            receipt(&census(), 3, Vec::new())
                .agent(SID, &lifetime(100, 500))
                .is_none(),
            "scope defect {defect}"
        );
    }
    let pending = memory::Pending::begin(PROBES, stamp());
    EVIDENCE.with(|slot| slot.borrow_mut().context = None);
    assert!(
        pending
            .finish(&census())
            .publish(&Snapshot::default(), Vec::new())
            .agent(SID, &lifetime(100, 500))
            .is_none()
    );
}

#[test]
fn one_engine_processes_pairs_wall_birth_with_the_same_raw_mach_start() {
    let mut input = known();
    input.start = Some(ProcessStart {
        boot: BootClock(1000),
        wall: Some(ProcessBirth {
            seconds: 0,
            microseconds: 500,
        }),
    });
    let _restore = evidence(input);
    let mut sweep = census();
    sweep.procs[0].birth = None;
    assert_eq!(
        receipt(&sweep, 1, Vec::new())
            .agent(SID, &lifetime(100, 500))
            .unwrap()
            .footprint_mib,
        8
    );
    EVIDENCE.with(|slot| slot.borrow_mut().start.as_mut().unwrap().boot = BootClock(1001));
    assert!(
        receipt(&sweep, 2, Vec::new())
            .agent(SID, &lifetime(100, 500))
            .is_none(),
        "a later PID observation cannot fill this census's generation"
    );
    EVIDENCE.with(|slot| slot.borrow_mut().start = None);
    assert!(
        receipt(&sweep, 3, Vec::new())
            .agent(SID, &lifetime(100, 500))
            .is_none()
    );
}

#[test]
fn one_engine_processes_nonce_binds_only_the_sampled_unique_live_generation() {
    let _restore = evidence(known());
    let sid = SessionId::parse(SID).unwrap();
    let nonce = LaunchNonce::from_hex("11111111111111111111111111111111").unwrap();
    let other = LaunchNonce::from_hex("22222222222222222222222222222222").unwrap();
    let pane = memory::PaneSource {
        local: 7,
        sid: sid.clone(),
        nonce,
    };
    let valid = receipt(&census(), 1, vec![pane.clone()]);
    assert_eq!(valid.nonce_for(7, &sid, nonce), Some(nonce.to_hex()));
    assert_eq!(
        valid.nonce_for(7, &sid, other),
        None,
        "new live nonce does not freshen old bytes"
    );
    assert_eq!(valid.nonce_for(8, &sid, nonce), None);
    assert_eq!(
        valid.nonce_for(7, &SessionId::parse("t0000000002").unwrap(), nonce),
        None
    );
    for panes in [
        Vec::new(),
        vec![pane.clone(), pane.clone()],
        vec![pane.clone(), memory::PaneSource { local: 8, ..pane }],
    ] {
        assert_eq!(receipt(&census(), 2, panes).nonce_for(7, &sid, nonce), None);
    }
}

#[test]
fn one_engine_processes_memory_pull_is_one_cold_read_and_machine_frames_do_not_freshen_it() {
    static COPY: std::sync::LazyLock<crate::sys_host::Shared> =
        std::sync::LazyLock::new(|| crate::sys_host::Shared::with_clock(|| Some(0)));
    struct Engine {
        model: aterm_vitals::Vitals,
        retained: Arc<memory::Receipt>,
        seen: Arc<Mutex<Vec<(bool, bool)>>>,
        published: std::sync::mpsc::Sender<()>,
        seq: u64,
    }
    impl Source for Engine {
        fn reading(&mut self, req: &Request) -> Snapshot {
            self.seen.lock().unwrap().push((req.machine_only, req.pull));
            self.seq += 1;
            let mut receipt = (*self.retained).clone();
            receipt.seq = self.seq;
            receipt.observed_at = memory::ObservedAt::capture(Instant::now, || Some(0));
            self.retained = Arc::new(receipt);
            Snapshot {
                seq: self.seq,
                machine_only: req.machine_only,
                ..Snapshot::default()
            }
        }
        fn model(&mut self) -> &mut aterm_vitals::Vitals {
            &mut self.model
        }
        fn process_receipt(&self) -> Option<Arc<memory::Receipt>> {
            Some(Arc::clone(&self.retained))
        }
        fn observed_at(&self) -> Option<memory::ObservedAt> {
            Some(self.retained.observed_at)
        }
        fn split_cold(&self) -> bool {
            true
        }
        fn after_publish(&mut self, _: bool) {
            self.published.send(()).unwrap();
        }
    }
    let _restore = evidence(known());
    let original = Arc::new(receipt(&census(), 1, Vec::new()));
    let seen = Arc::new(Mutex::new(Vec::new()));
    let recorded = Arc::clone(&seen);
    let (sent, published) = std::sync::mpsc::channel();
    let posts = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let posted = Arc::clone(&posts);
    let mut host = VitalsHost::default();
    host.start(
        move || Engine {
            model: aterm_vitals::Vitals::new(),
            retained: original,
            seen: recorded,
            published: sent,
            seq: 0,
        },
        move || {
            posted.fetch_add(1, Ordering::SeqCst);
            true
        },
        None,
        Some(&COPY),
    );
    assert!(host.offer(Msg::MemoryPull));
    published
        .recv_timeout(HANG)
        .expect("the first memory reading publishes");
    let first = COPY
        .fresh_memory(Duration::MAX, false)
        .expect("first full reading needs no CPU pair or Deck");
    assert!(first.snap.cpu.is_none());
    assert_eq!(*seen.lock().unwrap(), [(false, true)]);
    assert_eq!(
        first
            .processes
            .as_ref()
            .unwrap()
            .agent(SID, &lifetime(100, 500))
            .unwrap()
            .footprint_mib,
        8
    );

    let mut old = (**first.processes.as_ref().unwrap()).clone();
    old.observed_at = memory::ObservedAt {
        instant: Instant::now()
            .checked_sub(Duration::from_secs(120))
            .unwrap(),
        continuous_ns: Some(0),
    };
    COPY.publish(
        Arc::clone(&first.snap),
        &Machine::default(),
        memory::ObservedAt::capture(Instant::now, || Some(0)),
        None,
        None,
        false,
        Some(Arc::new(old)),
    );
    let prior = COPY.held_now().unwrap().processes.unwrap();
    COPY.publish(
        Arc::new(Snapshot {
            seq: 99,
            machine_only: true,
            ..Snapshot::default()
        }),
        &Machine::default(),
        memory::ObservedAt::capture(Instant::now, || Some(0)),
        None,
        None,
        false,
        None,
    );
    assert_eq!(
        COPY.held_now()
            .unwrap()
            .processes
            .as_ref()
            .unwrap()
            .observed_at,
        prior.observed_at
    );
    // The production pull may exhaust its own short reply budget on a loaded host. The law
    // waits for publication, rather than treating that reply budget as test latency.
    let _ = COPY.fresh_memory(Duration::from_secs(60), false);
    published
        .recv_timeout(HANG)
        .expect("the stale memory pull publishes");
    let refreshed = COPY.held_now().expect("the requested full reading landed");
    assert_eq!(refreshed.snap.seq, 2);
    assert_eq!(*seen.lock().unwrap(), [(false, true), (false, true)]);
    assert_eq!(
        posts.load(Ordering::SeqCst),
        0,
        "a memory pull posts no UI work"
    );
    assert!(host.stop_at_exit(HANG));
}

#[test]
fn one_engine_processes_first_census_spanning_sleep_keeps_its_original_age_and_identity() {
    let _restore = evidence(known());
    let sid = SessionId::parse(SID).unwrap();
    let nonce = LaunchNonce::from_hex("11111111111111111111111111111111").unwrap();
    let original = memory::ObservedAt {
        instant: Instant::now().checked_sub(Duration::from_secs(1)).unwrap(),
        continuous_ns: Some(0),
    };
    let pending = memory::Pending::begin(PROBES, original);
    let inclusive_age = Duration::from_secs(6 * 60 * 60 + 1);
    let inclusive = u64::try_from(inclusive_age.as_nanos()).unwrap();
    EVIDENCE.with(|slot| slot.borrow_mut().continuous_ns = Some(inclusive));
    let retained = pending.finish(&census()).publish(
        &Snapshot {
            seq: 1,
            ..Snapshot::default()
        },
        vec![memory::PaneSource {
            local: 7,
            sid: sid.clone(),
            nonce,
        }],
    );
    let now = memory::ObservedAt {
        instant: original.instant + Duration::from_secs(1),
        continuous_ns: Some(inclusive),
    };
    assert_eq!(
        now.instant.duration_since(original.instant),
        Duration::from_secs(1)
    );
    assert_eq!(retained.observed_at, original);
    assert_eq!(retained.observed_at.age_at(now), Some(inclusive_age));
    assert!(!retained.observed_at.within(now, Duration::from_secs(5)));
    assert_eq!(retained.nonce_for(7, &sid, nonce), Some(nonce.to_hex()));
    assert_eq!(
        retained
            .agent(SID, &lifetime(100, 500))
            .unwrap()
            .footprint_mib,
        8
    );
    assert!(retained.agent(SID, &lifetime(100, 501)).is_none());

    let shared = crate::sys_host::Shared::with_clock(|| Some((6 * 60 * 60 + 1) * 1_000_000_000));
    shared.publish(
        Arc::new(Snapshot {
            seq: 1,
            ..Snapshot::default()
        }),
        &Machine::default(),
        now,
        None,
        None,
        false,
        Some(Arc::new(retained)),
    );
    assert!(
        shared.fresh_memory(Duration::from_secs(5), false).is_none(),
        "finishing the first census after sleep cannot make its original bytes fresh"
    );
}

#[test]
fn one_engine_processes_sleep_stale_memory_coalesces_a_new_full_census() {
    use std::sync::atomic::AtomicU64;
    static CLOCK: AtomicU64 = AtomicU64::new(0);
    static AWAKE: AtomicU64 = AtomicU64::new(0);
    static EPOCH: std::sync::LazyLock<Instant> = std::sync::LazyLock::new(Instant::now);
    fn instant() -> Instant {
        *EPOCH + Duration::from_secs(AWAKE.load(Ordering::SeqCst))
    }
    fn continuous() -> Option<u64> {
        Some(CLOCK.load(Ordering::SeqCst))
    }
    CLOCK.store(0, Ordering::SeqCst);
    AWAKE.store(0, Ordering::SeqCst);
    let _restore = evidence(known());
    let shared = Arc::new(crate::sys_host::Shared::paced_with_clocks(
        HANG, HANG, instant, continuous,
    ));
    let (sent, requested) = std::sync::mpsc::sync_channel(2);
    shared.install(sent);
    let sid = SessionId::parse(SID).unwrap();
    let nonce = LaunchNonce::from_hex("11111111111111111111111111111111").unwrap();
    let pane = memory::PaneSource {
        local: 7,
        sid: sid.clone(),
        nonce,
    };
    let original = Arc::new(receipt_at(
        &census(),
        1,
        vec![pane.clone()],
        memory::ObservedAt::capture(instant, continuous),
    ));
    shared.publish(
        Arc::new(Snapshot {
            seq: 1,
            ..Snapshot::default()
        }),
        &Machine::default(),
        original.observed_at,
        None,
        None,
        false,
        Some(Arc::clone(&original)),
    );
    assert!(shared.fresh_memory(Duration::from_secs(5), false).is_some());

    let inclusive = (6 * 60 * 60 + 1) * 1_000_000_000;
    AWAKE.store(1, Ordering::SeqCst);
    CLOCK.store(inclusive, Ordering::SeqCst);
    shared.publish(
        Arc::new(Snapshot {
            seq: 2,
            machine_only: true,
            ..Snapshot::default()
        }),
        &Machine::default(),
        memory::ObservedAt::capture(instant, continuous),
        None,
        None,
        false,
        None,
    );
    let carried = shared.held_now().unwrap();
    assert_eq!(carried.snap.seq, 1);
    assert_eq!(carried.observed_at, original.observed_at);
    assert!(Arc::ptr_eq(carried.processes.as_ref().unwrap(), &original));

    let start = Arc::new(std::sync::Barrier::new(3));
    let consumers: Vec<_> = (0..2)
        .map(|_| {
            let shared = Arc::clone(&shared);
            let start = Arc::clone(&start);
            std::thread::spawn(move || {
                start.wait();
                shared.fresh_memory(Duration::from_secs(5), false)
            })
        })
        .collect();
    start.wait();
    assert!(
        matches!(requested.recv_timeout(HANG), Ok(Msg::MemoryPull)),
        "sleep-stale memory asks the existing engine for a full census"
    );
    EVIDENCE.with(|slot| slot.borrow_mut().continuous_ns = Some(inclusive));
    let fresh = Arc::new(receipt_at(
        &census(),
        3,
        vec![pane],
        memory::ObservedAt::capture(instant, continuous),
    ));
    shared.publish(
        Arc::new(Snapshot {
            seq: 3,
            ..Snapshot::default()
        }),
        &Machine::default(),
        fresh.observed_at,
        None,
        None,
        false,
        Some(Arc::clone(&fresh)),
    );
    for consumer in consumers {
        let held = consumer
            .join()
            .unwrap()
            .expect("both consumers use the new full census");
        let retained = held.processes.unwrap();
        assert_eq!(held.snap.seq, 3);
        assert!(Arc::ptr_eq(&retained, &fresh));
        assert_eq!(retained.nonce_for(7, &sid, nonce), Some(nonce.to_hex()));
        assert_eq!(
            retained
                .agent(SID, &lifetime(100, 500))
                .unwrap()
                .footprint_mib,
            8
        );
        assert!(retained.agent(SID, &lifetime(100, 501)).is_none());
    }
    assert!(
        requested.try_recv().is_err(),
        "the two consumers share one full-census request"
    );
    shared.uninstall();
}

#[test]
fn one_engine_processes_unread_or_regressing_memory_clock_withholds_freshness() {
    use std::sync::atomic::AtomicU64;
    static CLOCK: AtomicU64 = AtomicU64::new(10);
    fn continuous() -> Option<u64> {
        let now = CLOCK.load(Ordering::SeqCst);
        (now != u64::MAX).then_some(now)
    }
    let _restore = evidence(known());
    for (before, after) in [(None, Some(10)), (Some(10), None), (Some(10), Some(9))] {
        EVIDENCE.with(|slot| slot.borrow_mut().continuous_ns = before);
        let pending = memory::Pending::begin(PROBES, stamp());
        EVIDENCE.with(|slot| slot.borrow_mut().continuous_ns = after);
        let retained = pending.finish(&census()).publish(
            &Snapshot {
                seq: 1,
                ..Snapshot::default()
            },
            Vec::new(),
        );
        assert_eq!(
            retained.observed_at.continuous_ns, None,
            "{before:?} -> {after:?}"
        );
        CLOCK.store(10, Ordering::SeqCst);
        let shared = crate::sys_host::Shared::with_clock(continuous);
        shared.publish(
            Arc::new(Snapshot {
                seq: 1,
                ..Snapshot::default()
            }),
            &Machine::default(),
            memory::ObservedAt::now(Instant::now, continuous),
            None,
            None,
            false,
            Some(Arc::new(retained)),
        );
        assert!(
            shared.fresh_memory(Duration::MAX, false).is_none(),
            "a valid publication clock cannot repair missing or regressing capture evidence"
        );
    }
    EVIDENCE.with(|slot| slot.borrow_mut().continuous_ns = Some(10));
    CLOCK.store(10, Ordering::SeqCst);
    let retained = Arc::new(receipt(&census(), 2, Vec::new()));
    let shared = crate::sys_host::Shared::with_clock(continuous);
    shared.publish(
        Arc::new(Snapshot {
            seq: 2,
            ..Snapshot::default()
        }),
        &Machine::default(),
        retained.observed_at,
        None,
        None,
        false,
        Some(Arc::clone(&retained)),
    );
    assert!(shared.fresh_memory(Duration::MAX, false).is_some());
    for now in [u64::MAX, 9] {
        CLOCK.store(now, Ordering::SeqCst);
        assert!(
            shared.fresh_memory(Duration::MAX, false).is_none(),
            "the current memory clock must also be readable and ordered: {now}"
        );
        assert!(Arc::ptr_eq(
            shared.held_now().unwrap().processes.as_ref().unwrap(),
            &retained
        ));
    }
}

/// The former frontend measure's real stand-in now reaches its consumer through Reader.
#[cfg(target_os = "macos")]
#[test]
fn one_engine_processes_live_reader_feeds_the_stand_ins_group_and_ceiling() {
    use aterm_agent::supervise::memory::{self as ceiling, AgentMemory, MemoryCeiling};
    if !crate::test_alone::alone() {
        return;
    }
    // This isolated fixture supplies only the already captured supervised root. The real
    // process worker still owns the census, native birth pairing and memory publication.
    static ROOTS: Mutex<Vec<memory::AgentRoot>> = Mutex::new(Vec::new());
    fn live_roots() -> Vec<memory::AgentRoot> {
        ROOTS.lock().unwrap_or_else(PoisonError::into_inner).clone()
    }
    struct ClearRoots;
    impl Drop for ClearRoots {
        fn drop(&mut self) {
            ROOTS.lock().unwrap_or_else(PoisonError::into_inner).clear();
        }
    }
    let _clear = ClearRoots;
    struct Parked(aterm_uds::spawnfd::Child);
    impl Drop for Parked {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
    let mut reader = Reader::new(Arc::default());
    reader.procs = ProcsRead::lane(HANG);
    reader.split.probe.sweep = aterm_sysprobe::processes_keeping;
    reader.memory_probes = memory::Probes {
        roots: live_roots,
        ..memory::Probes::NATIVE
    };
    let req = Request {
        self_pid: std::process::id(),
        pull: true,
        ..Request::default()
    };
    let stand_in = |reader: &mut Reader, mib: u64| {
        let script = format!(
            "my $x = \"a\" x ({mib} * 1024 * 1024); $| = 1; print \"ready\\n\"; my $b; sysread(STDIN, $b, 1);"
        );
        let mut child = Parked(
            aterm_uds::spawnfd::command("/usr/bin/perl")
                .args(["-e", &script])
                .stdin(aterm_uds::spawnfd::Stdio::piped())
                .stdout(aterm_uds::spawnfd::Stdio::piped())
                .process_group(0)
                .spawn()
                .expect("perl"),
        );
        aterm_uds::deadline::prints(&mut child.0, "ready", HANG);
        let lifetime =
            ProcessLifetime::capture(child.0.id()).expect("the stand-in's precise lifetime");
        *ROOTS.lock().unwrap_or_else(PoisonError::into_inner) = vec![memory::AgentRoot {
            sid: SID.into(),
            lifetime: lifetime.clone(),
        }];
        reader.read(&req);
        let measured = reader
            .process_receipt()
            .and_then(|r| r.agent(SID, &lifetime));
        let _ = child.0.kill();
        child.0.wait().unwrap();
        (measured, lifetime)
    };
    let small_mac = |m: AgentMemory| AgentMemory {
        physical_mib: Some(1024),
        available_mib: Some(1024),
        used_mib: Some(1024),
        pressure: Some(ceiling::Pressure::Normal),
        ..m
    };
    let (big, gone) = stand_in(&mut reader, 640);
    let big = big.expect("engine captured the stand-in's group");
    assert!(big.footprint_mib >= 600, "{big:?}");
    assert!(big.physical_mib.is_some_and(|p| p >= 4096), "{big:?}");
    assert!(
        big.available_mib.is_some() && big.used_mib.is_some_and(|u| u > 0),
        "{big:?}"
    );
    let over = ceiling::judge(MemoryCeiling::Auto, &small_mac(big)).expect("over 512 MiB");
    assert_eq!(over.ceiling_mib, 512);
    assert!(
        !over.acts(),
        "a warning while the machine is not in trouble"
    );
    let (small, _) = stand_in(&mut reader, 16);
    let small = small.expect("engine captured the smaller group");
    assert!(small.footprint_mib < 256, "{small:?}");
    assert_eq!(ceiling::judge(MemoryCeiling::Auto, &small_mac(small)), None);
    *ROOTS.lock().unwrap_or_else(PoisonError::into_inner) = vec![memory::AgentRoot {
        sid: SID.into(),
        lifetime: gone.clone(),
    }];
    reader.read(&req);
    assert_eq!(
        reader.process_receipt().unwrap().agent(SID, &gone),
        None,
        "a ceased root has no memory reading"
    );
}
