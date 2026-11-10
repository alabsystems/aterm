// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The census's laws (A4 §6.5, §15; M-DSTATE): its budget stop, its cover, its re-read.

use std::collections::{BTreeMap, BTreeSet};

use super::*;

/// A kernel stand-in: the pids listed, the list's and each read's thread-time cost, which pids
/// hold a thread uninterruptibly, each pid's start, and every pid read, in order.
#[derive(Debug, Default)]
struct Fake {
    pids: Vec<u32>,
    list_ns: u64,
    cost_ns: BTreeMap<u32, u64>,
    stuck: BTreeSet<u32>,
    start: BTreeMap<u32, u64>,
    now_ns: u64,
    reads: Vec<u32>,
}

impl Source for Fake {
    fn list(&mut self, pids: &mut Vec<u32>) -> bool {
        self.now_ns += self.list_ns;
        pids.clear();
        pids.extend(&self.pids);
        true
    }

    fn read(&mut self, pid: u32) -> Read {
        self.now_ns += self.cost_ns.get(&pid).copied().unwrap_or(2_000);
        self.reads.push(pid);
        if !self.pids.contains(&pid) {
            return Read::Gone;
        }
        Read::Threads {
            threads: 4,
            uninterruptible: u32::from(self.stuck.contains(&pid)),
        }
    }

    fn thread_ns(&mut self) -> u64 {
        self.now_ns
    }

    fn identity(&mut self, pid: u32) -> Option<(u64, String)> {
        self.pids.contains(&pid).then(|| {
            (
                self.start.get(&pid).copied().unwrap_or(1),
                format!("p{pid}"),
            )
        })
    }
}

/// N1's L1 desk in numbers: 1,850 own processes, most reads 2–300 µs, a few up to 0.57 ms.
fn desk() -> Fake {
    let pids: Vec<u32> = (100..1950).collect();
    let cost_ns = pids
        .iter()
        .map(|&p| {
            let c = match p % 97 {
                0 => 570_000,
                1..=3 => 300_000,
                _ => 2_000 + u64::from(p % 13) * 900,
            };
            (p, c)
        })
        .collect();
    Fake {
        pids,
        cost_ns,
        ..Fake::default()
    }
}

/// The literal rule, a mutant: read on until the budget is spent, then stop.
fn spent_rule_pass(f: &mut Fake, from: usize, budget: u64) -> (u64, usize) {
    let t0 = f.now_ns;
    let mut i = from;
    while i < f.pids.len() && f.now_ns - t0 < budget {
        let pid = f.pids[i];
        let _ = f.read(pid);
        i += 1;
    }
    (f.now_ns - t0, i)
}

#[test]
fn the_census_stops_before_a_process_that_could_cross_its_budget() {
    let desk_cost: u64 = desk().cost_ns.values().sum();
    let mut census = Census::new(desk());
    let mut covers = 0;
    for _ in 0..60 {
        let p = census.pass();
        assert!(
            p.thread_ns <= PASS_BUDGET_NS,
            "a pass spent {} ns of {PASS_BUDGET_NS}",
            p.thread_ns
        );
        assert!(p.margin_ns >= MARGIN_FLOOR_NS);
        covers += usize::from(p.cover_done);
    }
    // A pass stops only once its spending and the margin pass the budget, so each pass but a
    // cover's last reads more than the budget less the margin: a cover takes at most that many
    // passes (10 on this desk), and 60 passes end at least 5.
    let per_cover = desk_cost.div_ceil(PASS_BUDGET_NS - MARGIN_FLOOR_NS) + 1;
    let least = 60 / usize::try_from(per_cover).expect("few");
    assert!(
        least >= 5 && covers >= least,
        "{covers} covers in 60 passes"
    );

    // Negative control: stopping once the budget is spent overshoots on the same desk.
    let mut f = desk();
    let (mut from, mut over) = (0, 0);
    for _ in 0..60 {
        let (spent, next) = spent_rule_pass(&mut f, from, PASS_BUDGET_NS);
        over += usize::from(spent > PASS_BUDGET_NS);
        from = if next >= f.pids.len() { 0 } else { next };
    }
    assert!(
        over > 0,
        "the literal rule never overshot: the desk tests nothing"
    );
}

#[test]
fn a_cover_reads_every_process_once_and_a_pass_resumes_where_the_last_stopped() {
    let mut census = Census::new(desk());
    let mut passes = Vec::new();
    loop {
        let p = census.pass();
        let done = p.cover_done;
        passes.push(p);
        if done {
            break;
        }
    }
    assert!(passes.len() >= 2, "the desk takes more than one pass");
    let reads = &census.source().reads;
    assert_eq!(reads, &census.source().pids, "each pid once, in order");
    assert_eq!(
        passes.last().map(|p| p.passes_in_cover),
        Some(u32::try_from(passes.len()).expect("few"))
    );
}

#[test]
fn a_pass_always_moves_its_cover() {
    // One process costs more than a whole budget: the margin learns it, and every later pass
    // could stop before its first process. Each still reads one, so the cover ends.
    let mut f = desk();
    f.cost_ns.insert(100, 6_000_000);
    let mut census = Census::new(f);
    let mut done = false;
    for _ in 0..4000 {
        let p = census.pass();
        assert!(p.read >= 1, "a pass read nothing");
        if p.cover_done {
            done = true;
            break;
        }
    }
    assert!(done, "the cover never ended");
}

/// One read that costs a whole budget holds the margin up for two covers at most: the cover it
/// ended in and the next, each pass still reading half a budget of its cover, and the cover after
/// those takes no more passes than the desk's own. CONTROL (mutant): a margin that only grows
/// reads one process a pass from then on, so 200 passes end no second cover.
#[test]
fn one_costly_read_holds_the_margin_up_for_two_covers_at_most() {
    let desk_cost: u64 = desk().cost_ns.values().sum();
    let mut f = desk();
    f.cost_ns.insert(100, PASS_BUDGET_NS);
    let mut census = Census::new(f);
    let first = census.pass();
    assert_eq!(first.costliest_ns, PASS_BUDGET_NS, "the spike was read");
    census.source.cost_ns.insert(100, 2_000);
    let mut covers = Vec::new();
    for _ in 0..200 {
        let p = census.pass();
        assert!(p.margin_ns <= MARGIN_CAP_NS, "margin {}", p.margin_ns);
        if p.cover_done {
            covers.push(p.passes_in_cover);
            if covers.len() == 3 {
                break;
            }
        }
    }
    // Each pass but a cover's last reads more than the budget less the margin.
    let spiked = (desk_cost + PASS_BUDGET_NS).div_ceil(PASS_BUDGET_NS - MARGIN_CAP_NS) + 1;
    let at_cap = desk_cost.div_ceil(PASS_BUDGET_NS - MARGIN_CAP_NS) + 1;
    let at_floor = desk_cost.div_ceil(PASS_BUDGET_NS - MARGIN_FLOOR_NS) + 1;
    let within = |passes: u32, most: u64| u64::from(passes) <= most;
    assert!(
        covers.len() == 3
            && within(covers[0], spiked)
            && within(covers[1], at_cap)
            && within(covers[2], at_floor),
        "passes per cover {covers:?}, at most {spiked}, {at_cap}, {at_floor}"
    );
}

/// The list's own thread time is spent but is no process's read: a list costing 2 ms leaves the
/// costliest read and the margin where the reads put them. CONTROL (mutant): charged to the
/// first read after it, it would be learned as a 2 ms read.
#[test]
fn the_lists_time_is_not_learned_as_a_read() {
    let mut f = desk();
    f.list_ns = 2_000_000;
    let mut census = Census::new(f);
    for _ in 0..20 {
        let p = census.pass();
        assert!(p.costliest_ns <= 570_000, "costliest {}", p.costliest_ns);
        assert_eq!(p.margin_ns, MARGIN_FLOOR_NS);
        assert!(p.thread_ns <= PASS_BUDGET_NS, "spent {}", p.thread_ns);
    }
}

/// A mark whose re-read alone costs more than the budget does not stop the cover: after it each
/// pass still reads the cover's next process, so the cover ends. CONTROL (mutant): without that
/// first cover read, the re-read spends the budget and the cover never moves past it.
#[test]
fn a_costly_mark_reread_first_does_not_stop_the_cover() {
    let mut f = Fake {
        pids: (10..20).collect(),
        ..Fake::default()
    };
    f.stuck.insert(10);
    f.cost_ns.insert(10, 6_000_000);
    let mut census = Census::new(f);
    let covers: usize = (0..40).map(|_| usize::from(census.pass().cover_done)).sum();
    assert!(covers >= 2, "{covers} covers in 40 passes");
}

#[test]
fn a_mark_is_reread_first_and_held_only_for_the_same_process() {
    let mut f = Fake {
        pids: (10..20).collect(),
        ..Fake::default()
    };
    f.stuck.extend([12, 15]);
    let mut census = Census::new(f);
    let first = census.pass();
    assert!(first.cover_done);
    let pids = |p: &Pass| {
        p.marked
            .iter()
            .map(|m| (m.pid, m.again))
            .collect::<Vec<_>>()
    };
    assert_eq!(
        pids(&first),
        [(12, false), (15, false)],
        "one pass is no hold"
    );

    // 12 stays stuck; 15 is gone and its pid taken by a new process, stuck too; 17 sticks now.
    let reads_before = census.source().reads.len();
    census.source.start.insert(15, 99);
    census.source.stuck.insert(17);
    let second = census.pass();
    assert_eq!(
        &census.source().reads[reads_before..reads_before + 2],
        [12, 15],
        "the last pass's marks are read first"
    );
    assert_eq!(
        pids(&second),
        [(12, true), (15, false), (17, false)],
        "held: the same pid and start, two passes running"
    );
    let rereads = census.source().reads[reads_before..]
        .iter()
        .filter(|&&p| p == 12)
        .count();
    assert_eq!(rereads, 1, "a re-read pid is not read again in the cover");

    // 12 clears: no mark, no hold.
    census.source.stuck.remove(&12);
    let third = census.pass();
    assert_eq!(pids(&third), [(15, true), (17, true)]);

    // The hold comes from the re-read, not the cover: with a budget for one process a pass, a
    // cover of 30 takes 30 passes, yet 11 is held at the pass after it was marked (a census
    // that waited for its next cover would hold it 30 passes later).
    let mut f = Fake {
        pids: (10..40).collect(),
        ..Fake::default()
    };
    f.stuck.insert(11);
    let mut tight = Census::with_budget(f, 1);
    let _ = tight.pass();
    let _ = tight.pass();
    let held = tight.pass();
    assert!(
        held.marked.iter().any(|m| m.pid == 11 && m.again),
        "held within passes, not covers: {held:?}"
    );
}
