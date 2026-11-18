// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Blockage's laws (A4 §6.5): zombies counted machine-wide, by parent, once aged; the census's
//! hold and staleness; Linux's sweep states.

use super::*;
use crate::model::{Snapshot, Vitals};

const T0: i64 = 1_792_000_800_000;
const SELF_PID: u32 = 500;
const SHELL: u32 = 600;

fn proc(pid: u32, ppid: u32, name: &str, state: ProcState) -> ProcSample {
    ProcSample {
        pid,
        ppid,
        name: name.into(),
        state,
        start: Some(u64::from(pid) * 7),
        ..ProcSample::default()
    }
}

/// aterm (500), its tab's shell (600) and a build under it (601), and another user's app (900)
/// outside every tab; `zombies` children of the build and `other` of the app, each a zombie.
fn sweep(zombies: u32, other: u32) -> Vec<ProcSample> {
    let mut p = vec![
        proc(1, 0, "launchd", ProcState::Run),
        proc(SELF_PID, 1, "aterm", ProcState::Run),
        proc(SHELL, SELF_PID, "zsh", ProcState::Run),
        proc(601, SHELL, "make", ProcState::Run),
        proc(900, 1, "Helper", ProcState::Run),
    ];
    p.extend((0..zombies).map(|i| proc(2000 + i, 601, "cc", ProcState::Zombie)));
    p.extend((0..other).map(|i| proc(3000 + i, 900, "worker", ProcState::Zombie)));
    p
}

/// Reading `t` (seconds) with `procs` swept and `dstate` read.
fn reading(t: u64, procs: Option<Vec<ProcSample>>, dstate: DStateRead) -> Sample {
    Sample {
        at_ns: t * 1_000_000_000,
        mono_ms: t * 1000,
        at_unix_ms: T0 + i64::try_from(t).expect("small") * 1000,
        cadence_ms: 1000,
        self_pid: SELF_PID,
        tabs: vec![TabRoot {
            key: 41,
            tab: 41,
            tab_id: Some(41),
            shell_pid: SHELL,
            label: "Tab 1".into(),
            program: "make".into(),
            agent_leader: None,
        }],
        procs,
        dstate,
        ..Sample::default()
    }
}

fn job_reading(t: u64, mut procs: Vec<ProcSample>) -> Sample {
    for process in &mut procs {
        process.uid = Some(501);
        process.pgid = if process.pid == 601 || (2000..3000).contains(&process.pid) {
            601
        } else {
            process.pid
        };
        process.sid = if process.pid == SHELL || process.pgid == 601 {
            SHELL
        } else {
            process.pid
        };
    }
    Sample {
        self_uid: Some(501),
        ..reading(t, Some(procs), DStateRead::Unsupported)
    }
}

fn jobs_reading(
    jobs: &mut crate::jobs::Jobs,
    sample: &Sample,
    snapshot: &Snapshot,
    receipt: Arc<ZombieFacts>,
    assessment_ms: u64,
) -> crate::jobs::JobsView {
    let state = |state| match state {
        ProcState::Run => crate::jobs::ProcState::Running,
        ProcState::Stopped => crate::jobs::ProcState::Stopped,
        ProcState::Zombie => crate::jobs::ProcState::Zombie,
        ProcState::Uninterruptible => crate::jobs::ProcState::Uninterruptible,
        ProcState::Unknown => crate::jobs::ProcState::Unknown,
    };
    let rows = sample.procs.as_ref().unwrap();
    jobs.absorb(&crate::jobs::JobTick {
        at_ms: assessment_ms,
        read_ms: assessment_ms.max(sample.mono_ms),
        probe_ms: assessment_ms.max(sample.mono_ms),
        source_seq: Some(snapshot.seq),
        zombie_facts: Some(receipt),
        self_uid: sample.self_uid,
        sweep: rows
            .iter()
            .map(|process| crate::jobs::SweepProc {
                pid: process.pid,
                ppid: process.ppid,
                pgid: process.pgid,
                uid: process.uid,
                name: process.name.clone(),
                state: state(process.state),
                ..crate::jobs::SweepProc::default()
            })
            .collect(),
        procs: rows
            .iter()
            .map(|process| crate::jobs::JobProc {
                pid: process.pid,
                ppid: process.ppid,
                pgid: process.pgid,
                sid: Some(process.sid),
                start: process.start,
                age_ms: Some(0),
                uid: process.uid,
                name: process.name.clone(),
                state: state(process.state),
                tty: Some(SHELL),
                tpgid: Some(SHELL),
                counters: Some(crate::jobs::JobCounters::default()),
                ..crate::jobs::JobProc::default()
            })
            .collect(),
        tabs: vec![crate::jobs::JobTab {
            key: 41,
            shell_pid: SHELL,
            ..crate::jobs::JobTab::default()
        }],
        ..crate::jobs::JobTick::default()
    })
}

fn zombies(s: &Snapshot) -> ZombiesView {
    match &s.facts.zombies {
        Zombies::Read(v) => v.clone(),
        Zombies::Unread(why) => panic!("unread: {why:?}"),
    }
}

/// Today's well and lens filter (`.filter(|z| z.tab.is_some())`), the control: what it counts.
fn tab_filtered(v: &ZombiesView) -> u32 {
    v.piles
        .iter()
        .filter(|z| z.tab.is_some())
        .map(|z| z.count)
        .sum()
}

#[test]
fn zombies_are_counted_machine_wide_by_parent_once_aged() {
    let mut v = Vitals::new();
    let first = v.absorb(&reading(1, Some(sweep(3, 14)), DStateRead::Unsupported));
    let young = zombies(&first);
    assert_eq!(
        (young.count, young.piles.len()),
        (0, 0),
        "seen under 10 s: none yet"
    );

    let s = v.absorb(&reading(11, Some(sweep(3, 14)), DStateRead::Unsupported));
    let z = zombies(&s);
    assert_eq!(z.count, 17, "every parent's, the tab's and another app's");
    assert_eq!(
        z.piles
            .iter()
            .map(|p| (p.ppid, p.name.as_str(), p.count, p.tab))
            .collect::<Vec<_>>(),
        [(900, "Helper", 14, None), (601, "make", 3, Some(41))],
        "the largest pile first, each named, a parent in a tab with its tab"
    );
    assert!(z.whole && z.age_ms == 0);
    // Negative control: the tab filter today's well and lens keep leaves the app's pile dark.
    assert_eq!(tab_filtered(&z), 3);
    assert_ne!(tab_filtered(&z), z.count);

    // A pile found whole reads its count a minute; one that grows reads its growth.
    let helper = |v: &ZombiesView| v.piles.iter().find(|p| p.ppid == 900).cloned();
    assert_eq!(helper(&z).map(|p| p.per_min_deci), Some(140));

    // A reading with no sweep keeps the last sweep's piles, dated.
    let machine = v.absorb(&reading(12, None, DStateRead::Unsupported));
    assert_eq!(zombies(&machine).count, 17);
    assert_eq!(zombies(&machine).age_ms, 1000);

    // The app reaps; the tab's build's zombies are reparented to launchd: forgotten, and new
    // under their new parent until aged again.
    let mut moved = sweep(0, 0);
    moved.extend((0..3).map(|i| proc(2000 + i, 1, "cc", ProcState::Zombie)));
    let s = v.absorb(&reading(13, Some(moved.clone()), DStateRead::Unsupported));
    assert_eq!(zombies(&s).count, 0);
    let s = v.absorb(&reading(23, Some(moved), DStateRead::Unsupported));
    assert_eq!(zombies(&s).count, 3);
    assert_eq!(zombies(&s).piles[0].ppid, 1);
}

#[test]
fn one_zombie_book_skipped_assessments_keep_the_canonical_pile_start_and_rate() {
    let mut model = Vitals::new();
    let initial = job_reading(0, sweep(0, 0));
    let initial_snapshot = model.absorb(&initial);
    let mut jobs = crate::jobs::Jobs::new();
    jobs_reading(
        &mut jobs,
        &initial,
        &initial_snapshot,
        model.current_zombie_facts().unwrap(),
        0,
    );
    let mut old_jobs = ZombieBook::default();
    old_jobs.note(0, []);
    for at in [1, 7] {
        model.absorb(&job_reading(at, sweep(3, 0)));
    }
    let sample = job_reading(11, sweep(3, 0));
    let snap = model.absorb(&sample);
    let held = model.current_zombie_facts().unwrap();
    let pile = held.parent_pile(601).unwrap();
    assert_eq!(
        (held.seq(), held.at_ns(), held.mono_ms()),
        (snap.seq, snap.at_ns, 11_000)
    );
    assert!(held.whole());
    assert_eq!(
        (
            pile.count(),
            pile.since_ms(),
            pile.elapsed_ms(),
            pile.per_min_deci()
        ),
        (3, 1000, 10_000, 30)
    );
    assert_eq!(pile.count(), zombies(&snap).count);
    let assessed = jobs_reading(&mut jobs, &sample, &snap, Arc::clone(&held), 10_999);
    let actual = assessed
        .rows
        .iter()
        .find(|row| row.leader.0 == 601)
        .unwrap()
        .zombies
        .unwrap();
    assert_eq!(
        (actual.count, actual.parent, actual.per_min_deci),
        (pile.count(), pile.parent(), pile.per_min_deci())
    );
    assert_eq!(
        assessed
            .findings
            .zombies
            .iter()
            .find(|pile| pile.ppid == 601)
            .unwrap()
            .count,
        zombies(&snap).count
    );
    assert!(Arc::ptr_eq(assessed.zombie_facts.as_ref().unwrap(), &held));
    old_jobs.note(11_000, (2000..2003).map(|pid| (pid, 601)));
    assert!(
        old_jobs.piles(11_000).is_empty(),
        "the old Jobs-only cadence first sees the already-aged children too late"
    );

    let sample = job_reading(71, sweep(3, 0));
    let snap = model.absorb(&sample);
    let current = model.current_zombie_facts().unwrap();
    let pile = current.parent_pile(601).unwrap();
    assert_eq!(
        (pile.since_ms(), pile.elapsed_ms(), pile.per_min_deci()),
        (1000, 70_000, 25)
    );
    let later = jobs_reading(&mut jobs, &sample, &snap, Arc::clone(&current), 99_000);
    let actual = later
        .rows
        .iter()
        .find(|row| row.leader.0 == 601)
        .unwrap()
        .zombies
        .unwrap();
    assert_eq!(
        (actual.count, actual.per_min_deci),
        (pile.count(), pile.per_min_deci())
    );
    assert!(Arc::ptr_eq(later.zombie_facts.as_ref().unwrap(), &current));
    old_jobs.note(71_000, (2000..2003).map(|pid| (pid, 601)));
    let old_rate = per_min_deci(3, 71_000 - old_jobs.pile_since(601).unwrap());
    assert_eq!(old_rate, 30);
    assert_ne!(
        pile.per_min_deci(),
        old_rate,
        "a second history changes the rate too"
    );
    assert_eq!(
        (held.mono_ms(), held.parent_pile(601).unwrap().elapsed_ms()),
        (11_000, 10_000),
        "a later model reading cannot mutate a queued observation"
    );
}

#[test]
fn one_zombie_book_observed_absence_and_reparenting_reset_age_between_assessments() {
    let mut model = Vitals::new();
    let initial = job_reading(1, sweep(1, 0));
    let initial_snapshot = model.absorb(&initial);
    let mut jobs = crate::jobs::Jobs::new();
    jobs_reading(
        &mut jobs,
        &initial,
        &initial_snapshot,
        model.current_zombie_facts().unwrap(),
        1000,
    );
    let mut old_jobs = ZombieBook::default();
    old_jobs.note(1000, [(2000, 601)]);
    model.absorb(&job_reading(6, sweep(0, 0)));
    let mut reused = sweep(1, 0);
    reused.iter_mut().find(|p| p.pid == 2000).unwrap().start = Some(99_999);
    model.absorb(&job_reading(7, reused.clone()));
    let sample = job_reading(11, reused.clone());
    let snap = model.absorb(&sample);
    let facts = model.current_zombie_facts().unwrap();
    assert!(facts.piles().is_empty());
    assert_eq!(zombies(&snap).count, 0);
    let assessed = jobs_reading(&mut jobs, &sample, &snap, Arc::clone(&facts), 11_000);
    assert!(
        assessed
            .rows
            .iter()
            .find(|row| row.leader.0 == 601)
            .unwrap()
            .zombies
            .is_none()
    );
    assert!(assessed.findings.zombies.is_empty());
    assert!(Arc::ptr_eq(assessed.zombie_facts.as_ref().unwrap(), &facts));
    old_jobs.note(11_000, [(2000, 601)]);
    assert_eq!(
        old_jobs.piles(11_000).get(&601),
        Some(&1),
        "the old history missed the whole-sweep absence and donates the previous age"
    );

    reused.iter_mut().find(|p| p.pid == 2000).unwrap().ppid = 1;
    model.absorb(&job_reading(12, reused.clone()));
    let sample = job_reading(22, reused);
    let snap = model.absorb(&sample);
    let current = model.current_zombie_facts().unwrap();
    assert!(current.parent_pile(601).is_none());
    let moved = current.parent_pile(1).unwrap();
    assert_eq!(
        (moved.count(), moved.since_ms(), moved.elapsed_ms()),
        (1, 12_000, 10_000)
    );
    let assessed = jobs_reading(&mut jobs, &sample, &snap, Arc::clone(&current), 22_000);
    assert!(
        assessed
            .rows
            .iter()
            .find(|row| row.leader.0 == 601)
            .unwrap()
            .zombies
            .is_none()
    );
    let actual = assessed
        .findings
        .zombies
        .iter()
        .find(|pile| pile.ppid == 1)
        .unwrap();
    assert_eq!(
        (actual.count, actual.per_min_deci),
        (moved.count(), moved.per_min_deci())
    );
    old_jobs.note(22_000, [(2000, 1)]);
    assert!(
        old_jobs.piles(22_000).is_empty(),
        "a skipped reparenting is dated too late"
    );
}

#[test]
fn one_zombie_book_receipts_never_reage_at_an_assessments_clock() {
    for (canonical, other, counted) in [(9999, 10_000, 0), (10_000, 9999, 1)] {
        let mut model = Vitals::new();
        model.absorb(&reading(0, Some(sweep(1, 0)), DStateRead::Unsupported));
        let mut sample = reading(10, Some(sweep(1, 0)), DStateRead::Unsupported);
        sample.mono_ms = canonical;
        sample.at_ns = canonical * 1_000_000;
        let snap = model.absorb(&sample);
        let facts = model.current_zombie_facts().unwrap();
        assert_eq!(facts.mono_ms(), canonical);
        assert_eq!(facts.parent_pile(601).map_or(0, ZombiePile::count), counted);
        assert_eq!(zombies(&snap).count, counted);
        let mut mutable_clock_control = ZombieBook::default();
        mutable_clock_control.note(0, [(2000, 601)]);
        assert_ne!(
            mutable_clock_control
                .piles(other)
                .get(&601)
                .copied()
                .unwrap_or(0),
            counted,
            "an earlier assessment start or later detail-probe clock crosses the wrong cutoff"
        );
        assert_eq!(facts.parent_pile(601).map_or(0, ZombiePile::count), counted);
    }
}

#[test]
fn one_zombie_book_current_receipts_distinguish_missing_unsupported_and_empty() {
    let mut model = Vitals::new();
    let absent = model.absorb(&reading(1, None, DStateRead::Unsupported));
    assert!(model.current_zombie_facts().is_none());
    assert_eq!(absent.facts.zombies, Zombies::Unread(Unread::NotSampled));
    let empty = model.absorb(&reading(2, Some(Vec::new()), DStateRead::Unsupported));
    let empty_facts = model.current_zombie_facts().unwrap();
    assert_eq!(
        (empty_facts.seq(), empty_facts.mono_ms()),
        (empty.seq, 2000)
    );
    assert!(empty_facts.whole() && empty_facts.piles().is_empty());
    assert_eq!(zombies(&empty).count, 0);
    let unsupported = model.absorb(&Sample {
        zombie_read: ZombieRead::Unsupported,
        ..reading(3, Some(Vec::new()), DStateRead::Unsupported)
    });
    assert!(model.current_zombie_facts().is_none());
    assert_eq!(
        unsupported.facts.zombies,
        Zombies::Unread(Unread::Unsupported)
    );
    assert_eq!(
        (empty_facts.seq(), empty_facts.mono_ms()),
        (empty.seq, 2000)
    );

    model.absorb(&reading(4, Some(sweep(3, 0)), DStateRead::Unsupported));
    let full = model.absorb(&reading(14, Some(sweep(3, 0)), DStateRead::Unsupported));
    let full_facts = model.current_zombie_facts().unwrap();
    let unread = model.absorb(&reading(15, None, DStateRead::Unsupported));
    assert!(
        model.current_zombie_facts().is_none(),
        "an absent/refused current census has no new receipt"
    );
    assert_eq!((zombies(&unread).count, zombies(&unread).age_ms), (3, 1000));
    assert_eq!(
        (full_facts.seq(), full_facts.at_ns(), full_facts.mono_ms()),
        (full.seq, full.at_ns, 14_000)
    );
}

#[test]
fn one_zombie_book_receipts_keep_every_parent_and_the_cap_recovery_qualification() {
    let mut many = sweep(0, 0);
    for parent in 10_000..10_032 {
        many.push(proc(parent, 1, "parent", ProcState::Run));
        many.push(proc(parent + 10_000, parent, "child", ProcState::Zombie));
    }
    let mut model = Vitals::new();
    model.absorb(&reading(1, Some(many.clone()), DStateRead::Unsupported));
    let all = model.absorb(&reading(11, Some(many), DStateRead::Unsupported));
    let facts = model.current_zombie_facts().unwrap();
    assert_eq!(
        facts.piles().len(),
        32,
        "the receipt is not a global display-top list"
    );
    assert_eq!(zombies(&all).count, 32);
    assert_eq!(facts.parent_pile(10_031).unwrap().count(), 1);
    assert!(
        facts
            .piles()
            .windows(2)
            .all(|pair| pair[0].parent() < pair[1].parent())
    );

    let mut model = Vitals::new();
    let mut old_control = ZombieBook::default();
    for at in [0, 70, 80, 81, 91, 150, 151, 160, 170] {
        let mut rows = sweep(if at >= 160 { 4 } else { 3 }, 0);
        if at == 80 {
            rows.retain(|p| p.pid != 2000);
        }
        old_control.note(
            at * 1000,
            rows.iter()
                .filter(|p| p.state == ProcState::Zombie)
                .map(|p| (p.pid, p.ppid)),
        );
        let snap = model.absorb(&Sample {
            procs_truncated: at == 80,
            ..reading(at, Some(rows), DStateRead::Unsupported)
        });
        let facts = model.current_zombie_facts().unwrap();
        assert_eq!(facts.whole(), at != 80);
        let grows = facts.piles().iter().any(ZombiePile::growing);
        assert_eq!(grows, zombies(&snap).growing);
        assert_eq!(grows, at == 170, "canonical cap recovery at {at}");
        if at == 150 {
            assert!(
                old_control.grows(601, at * 1000),
                "the old unqualified book falsely grows"
            );
            assert!(!grows);
        }
    }
}

#[test]
fn no_sweep_yet_is_unread_not_none() {
    let mut v = Vitals::new();
    let s = v.absorb(&reading(1, None, DStateRead::Unsupported));
    assert_eq!(s.facts.zombies, Zombies::Unread(Unread::NotSampled));
    assert_eq!(s.facts.zombies.count(), None);
    assert_eq!(s.facts.dstate, DState::Unread(Unread::Unsupported));
}

/// A host whose sweep reads no process state (Windows: every state `Unknown`) says the count is
/// unsupported, never 0. CONTROL: the same sweep from a host that reads states counts 0.
#[test]
fn a_sweep_that_reads_no_state_counts_no_zombie_as_zero() {
    let blind: Vec<ProcSample> = sweep(0, 0)
        .into_iter()
        .map(|p| ProcSample {
            state: ProcState::Unknown,
            ..p
        })
        .collect();
    let mut v = Vitals::new();
    let s = v.absorb(&Sample {
        zombie_read: ZombieRead::Unsupported,
        ..reading(1, Some(blind.clone()), DStateRead::Unsupported)
    });
    assert_eq!(s.facts.zombies, Zombies::Unread(Unread::Unsupported));
    let mut v = Vitals::new();
    let s = v.absorb(&reading(1, Some(blind), DStateRead::Unsupported));
    assert_eq!(zombies(&s).count, 0, "control");
}

#[test]
fn the_book_forgets_a_zombie_gone_and_its_parents_pile() {
    let mut b = ZombieBook::default();
    b.note(0, [(10, 1), (11, 1), (12, 2)]);
    assert_eq!(b.piles(ZOMBIE_AGE_MS - 1).len(), 0);
    assert_eq!(
        b.piles(ZOMBIE_AGE_MS).into_iter().collect::<Vec<_>>(),
        [(1, 2), (2, 1)]
    );
    b.note(5_000, [(10, 1)]);
    assert_eq!(b.seen(11), None);
    assert_eq!(b.pile_since(2), None, "no zombie left under 2");
    assert_eq!(b.pile_since(1), Some(0), "1's pile began with its first");
    assert_eq!(
        per_min_deci(6, 30_000),
        60,
        "under a minute reads the minute"
    );
    assert_eq!(per_min_deci(6, 120_000), 30);
}

fn pass(at_mono_ms: u64, marked: &[(u32, bool)]) -> DStateRead {
    DStateRead::Census(Some(Arc::new(CensusPass {
        at_mono_ms,
        marked: marked
            .iter()
            .map(|&(pid, held)| CensusMark {
                pid,
                name: format!("p{pid}"),
                uninterruptible: 1,
                cut: false,
                held,
            })
            .collect(),
        listed: 1_850,
        read: 600,
        thread_ns: 4_500_000,
        ..CensusPass::default()
    })))
}

#[test]
fn the_census_stands_for_now_only_while_fresh() {
    let mut v = Vitals::new();
    let s = v.absorb(&reading(1, None, DStateRead::Census(None)));
    assert_eq!(
        s.facts.dstate,
        DState::Unread(Unread::NotSampled),
        "no pass yet"
    );

    let s = v.absorb(&reading(31, None, pass(30_500, &[(77, true), (78, false)])));
    let DState::Read(d) = &s.facts.dstate else {
        panic!("{:?}", s.facts.dstate)
    };
    assert_eq!(d.scope, DScope::Own);
    assert_eq!(d.held(), 1, "held: two passes running");
    assert_eq!(d.age_ms, 500);
    assert_eq!(d.procs[0].threads, Some(1));
    // Readings with no new pass keep it, dated, until it is three periods (90 s) old: it stands
    // at exactly 90 s, and is gone past them.
    let s = v.absorb(&reading(60, None, DStateRead::Census(None)));
    assert!(matches!(&s.facts.dstate, DState::Read(d) if d.age_ms == 29_500));
    let at_three = Sample {
        at_ns: 120_500_000_000,
        mono_ms: 120_500,
        at_unix_ms: T0 + 120_500,
        ..reading(120, None, DStateRead::Census(None))
    };
    let s = v.absorb(&at_three);
    assert!(
        matches!(&s.facts.dstate, DState::Read(d) if d.age_ms == 90_000),
        "{:?}",
        s.facts.dstate
    );
    let s = v.absorb(&reading(121, None, DStateRead::Census(None)));
    assert_eq!(s.facts.dstate, DState::Unread(Unread::NotSampled), "stale");

    // A list the OS refused reads refused, never an empty census.
    let refused = DStateRead::Census(Some(Arc::new(CensusPass {
        at_mono_ms: 121_000,
        list_refused: true,
        ..CensusPass::default()
    })));
    let s = v.absorb(&reading(121, None, refused));
    assert_eq!(s.facts.dstate, DState::Unread(Unread::Refused));
}

#[test]
fn linux_holds_a_d_state_seen_ten_seconds_and_counts_procs_blocked() {
    let mut v = Vitals::new();
    let swept = |t: u64, d: bool| {
        let mut p = sweep(0, 0);
        if d {
            p.push(proc(700, 1, "rsync", ProcState::Uninterruptible));
        }
        reading(t, Some(p), DStateRead::Sweep { blocked: Some(3) })
    };
    let s = v.absorb(&reading(1, None, DStateRead::Sweep { blocked: Some(3) }));
    assert_eq!(
        s.facts.dstate,
        DState::Unread(Unread::NotSampled),
        "no sweep yet"
    );
    let s = v.absorb(&swept(2, true));
    let DState::Read(d) = &s.facts.dstate else {
        panic!("{:?}", s.facts.dstate)
    };
    assert_eq!(
        (d.scope, d.blocked, d.held()),
        (DScope::Machine, Some(3), 0)
    );
    assert_eq!(d.procs[0].name, "rsync");
    let s = v.absorb(&swept(12, true));
    assert!(
        matches!(&s.facts.dstate, DState::Read(d) if d.held() == 1),
        "10 s"
    );
    // Its pid taken by another process (another start), in D too: a clock of its own.
    let mut reused = sweep(0, 0);
    reused.push(ProcSample {
        start: Some(1),
        ..proc(700, 1, "rsync", ProcState::Uninterruptible)
    });
    let s = v.absorb(&reading(
        13,
        Some(reused),
        DStateRead::Sweep { blocked: Some(3) },
    ));
    assert!(
        matches!(&s.facts.dstate, DState::Read(d) if d.held() == 0),
        "a reused pid"
    );
    // Out of D and back: its clock starts again.
    let _ = v.absorb(&swept(14, false));
    let s = v.absorb(&swept(15, true));
    assert!(matches!(&s.facts.dstate, DState::Read(d) if d.held() == 0));
}

fn pile(count: u32, growing: bool, age_ms: u64) -> Zombies {
    Zombies::Read(ZombiesView {
        count,
        growing,
        whole: true,
        age_ms,
        ..ZombiesView::default()
    })
}

fn waits(held: &[bool], age_ms: u64) -> DState {
    DState::Read(DStateView {
        scope: DScope::Own,
        procs: held
            .iter()
            .zip(70u32..)
            .map(|(&held, pid)| DStuck {
                pid,
                name: format!("p{pid}"),
                threads: Some(1),
                threads_cut: false,
                held,
            })
            .collect(),
        blocked: None,
        age_ms,
        whole: true,
        pass: None,
    })
}

const fn stuck(jobs: u32, busy_orphans: u32, age_ms: u64) -> StuckWork {
    StuckWork {
        jobs,
        busy_orphans,
        age_ms,
    }
}

/// The one per-reading rule (A4 §6.2, §6.5): the worst of a growing pile, stuck work, a steady
/// pile and a held uninterruptible wait, each from a reader that answered within
/// [`BLOCKAGE_STALE_MS`]; clear where at least one answered and none of them blocks.
/// CONTROL: with no reader answering, or each answer too old, the reading is unread, never
/// clear; a wait not yet held, an empty pile and a pass that found nothing stuck block nothing.
#[test]
fn the_blockage_rule_takes_the_worst_kind_a_fresh_reader_found() {
    let none_z = Zombies::default();
    let none_d = DState::default();
    let by = |z: &Zombies, d: &DState, s: Option<StuckWork>| Blocked::judge(z, d, s.as_ref());

    // Each kind alone.
    assert_eq!(
        by(&none_z, &waits(&[false, true], 0), None),
        Blocked::By(Blockage::Uninterruptible)
    );
    assert_eq!(
        by(&pile(3, false, 0), &none_d, None),
        Blocked::By(Blockage::SteadyPile)
    );
    assert_eq!(
        by(&none_z, &none_d, Some(stuck(1, 0, 0))),
        Blocked::By(Blockage::Stuck)
    );
    assert_eq!(
        by(&none_z, &none_d, Some(stuck(0, 2, 0))),
        Blocked::By(Blockage::Stuck)
    );
    assert_eq!(
        by(&pile(3, true, 0), &none_d, None),
        Blocked::By(Blockage::GrowingPile)
    );
    // The worst of those that hold, in the kinds' order.
    assert_eq!(
        by(&pile(3, false, 0), &waits(&[true], 0), None),
        Blocked::By(Blockage::SteadyPile)
    );
    assert_eq!(
        by(&pile(3, false, 0), &waits(&[true], 0), Some(stuck(1, 0, 0))),
        Blocked::By(Blockage::Stuck)
    );
    assert_eq!(
        by(&pile(3, true, 0), &waits(&[true], 0), Some(stuck(1, 1, 0))),
        Blocked::By(Blockage::GrowingPile)
    );
    assert!(Blockage::ALL.is_sorted());
    assert_eq!(
        Blockage::ALL.map(Blockage::critical),
        [false, false, false, true]
    );
    for kind in Blockage::ALL {
        assert_eq!(Blockage::of_rank(kind.rank()), Some(kind));
    }
    assert_eq!((Blockage::of_rank(0), Blockage::of_rank(5)), (None, None));
    assert_eq!(
        Blockage::ALL.map(Blockage::word),
        ["d_state", "steady_pile", "stuck", "growing_pile"]
    );

    // CONTROL: answered, and nothing blocks.
    assert_eq!(by(&pile(0, false, 0), &none_d, None), Blocked::Clear);
    assert_eq!(by(&none_z, &waits(&[false], 0), None), Blocked::Clear);
    assert_eq!(by(&none_z, &none_d, Some(stuck(0, 0, 0))), Blocked::Clear);
    // CONTROL: nobody answered.
    assert_eq!(by(&none_z, &none_d, None), Blocked::Unread);
    assert_eq!(Blocked::default(), Blocked::Unread);
    // An answer stands at exactly the bound and says nothing past it, either way.
    let (at, past) = (BLOCKAGE_STALE_MS, BLOCKAGE_STALE_MS + 1);
    assert_eq!(
        by(&pile(3, true, at), &none_d, None),
        Blocked::By(Blockage::GrowingPile)
    );
    assert_eq!(
        by(
            &pile(3, true, past),
            &waits(&[true], past),
            Some(stuck(4, 0, past))
        ),
        Blocked::Unread
    );
    assert_eq!(
        by(
            &pile(3, true, past),
            &waits(&[false], at),
            Some(stuck(4, 0, past))
        ),
        Blocked::Clear,
        "the fresh reader alone is heard"
    );
    assert_eq!(
        by(
            &pile(0, false, at),
            &waits(&[true], past),
            Some(stuck(1, 0, at))
        ),
        Blocked::By(Blockage::Stuck)
    );

    // What the ring takes of each.
    assert_eq!(Blocked::Unread.reading(), None);
    assert_eq!(Blocked::Clear.reading(), Some((0.0, false)));
    assert_eq!(
        Blocked::By(Blockage::GrowingPile).reading(),
        Some((4.0, true))
    );
}

/// What the jobs pass found stuck: its jobs judged stuck and its busy orphans, dated.
/// CONTROL: a slow job and a job judged fine are no stuck work.
#[test]
fn stuck_work_counts_the_stuck_jobs_and_the_busy_orphans() {
    use crate::jobs::{Class, JobsView, Named, Verdict};
    let row = |verdict| crate::sys::jobs_view::fixtures::row("j1", 41, Class::Running, verdict);
    let mut view = JobsView {
        rows: vec![row(Verdict::Stuck), row(Verdict::Slow), row(Verdict::Stuck)],
        ..JobsView::default()
    };
    assert_eq!(StuckWork::of(&view, 7_000), stuck(2, 0, 7_000));
    assert_eq!(StuckWork::of(&view, 0).count(), 2);
    view.rows.truncate(2);
    view.rows.remove(0);
    assert_eq!(
        StuckWork::of(&view, 0).count(),
        0,
        "control: slow is not stuck"
    );
    view.findings.busy_orphans = vec![Named {
        pid: 812,
        exec_version: None,
        name: "node".into(),
        tab: Some(41),
        job: None,
        for_ms: 240_000,
    }];
    assert_eq!(StuckWork::of(&view, 3_000), stuck(0, 1, 3_000));
    assert_eq!(StuckWork::of(&JobsView::default(), 0).count(), 0);
}

/// A pile grows while it has stood a minute and gained a zombie within the last (each counted
/// once seen [`ZOMBIE_AGE_MS`]), and stands steady once a minute passes with none new.
/// CONTROL: a pile found whole and never added to is steady whatever its size, and a zombie too
/// young to count makes no pile grow.
#[test]
fn a_pile_grows_while_zombies_keep_coming_and_is_steady_once_they_stop() {
    let mut b = ZombieBook::default();
    b.note(0, [(10, 1)]);
    b.note(40_000, [(10, 1), (11, 1)]);
    let stood = ZOMBIE_AGE_MS + PILE_GROWS_MS;
    assert!(!b.grows(1, stood - 1), "the pile has not stood its minute");
    assert!(
        b.grows(1, stood),
        "stood a minute, and 11 came to count 20 s ago"
    );
    assert!(b.grows(1, 40_000 + stood - 1));
    assert!(
        !b.grows(1, 40_000 + stood),
        "a minute with none new: steady"
    );
    assert!(!b.grows(2, stood), "no pile under 2");

    // CONTROL: found whole.
    let mut whole = ZombieBook::default();
    whole.note(0, (20..60).map(|pid| (pid, 2)));
    for at in [stood - 1, stood, stood + 1, 10 * stood] {
        assert!(!whole.grows(2, at), "{at}");
    }
    // CONTROL: one seen 5 s ago does not count yet, so the pile does not grow by it.
    whole.note(stood, (20..61).map(|pid| (pid, 2)));
    assert!(!whole.grows(2, stood + 5_000));
    assert!(whole.grows(2, stood + ZOMBIE_AGE_MS));

    // A parent that reaped its pile starts again.
    b.note(200_000, []);
    b.note(201_000, [(12, 1)]);
    assert!(!b.grows(1, 201_000 + ZOMBIE_AGE_MS));
}

/// A pile its parent reaps as fast as it fills does not grow, however long it stands: a child
/// exits every 20 s and is reaped 30 s later, so one or two zombies stand at every sweep and the
/// pile never counts more than it counted a minute before. Nor does a pile that shrinks while
/// one new zombie joins it.
/// CONTROL: the same arrivals with nothing reaped grow from the pile's first minute on.
#[test]
fn a_pile_reaped_as_fast_as_it_fills_does_not_grow() {
    let (mut turning, mut kept) = (ZombieBook::default(), ZombieBook::default());
    for t in (0..=600_000_u64).step_by(1_000) {
        // Zombie n is first seen at 20 n s; the turning pile's parent reaps it 30 s later.
        let born =
            (0..=u32::try_from(t / 20_000).expect("small")).map(|n| (n, u64::from(n) * 20_000));
        turning.note(
            t,
            born.clone()
                .filter(|&(_, first)| t < first + 30_000)
                .map(|(n, _)| (100 + n, 7)),
        );
        kept.note(t, born.map(|(n, _)| (100 + n, 7)));
        assert!(!turning.grows(7, t), "turning over at {t}");
        assert_eq!(
            kept.grows(7, t),
            t >= ZOMBIE_AGE_MS + PILE_GROWS_MS,
            "CONTROL: nothing reaped, at {t}"
        );
    }
    assert_eq!(turning.pile_since(7), Some(0), "one pile throughout");

    // Twenty found whole; five minutes on the parent reaps nineteen as one new child exits.
    let mut shrinking = ZombieBook::default();
    shrinking.note(0, (0..20).map(|pid| (pid, 9)));
    shrinking.note(300_000, [(0, 9), (20, 9)]);
    // The nineteen that went weigh against it for as long as the one that came is new.
    for at in (300_000..=400_000_u64)
        .step_by(1_000)
        .chain([359_999, 369_999])
    {
        assert!(!shrinking.grows(9, at), "{at}");
    }
    // One more a minute later, with none gone since: it grows again.
    shrinking.note(400_000, [(0, 9), (20, 9), (21, 9)]);
    assert!(shrinking.grows(9, 400_000 + ZOMBIE_AGE_MS));
}

/// The strip's blockage ring through the model (N3): a box keeps the share of its read time a
/// blockage held and the worst kind read in it. The tab's build piles three zombies at 1 s, so
/// they count from 11 s and the pile stands steady; a fourth seen at 55 s counts from 65 s, and
/// at 71 s the pile has stood its minute with that one new in it: it grows from then.
/// CONTROL: the first box, its zombies too young to count, reads clear with no kind; a model
/// that swept nothing and read no census keeps no blockage box at all, never a clear one.
#[test]
fn a_blockage_box_keeps_the_blocked_share_and_the_worst_kind() {
    let box_at = |v: &Vitals, t_s: i64| {
        let k = (T0 + t_s * 1000).div_euclid(crate::ring::BUCKET_MS);
        v.saved_strip(false)
            .blockage
            .buckets
            .iter()
            .find(|(bk, _)| *bk == k)
            .and_then(|(_, agg)| BlockageBox::of(agg))
    };
    let mut v = Vitals::new();
    for t in 1..=100u64 {
        let zombies = if t >= 55 { 4 } else { 3 };
        let s = v.absorb(&reading(
            t,
            Some(sweep(zombies, 0)),
            DStateRead::Unsupported,
        ));
        let want = match t {
            1..=10 => Blocked::Clear,
            11..=70 => Blocked::By(Blockage::SteadyPile),
            _ => Blocked::By(Blockage::GrowingPile),
        };
        assert_eq!(s.facts.blocked, want, "reading {t}");
        assert_eq!(
            zombies_growing(&s),
            t >= 71,
            "the pile column's critical ink, reading {t}"
        );
    }
    // CONTROL: the box of readings 1 to 9 read clear.
    assert_eq!(
        box_at(&v, 0),
        Some(BlockageBox {
            share: 0.0,
            worst: None
        })
    );
    // Readings 10 to 19: clear for the half second reading 10 stands for, blocked the 9 s after.
    let second = box_at(&v, 10).expect("read");
    assert!((second.share - 9.0 / 9.5).abs() < 1e-9, "{second:?}");
    assert_eq!(second.worst, Some(Blockage::SteadyPile));
    // Readings 70 to 79: blocked throughout, the growing pile its worst (reading 70 itself
    // still read it steady); the box before read the steady pile alone.
    assert_eq!(
        box_at(&v, 70),
        Some(BlockageBox {
            share: 1.0,
            worst: Some(Blockage::GrowingPile)
        })
    );
    assert_eq!(
        box_at(&v, 60),
        Some(BlockageBox {
            share: 1.0,
            worst: Some(Blockage::SteadyPile)
        })
    );
    // Stuck work the host handed in outranks the steady pile, and a stale pass is not heard.
    let mut with_jobs = Vitals::new();
    for t in 1..=12u64 {
        let _ = with_jobs.absorb(&reading(t, Some(sweep(3, 0)), DStateRead::Unsupported));
    }
    let fresh = with_jobs.absorb(&Sample {
        stuck_work: Some(stuck(1, 0, 4_000)),
        ..reading(13, Some(sweep(3, 0)), DStateRead::Unsupported)
    });
    assert_eq!(fresh.facts.blocked, Blocked::By(Blockage::Stuck));
    let stale = with_jobs.absorb(&Sample {
        stuck_work: Some(stuck(1, 0, BLOCKAGE_STALE_MS + 1)),
        ..reading(14, Some(sweep(3, 0)), DStateRead::Unsupported)
    });
    assert_eq!(stale.facts.blocked, Blocked::By(Blockage::SteadyPile));

    // CONTROL: nothing read, no box.
    let mut blind = Vitals::new();
    for t in 1..=30u64 {
        let s = blind.absorb(&reading(t, None, DStateRead::Unsupported));
        assert_eq!(s.facts.blocked, Blocked::Unread);
    }
    assert_eq!(blind.saved_strip(false).blockage.buckets, Vec::new());
    assert_eq!(BlockageBox::of(&crate::ring::Agg::default()), None);
}

fn zombies_growing(s: &Snapshot) -> bool {
    zombies(s).growing
}

/// A parent that reaps one child as the next exits keeps one zombie standing and never a
/// growing pile: the one that came counts ten seconds after the one that went left, so the pile
/// dips to none and back, which is no growth a minute on.
/// CONTROL: the same child joining a pile whose first zombie stays is growth, for the minute it
/// is new.
#[test]
fn a_zombie_replaced_one_for_one_is_no_growth() {
    let (mut replaced, mut joined) = (ZombieBook::default(), ZombieBook::default());
    for t in (0..=300_000_u64).step_by(1_000) {
        if t < 100_000 {
            replaced.note(t, [(10, 1)]);
            joined.note(t, [(10, 1)]);
        } else {
            replaced.note(t, [(11, 1)]);
            joined.note(t, [(10, 1), (11, 1)]);
        }
        assert!(!replaced.grows(1, t), "replaced at {t}");
        assert_eq!(
            joined.grows(1, t),
            (100_000 + ZOMBIE_AGE_MS..100_000 + ZOMBIE_AGE_MS + PILE_GROWS_MS).contains(&t),
            "CONTROL: joined at {t}"
        );
    }
    assert_eq!(replaced.pile_since(1), Some(0), "one pile throughout");
}

/// A sweep stopped at its cap tells no growth: the book forgets a zombie the cut sweep did not
/// reach and dates it anew when the next sweep sees it, so the model judges no pile growing at
/// a cut sweep, nor until a whole one after it has stood the time a new zombie counts as new.
/// CONTROL: the book alone, noted the same sweeps, reads the unchanged pile as growing.
#[test]
fn a_cut_sweep_tells_no_growth() {
    let cut = |t: u64| {
        let mut procs = sweep(3, 0);
        procs.retain(|p| p.pid != 2000);
        Sample {
            procs_truncated: true,
            ..reading(t, Some(procs), DStateRead::Unsupported)
        }
    };
    let mut v = Vitals::new();
    let mut book = ZombieBook::default();
    let mut book_grew = Vec::new();
    for t in 1..=260u64 {
        let s = if t == 80 {
            cut(t)
        } else {
            reading(t, Some(sweep(3, 0)), DStateRead::Unsupported)
        };
        let seen = v.absorb(&s);
        let z = zombies(&seen);
        assert!(!z.growing, "reading {t}");
        assert_eq!(z.whole, t != 80);
        book.note(
            t * 1000,
            s.procs
                .iter()
                .flatten()
                .filter(|p| p.state == ProcState::Zombie)
                .map(|p| (p.pid, p.ppid)),
        );
        if book.grows(601, t * 1000) {
            book_grew.push(t);
        }
    }
    assert_eq!(
        book_grew,
        [150],
        "CONTROL: the re-dated zombie outlasts the forgotten one"
    );
    // The same pile gaining a zombie once the cut sweep is that far behind does read growing.
    for t in 261..=300u64 {
        let s = v.absorb(&reading(t, Some(sweep(4, 0)), DStateRead::Unsupported));
        assert_eq!(zombies(&s).growing, t >= 271, "reading {t}");
    }
}

/// A READ CUT AT A CAP SAYS SO, NEVER AN EXACT COUNT: Linux's sweep that stopped at its process
/// cap leaves its `D` processes a lower bound, on that reading and on each later one that reuses
/// it, until a whole sweep runs; a census pass that cut a thread list and found none
/// uninterruptible in what it read does the same, and a mark whose own list was cut counts at
/// least its threads. CONTROL: a whole sweep and a pass with no cut read are whole.
#[test]
fn a_d_state_read_cut_at_a_cap_is_not_whole() {
    let mut v = Vitals::new();
    let swept = |t: u64, truncated: bool| {
        let mut p = sweep(0, 0);
        p.push(proc(700, 1, "rsync", ProcState::Uninterruptible));
        Sample {
            procs_truncated: truncated,
            ..reading(t, Some(p), DStateRead::Sweep { blocked: None })
        }
    };
    let whole = |s: &Snapshot| match &s.facts.dstate {
        DState::Read(d) => (d.whole, d.age_ms, d.procs.len()),
        other => panic!("{other:?}"),
    };
    assert_eq!(whole(&v.absorb(&swept(1, false))), (true, 0, 1));
    assert_eq!(whole(&v.absorb(&swept(2, true))), (false, 0, 1));
    // A reading that took no sweep reuses the cut one, and says it still.
    let later = v.absorb(&reading(3, None, DStateRead::Sweep { blocked: None }));
    assert_eq!(whole(&later), (false, 1_000, 1));
    assert_eq!(whole(&v.absorb(&swept(4, false))), (true, 0, 1));

    let census = |cut_unmarked: u32, cut: bool| {
        DStateRead::Census(Some(Arc::new(CensusPass {
            at_mono_ms: 30_500,
            marked: vec![CensusMark {
                pid: 77,
                name: "java".into(),
                uninterruptible: 256,
                cut,
                held: true,
            }],
            cut_unmarked,
            cover_done: true,
            passes_in_cover: 1,
            ..CensusPass::default()
        })))
    };
    for (cut_unmarked, cut) in [(0, false), (0, true), (2, false)] {
        let mut v = Vitals::new();
        let s = v.absorb(&reading(31, None, census(cut_unmarked, cut)));
        let DState::Read(d) = &s.facts.dstate else {
            panic!("{:?}", s.facts.dstate)
        };
        assert_eq!(d.whole, cut_unmarked == 0, "{cut_unmarked}");
        assert_eq!(
            (d.procs[0].threads, d.procs[0].threads_cut),
            (Some(256), cut)
        );
    }
}
