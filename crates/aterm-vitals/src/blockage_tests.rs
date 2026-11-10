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
    // Readings with no new pass keep it, dated, until it is three periods old.
    let s = v.absorb(&reading(60, None, DStateRead::Census(None)));
    assert!(matches!(&s.facts.dstate, DState::Read(d) if d.age_ms == 29_500));
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
    // Out of D and back: its clock starts again.
    let _ = v.absorb(&swept(13, false));
    let s = v.absorb(&swept(14, true));
    assert!(matches!(&s.facts.dstate, DState::Read(d) if d.held() == 0));
}
