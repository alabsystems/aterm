// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Busy CPU split over one matched window, by coalition.

use std::collections::HashMap;

use super::{CoalRow, CoalSample, CoalSplit, CoalUsage, Named, Part, Via};
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
    pub coalitions: HashMap<u64, CoalUsage>,
    /// The highest id the kernel listed, answered or not, where the list was whole.
    pub newest: Option<u64>,
    /// Each process of the user's, by pid: its kernel start and CPU time.
    pub procs: HashMap<u32, (Option<u64>, u64)>,
    /// Whether the list or the sweep stopped short.
    pub truncated: bool,
    /// Whether the list stopped short.
    pub list_truncated: bool,
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
    pub names: &'a HashMap<u64, Named>,
    /// Each coalition's live members this tick.
    pub live: &'a HashMap<u64, u32>,
    /// Coalitions the kernel listed.
    pub listed: u32,
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
    let mut moved: HashMap<u64, u64> = HashMap::new();
    for ((p, m), delegated) in i.procs.iter().zip(i.members).zip(i.delegated) {
        let (Some(ns), Some(&(start, before))) = (p.cpu_ns, i.base.procs.get(&p.pid)) else {
            continue;
        };
        if start.is_none() || start != p.start || p.uid != Some(i.self_uid) {
            continue;
        }
        let work = ns.saturating_sub(before);
        match m.tab.and_then(|t| tabs.get_mut(t)) {
            Some(tab) => add(tab, work),
            None if m.aterm => add(&mut own_ns, work),
            None => {}
        }
        if let Some(c) = p
            .coalition
            .filter(|c| *delegated && part_of(*c) != Part::Aterm)
        {
            add(moved.entry(c).or_default(), work);
        }
    }

    let mut homes: HashMap<u64, Option<Vec<u64>>> = HashMap::new();
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
    let mut rows = Vec::with_capacity(i.now.coalitions.len());
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
        let cpu_ns = u.cpu_ns - before.cpu_ns;
        let delegated = moved.get(&u.id).map_or(0, |d| (*d).min(cpu_ns));
        match part {
            Part::Aterm => add(&mut aterm_ns, cpu_ns),
            Part::Others => add(&mut others_ns, cpu_ns - delegated),
            Part::Services => add(&mut services_ns, cpu_ns - delegated),
        }
        add(&mut delegated_ns, delegated);
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
    rows.sort_by(|a, b| b.cpu_mc.cmp(&a.cpu_mc).then(a.id.cmp(&b.id)));

    let (aterm_mc, delegated_mc, others_mc) = (mc(aterm_ns), mc(delegated_ns), mc(others_ns));
    let services_mc = mc(services_ns);
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
        rows,
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
            own_mc: (aterm_mc - in_tabs).max(0),
            rows: Vec::new(),
        }
    }
}
