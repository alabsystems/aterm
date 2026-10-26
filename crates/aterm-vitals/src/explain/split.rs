// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Busy CPU split over one matched window, by coalition.

use super::{
    CoalRow, CoalSample, CoalSplit, CoalUsage, IdMap, Named, Part, ReapLedger, ReapTotals, Via,
};
use crate::model::{Membership, ProcSample, Sample, Split, TabRoot, TabView};

/// The longest window a split reads, ns.
pub const WINDOW_MAX_NS: u64 = 60_000_000_000;

/// One split tick's counters: where the next window starts.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CoalBase {
    /// The reading's stamp, ns on the host's monotonic clock.
    pub at_ns: u64,
    /// Busy ticks summed over every core.
    pub busy: u64,
    /// All ticks summed over every core.
    pub total: u64,
    /// The cores read.
    pub cores: usize,
    /// Each listed coalition's counters, by id.
    pub coalitions: IdMap<u64, CoalUsage>,
    /// The highest id the kernel listed, answered or not, where the list was whole.
    pub newest: Option<u64>,
    /// Each process of the user's, by pid: its kernel start and CPU time.
    pub procs: IdMap<u32, (Option<u64>, u64)>,
    /// Whether the list or the sweep stopped short.
    pub truncated: bool,
    /// Whether the list stopped short.
    pub list_truncated: bool,
    /// The reap credit's counters at this reading, where its ledger read the sweep.
    pub reap: Option<ReapTotals>,
}

impl CoalBase {
    /// Samples `s`'s counters, coalitions `read`, and CPU time of `uid`'s processes in `procs`.
    #[must_use]
    pub fn of(s: &Sample, read: &CoalSample, procs: &[ProcSample], uid: u32) -> Option<Self> {
        let cores = s.cores.as_deref().filter(|c| !c.is_empty())?;
        Some(Self {
            at_ns: s.at_ns,
            busy: cores.iter().map(|t| t.busy).fold(0, u64::saturating_add),
            total: cores.iter().map(|t| t.total).fold(0, u64::saturating_add),
            cores: cores.len(),
            coalitions: read.rows.iter().map(|u| (u.id, *u)).collect(),
            newest: read.newest.filter(|_| !read.truncated),
            procs: procs
                .iter()
                .filter(|p| p.uid == Some(uid))
                .filter_map(|p| Some((p.pid, (p.start, p.cpu_ns?))))
                .collect(),
            truncated: read.truncated || s.procs_truncated,
            list_truncated: read.truncated,
            reap: None,
        })
    }

    /// The window from `base` to this reading, ns.
    #[must_use]
    pub fn window_after(&self, base: &Self) -> Option<u64> {
        let window = self.at_ns.checked_sub(base.at_ns).filter(|w| *w > 0)?;
        (window <= WINDOW_MAX_NS && self.cores == base.cores && self.total > base.total)
            .then_some(window)
    }
}

/// What one split reads.
#[derive(Clone, Copy, Debug)]
pub struct SplitIn<'a> {
    /// The window's start.
    pub base: &'a CoalBase,
    /// Its end: this split tick.
    pub now: &'a CoalBase,
    /// This tick's sweep.
    pub procs: &'a [ProcSample],
    /// Each process's scopes, in [`Self::procs`]' order.
    pub members: &'a [Membership],
    /// Whether each process is a delegated job's member, in the same order.
    pub delegated: &'a [bool],
    /// This aterm's user.
    pub self_uid: u32,
    /// Every tab, in the host's order.
    pub tabs: &'a [TabRoot],
    /// Each listed coalition's part and lead ([`super::name`]).
    pub names: &'a IdMap<u64, Named>,
    /// Each coalition's live members this tick.
    pub live: &'a IdMap<u64, u32>,
    /// Coalitions the kernel listed.
    pub listed: u32,
    /// The reap credit's ledger, as it read this tick's sweep: where it read the window's both
    /// ends ([`CoalBase::reap`]), each tab's figure and aterm's own are its counters' growth over
    /// the window, live and reaped; else the live figures alone.
    pub reap: Option<&'a ReapLedger>,
}

/// The split of `i`'s window, by coalition.
#[must_use]
pub fn split(i: &SplitIn) -> CoalSplit {
    let window = i.now.at_ns.saturating_sub(i.base.at_ns).max(1);
    let mc = |ns: u64| per_window(ns, window);
    let global_mc = rounded(
        u128::from(i.now.busy.saturating_sub(i.base.busy)) * i.now.cores as u128 * 1000,
        u128::from(i.now.total.saturating_sub(i.base.total)),
    );
    let part_of = |c: u64| i.names.get(&c).map_or(Part::Services, |n| n.part);
    let add = |sum: &mut u64, ns: u64| *sum = sum.saturating_add(ns);

    let mut tabs = vec![0u64; i.tabs.len()];
    let mut own_ns = 0u64;
    let mut short_tabs = Vec::new();
    // The reap credit, where its ledger read both ends: it counts every
    // member but a delegated job's, whose live work the loop below reads.
    let reap = match (i.reap, &i.base.reap, &i.now.reap) {
        (Some(ledger), Some(base), Some(now)) => Some((ledger, base, now)),
        _ => None,
    };
    if let Some((_, base, now)) = reap {
        let mut in_tabs = 0u64;
        for (k, (t, sum)) in i.tabs.iter().zip(tabs.iter_mut()).enumerate() {
            // A split tab's panes share its key: its counter once, on its first.
            if i.tabs[..k].iter().all(|o| o.tab != t.tab) {
                let (ns, missed) = now.since(base, t.tab);
                *sum = ns;
                in_tabs = in_tabs.saturating_add(ns);
                if missed {
                    short_tabs.push(t.tab);
                }
            }
        }
        own_ns = now.all.saturating_sub(base.all).saturating_sub(in_tabs);
    }
    // Whether every member of aterm's coalitions swept now is in a figure
    // that read it at both ends of the window ([`CoalSplit::skew_mc`]).
    let mut all_live = true;
    let mut moved: IdMap<u64, u64> = IdMap::default();
    for ((p, m), delegated) in i.procs.iter().zip(i.members).zip(i.delegated) {
        let ours = p.coalition.is_some_and(|c| part_of(c) == Part::Aterm);
        if let Some((ledger, base, _)) = reap.filter(|_| !*delegated) {
            all_live &= !ours || ledger.counted(p.pid, p.start, base.step);
            continue;
        }
        let (Some(ns), Some(&(start, before))) = (p.cpu_ns, i.base.procs.get(&p.pid)) else {
            all_live &= !ours;
            continue;
        };
        if start.is_none() || start != p.start || p.uid != Some(i.self_uid) {
            all_live &= !ours;
            continue;
        }
        // A member a job took over since the window began: from where the ledger's counter
        // left it, which took its work until then.
        let before = reap
            .and_then(|(ledger, _, _)| ledger.credited(p.pid, p.start))
            .map_or(before, |c| c.max(before));
        let work = ns.saturating_sub(before);
        match m.tab.and_then(|t| tabs.get_mut(t)) {
            Some(tab) => add(tab, work),
            None if m.aterm => add(&mut own_ns, work),
            None => all_live &= !ours,
        }
        if let Some(c) = p
            .coalition
            .filter(|c| *delegated && part_of(*c) != Part::Aterm)
        {
            add(moved.entry(c).or_default(), work);
        }
    }

    let mut homes: IdMap<u64, Option<Vec<u64>>> = IdMap::default();
    for (p, m) in i.procs.iter().zip(i.members) {
        let Some(c) = p.coalition else { continue };
        let key = m.tab.and_then(|t| i.tabs.get(t)).map(|t| t.tab);
        let home = homes.entry(c).or_insert_with(|| Some(Vec::new()));
        match (home.as_mut(), key) {
            (Some(keys), Some(k)) if !keys.contains(&k) => keys.push(k),
            (Some(_), Some(_)) => {}
            _ => *home = None,
        }
    }

    let (mut aterm_ns, mut delegated_ns, mut others_ns, mut services_ns) = (0, 0, 0, 0);
    // Whether none of aterm's coalitions made or reaped a member between the
    // window's two reads, each read at both ends with the same task
    // counters, and the sweep found each one's every live member
    // ([`CoalSplit::skew_mc`]).
    let mut kept_members = true;
    let mut rows = Vec::with_capacity(i.now.coalitions.len());
    let mut rows_ns = Vec::with_capacity(i.now.coalitions.len());
    for u in i.now.coalitions.values() {
        let before = match i.base.coalitions.get(&u.id) {
            Some(b) => Some(*b).filter(|b| b.cpu_ns <= u.cpu_ns),
            None if i.base.newest.is_some_and(|n| u.id > n) => Some(CoalUsage::default()),
            None => None,
        };
        let measured = before.is_some();
        let before = before.unwrap_or(*u);
        let deferred = |e: &CoalUsage| e.eqos_ns[1].saturating_add(e.eqos_ns[2]);
        let qos = |e: &CoalUsage| {
            e.eqos_ns
                .iter()
                .fold(0, |sum: u64, ns| sum.saturating_add(*ns))
        };
        let named = i.names.get(&u.id);
        let part = named.map_or(Part::Services, |n| n.part);
        if part == Part::Aterm {
            let swept = u64::from(i.live.get(&u.id).copied().unwrap_or(0));
            kept_members &= measured
                && (before.tasks_started, before.tasks_exited) == (u.tasks_started, u.tasks_exited)
                && u.tasks_started.checked_sub(u.tasks_exited) == Some(swept);
        }
        let cpu_ns = u.cpu_ns - before.cpu_ns;
        let delegated = moved.get(&u.id).map_or(0, |d| (*d).min(cpu_ns));
        match part {
            Part::Aterm => add(&mut aterm_ns, cpu_ns),
            Part::Others => add(&mut others_ns, cpu_ns - delegated),
            Part::Services => add(&mut services_ns, cpu_ns - delegated),
        }
        add(&mut delegated_ns, delegated);
        rows_ns.push(cpu_ns);
        rows.push(CoalRow {
            id: u.id,
            part,
            cpu_mc: mc(cpu_ns),
            measured,
            delegated_mc: mc(delegated),
            deferred_mc: mc(deferred(u).saturating_sub(deferred(&before))),
            qos_mc: mc(qos(u).saturating_sub(qos(&before))),
            read_bps: u64::try_from(
                u128::from(u.bytes_read.saturating_sub(before.bytes_read)) * 1_000_000_000
                    / u128::from(window),
            )
            .unwrap_or(u64::MAX),
            footprint: u.footprint,
            live: i.live.get(&u.id).copied().unwrap_or(0),
            tabs: homes.get(&u.id).cloned().flatten().map(|mut keys| {
                keys.sort_unstable();
                keys
            }),
            finished: u.tasks_exited > 0,
            lead: named.and_then(|n| n.lead.clone()),
        });
    }
    let live_ns = tabs.iter().fold(own_ns, |sum, ns| sum.saturating_add(*ns));
    let past_ns = aterm_ns
        .saturating_add(delegated_ns)
        .saturating_sub(live_ns);
    // With no member made or reaped and every member swept and in a figure
    // that read it at both ends, aterm's counters held only the figures'
    // work: what they ran past the figures is the counters' skew against
    // their members' times, never finished work (RD §3.5).
    let (unattributed_ns, skew_ns) = if kept_members && all_live {
        (0, past_ns)
    } else {
        (past_ns, 0)
    };
    let unattributed_mc = mc(unattributed_ns);
    let (aterm_mc, delegated_mc, others_mc) = (mc(aterm_ns), mc(delegated_ns), mc(others_ns));
    let services_mc = mc(services_ns);
    as_a_set(
        &mut rows,
        &rows_ns,
        window,
        aterm_mc + delegated_mc + others_mc + services_mc,
    );
    // Ids are unique, so the order is total and an unstable sort is the
    // stable one's: it moves the ~650 rows (each a few hundred bytes) far less.
    rows.sort_unstable_by(|a, b| b.cpu_mc.cmp(&a.cpu_mc).then(a.id.cmp(&b.id)));
    let unaccounted_mc = global_mc - (aterm_mc + delegated_mc + others_mc + services_mc);
    CoalSplit {
        via: Via::Coalitions,
        exact: !(i.now.truncated || i.base.truncated) && rows.iter().all(|r| r.measured),
        list_truncated: i.now.list_truncated || i.base.list_truncated,
        listed: i.listed,
        global_mc,
        aterm_mc,
        delegated_mc,
        others_mc,
        services_mc,
        unaccounted_mc,
        tabs: by_tab(i.tabs.iter().map(|t| t.tab).zip(tabs))
            .into_iter()
            .map(|(k, ns)| (k, mc(ns)))
            .collect(),
        own_mc: mc(own_ns),
        unattributed_mc,
        skew_mc: mc(skew_ns),
        short_tabs,
        rows,
    }
}

/// `rows`' CPU (`ns` over `window`, in their order) rounded as a set, by the
/// largest remainder, so the rows add up to `target`, the parts' total: a
/// row's figure is its own rounded down or up, the larger remainders up
/// first (ties to the lower id). Rounded one by one, hundreds of near-idle
/// coalitions each round down, and the rows fall millicores short of the
/// parts. Where `target` is out of the set's reach (no remainder enough, or
/// past the floors), each row keeps its own rounding.
fn as_a_set(rows: &mut [CoalRow], ns: &[u64], window: u64, target: i64) {
    let window = u128::from(window.max(1));
    let floors: Vec<i64> = ns
        .iter()
        .map(|n| i64::try_from(u128::from(*n) * 1000 / window).unwrap_or(i64::MAX))
        .collect();
    let mut up: Vec<(u128, u64, usize)> = ns
        .iter()
        .zip(rows.iter())
        .enumerate()
        .map(|(k, (n, row))| (u128::from(*n) * 1000 % window, row.id, k))
        .filter(|(rem, _, _)| *rem > 0)
        .collect();
    let sum = floors.iter().fold(0, |sum: i64, f| sum.saturating_add(*f));
    let Some(extra) = usize::try_from(target.saturating_sub(sum))
        .ok()
        .filter(|e| *e <= up.len())
    else {
        return;
    };
    let larger = |a: &(u128, u64, usize), b: &(u128, u64, usize)| b.0.cmp(&a.0).then(a.1.cmp(&b.1));
    if extra < up.len() {
        up.select_nth_unstable_by(extra, larger);
    }
    for (row, floor) in rows.iter_mut().zip(&floors) {
        row.cpu_mc = *floor;
    }
    for &(_, _, k) in &up[..extra] {
        rows[k].cpu_mc += 1;
    }
    for row in rows {
        row.delegated_mc = row.delegated_mc.min(row.cpu_mc);
    }
}

fn by_tab<T: std::ops::AddAssign>(panes: impl IntoIterator<Item = (u64, T)>) -> Vec<(u64, T)> {
    let mut tabs: Vec<(u64, T)> = Vec::new();
    for (tab, v) in panes {
        match tabs.iter_mut().find(|(k, _)| *k == tab) {
            Some((_, sum)) => *sum += v,
            None => tabs.push((tab, v)),
        }
    }
    tabs
}

fn per_window(ns: u64, window: u64) -> i64 {
    rounded(u128::from(ns) * 1000, u128::from(window))
}

fn rounded(num: u128, den: u128) -> i64 {
    let den = den.max(1);
    i64::try_from((num + den / 2) / den).unwrap_or(i64::MAX)
}

impl CoalSplit {
    /// The split from the built [`crate::Split`] (by subtraction), with each tab's live figure from
    /// the same sweep.
    #[must_use]
    pub fn by_subtraction(built: &Split, tabs: &[TabView], roots: &[TabRoot]) -> Self {
        let mc = |cores: f32| super::millicores(cores);
        let (global_mc, aterm_mc, others_mc, services_mc) = (
            mc(built.global),
            mc(built.aterm),
            mc(built.others),
            mc(built.services),
        );
        let tab_of = |key: u64| roots.iter().find(|r| r.key == key).map_or(key, |r| r.tab);
        let tabs = by_tab(tabs.iter().map(|t| (tab_of(t.key), mc(t.scope.cores.raw))));
        let in_tabs: i64 = tabs.iter().map(|(_, mc)| mc).sum();
        let own_mc = (aterm_mc - in_tabs).max(0);
        Self {
            via: Via::Subtraction,
            exact: false,
            list_truncated: false,
            listed: 0,
            global_mc,
            aterm_mc,
            delegated_mc: 0,
            others_mc,
            services_mc,
            unaccounted_mc: global_mc - aterm_mc - others_mc - services_mc,
            tabs,
            own_mc,
            unattributed_mc: (aterm_mc - own_mc - in_tabs).max(0),
            skew_mc: 0,
            short_tabs: Vec::new(),
            rows: Vec::new(),
        }
    }
}
