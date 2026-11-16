// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Changes in this process's published CPU counters, grouped by stable observed thread names.
//! A kernel can publish CPU after it ran, so growth between observations is not CPU confined
//! to their wall-clock brackets. Different thread and process observations are not a partition.
//! Names are endpoint observations, not proof that a name held while the counted CPU ran.

use std::collections::{BTreeMap, BTreeSet};

/// The process generation whose counters a cut read.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct OwnCpuProcess {
    /// Its PID.
    pub pid: u32,
    /// Its native start counter, in the same units as the host's process-start identities.
    pub start: u64,
    /// Its kernel PID version, checked on both sides of the read.
    pub version: i32,
}

/// A published cumulative CPU counter with a one-sided representation-quantization bound.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct OwnCpuCounter {
    /// The reported cumulative nanoseconds.
    pub ns: u64,
    /// The published value before representation quantization lies in `ns..=ns + precision_ns`.
    /// This bounds no delay between CPU execution and its publication in the counter.
    pub precision_ns: u64,
}

/// One thread observed through a stable native thread identity.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct OwnCpuThread {
    /// The native thread ID, unique within this process generation.
    pub id: u64,
    /// The observed full name; absent when unreadable, invalid or possibly truncated.
    pub name: Option<String>,
    /// Its cumulative own CPU, or absent when that thread's counter was unreadable.
    pub cpu: Option<OwnCpuCounter>,
}

/// Coverage of one native thread-list read.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct OwnCpuCoverage {
    /// How many threads the native list returned.
    pub listed: usize,
    /// How many listed entries were inspected.
    pub inspected: usize,
    /// How many inspected entries had an unread identity or counter.
    pub unread: usize,
    /// Whether a processing cap left some listed entries uninspected.
    pub capped: bool,
}

/// A process-counter bracket around its thread observations, on one monotonic clock.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct OwnCpuCut {
    /// The unchanged process generation checked around the cut.
    pub process: OwnCpuProcess,
    /// When the read began, nanoseconds on the host reader's clock.
    pub begin_ns: u64,
    /// When the read ended, on the same clock.
    pub end_ns: u64,
    /// The process's CPU counter read before the thread observations.
    pub process_before: OwnCpuCounter,
    /// The process's CPU counter read after the thread observations.
    pub process_after: OwnCpuCounter,
    /// The readable thread identities, with partial observations retained.
    pub threads: Vec<OwnCpuThread>,
    /// What the native list and its individual reads covered.
    pub coverage: OwnCpuCoverage,
}

/// Bounds on a published cumulative counter's growth, in nanoseconds of counter units.
///
/// These are representation bounds, not CPU execution attributed to a wall-time interval.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct OwnCpuGrowth {
    /// The least increment allowed by the observations' quantization bounds.
    pub lower_ns: u64,
    /// The greatest increment allowed by those bounds.
    pub upper_ns: u64,
}

/// Counter growth for threads whose full observed names agree at both cuts.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OwnCpuGroup {
    /// The exact observed name, without interpreting it as an immutable causal lane.
    pub name: String,
    /// Sum of their independent published-counter growth bounds, without wall-time alignment.
    pub growth: OwnCpuGrowth,
    /// How many stable thread identities contributed to this name.
    pub threads: usize,
}

/// Coverage of the paired counter observations.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct OwnCpuWindowCoverage {
    /// The earlier read's native coverage.
    pub before: OwnCpuCoverage,
    /// The later read's native coverage.
    pub after: OwnCpuCoverage,
    /// Thread identities observed at only one cut, including exited and newly born threads.
    pub unpaired: usize,
    /// Matched thread pairs lacking at least one CPU counter.
    pub unread: usize,
    /// Matched readable pairs with no stable, nonempty name.
    pub unnamed: usize,
    /// Of the unnamed pairs, those whose two readable names differed.
    pub name_changes: usize,
}

/// Independent counter changes observed in two disjoint native read brackets.
///
/// Neither bracket bounds deferred publication. Group growth may exceed process growth;
/// their asynchronous counter changes have no remainder or common CPU-share denominator.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OwnCpuChanges {
    /// The process generation observed at both cuts.
    pub process: OwnCpuProcess,
    /// When the earlier native read began, on the host reader's monotonic clock.
    pub before_begin_ns: u64,
    /// When the earlier native read ended.
    pub before_end_ns: u64,
    /// When the later native read began.
    pub after_begin_ns: u64,
    /// When the later native read ended.
    pub after_end_ns: u64,
    /// Growth allowed by the process-counter ranges observed in the two read brackets.
    pub process_growth: OwnCpuGrowth,
    /// Stable observed names, largest growth lower bound first; ties ordered by name.
    pub groups: Vec<OwnCpuGroup>,
    /// Missing identities, counters and names remain separate from the numerical bounds.
    pub coverage: OwnCpuWindowCoverage,
}

/// This reading's self-counter observation state; unavailable is never zero growth.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum OwnCpuView {
    /// One valid cut exists; a later disjoint cut is needed.
    Warming,
    /// No counter changes can be observed from this reading, with a static diagnostic reason.
    Unavailable(&'static str),
    /// Counter changes with the original read brackets and coverage, without wall-time shares.
    Observed(OwnCpuChanges),
}

impl Default for OwnCpuView {
    fn default() -> Self {
        Self::Unavailable("not_sampled")
    }
}

/// The previous native cut, never a carried observation result.
#[derive(Clone, Debug, Default)]
pub struct OwnCpuState {
    previous: Option<OwnCpuCut>,
}

impl OwnCpuState {
    /// Absorb one native read. A skipped or failed read clears continuity; the next valid
    /// reading warms again. A changed generation or reset is unavailable at its edge and
    /// establishes only a new baseline for the following reading.
    pub fn absorb(&mut self, reading: Option<Result<&OwnCpuCut, &'static str>>) -> OwnCpuView {
        let cut = match reading {
            Some(Ok(cut)) => cut,
            Some(Err(reason)) => {
                self.previous = None;
                return OwnCpuView::Unavailable(reason);
            }
            None => {
                self.previous = None;
                return OwnCpuView::Unavailable("not_sampled");
            }
        };
        if let Err(reason) = validate(cut) {
            self.previous = None;
            return OwnCpuView::Unavailable(reason);
        }
        let Some(previous) = self.previous.replace(cut.clone()) else {
            return OwnCpuView::Warming;
        };
        match between(&previous, cut) {
            Ok(changes) => OwnCpuView::Observed(changes),
            Err(reason) => OwnCpuView::Unavailable(reason),
        }
    }
}

fn validate(cut: &OwnCpuCut) -> Result<(), &'static str> {
    if cut.process.pid == 0 || cut.process.start == 0 {
        return Err("invalid_process_identity");
    }
    if cut.begin_ns >= cut.end_ns {
        return Err("invalid_read_interval");
    }
    if cut.process_after.ns < cut.process_before.ns {
        return Err("process_counter_reset");
    }
    if cut.coverage.inspected > cut.coverage.listed
        || cut.threads.len() > cut.coverage.inspected
        || cut.coverage.unread > cut.coverage.inspected
    {
        return Err("invalid_thread_coverage");
    }
    let mut ids = BTreeSet::new();
    for thread in &cut.threads {
        if thread.id == 0 || !ids.insert(thread.id) {
            return Err("invalid_thread_identity");
        }
    }
    Ok(())
}

fn narrow(ns: u128) -> Result<u64, &'static str> {
    u64::try_from(ns).map_err(|_| "counter_overflow")
}

fn upper(counter: OwnCpuCounter) -> u128 {
    u128::from(counter.ns) + u128::from(counter.precision_ns)
}

fn growth(
    before_lower: u128,
    before_upper: u128,
    after_lower: u128,
    after_upper: u128,
) -> Result<OwnCpuGrowth, &'static str> {
    Ok(OwnCpuGrowth {
        lower_ns: narrow(after_lower.saturating_sub(before_upper))?,
        upper_ns: narrow(
            after_upper
                .checked_sub(before_lower)
                .ok_or("counter_reset")?,
        )?,
    })
}

fn between(before: &OwnCpuCut, after: &OwnCpuCut) -> Result<OwnCpuChanges, &'static str> {
    if before.process != after.process {
        return Err("process_changed");
    }
    if before.end_ns >= after.begin_ns {
        return Err("overlapping_read_intervals");
    }
    if after.process_before.ns < before.process_after.ns {
        return Err("process_counter_reset");
    }
    let process_growth = growth(
        u128::from(before.process_before.ns),
        upper(before.process_after),
        u128::from(after.process_before.ns),
        upper(after.process_after),
    )?;
    let previous: BTreeMap<_, _> = before.threads.iter().map(|row| (row.id, row)).collect();
    let current: BTreeSet<_> = after.threads.iter().map(|row| row.id).collect();
    let mut coverage = OwnCpuWindowCoverage {
        before: before.coverage,
        after: after.coverage,
        unpaired: previous.keys().filter(|id| !current.contains(id)).count(),
        ..OwnCpuWindowCoverage::default()
    };
    let mut groups: BTreeMap<&str, (u128, u128, usize)> = BTreeMap::new();
    for thread in &after.threads {
        let Some(prior) = previous.get(&thread.id) else {
            coverage.unpaired += 1;
            continue;
        };
        let (Some(old), Some(new)) = (prior.cpu, thread.cpu) else {
            coverage.unread += 1;
            continue;
        };
        if new.ns < old.ns {
            return Err("thread_counter_reset");
        }
        let changed = growth(
            u128::from(old.ns),
            upper(old),
            u128::from(new.ns),
            upper(new),
        )?;
        match (prior.name.as_deref(), thread.name.as_deref()) {
            (Some(old), Some(new)) if old == new && !new.is_empty() => {
                let group = groups.entry(new).or_default();
                group.0 = group
                    .0
                    .checked_add(u128::from(changed.lower_ns))
                    .ok_or("counter_overflow")?;
                group.1 = group
                    .1
                    .checked_add(u128::from(changed.upper_ns))
                    .ok_or("counter_overflow")?;
                group.2 += 1;
            }
            (old, new) => {
                coverage.unnamed += 1;
                coverage.name_changes +=
                    usize::from(matches!((old, new), (Some(a), Some(b)) if a != b));
            }
        }
    }
    let mut groups = groups
        .into_iter()
        .map(|(name, (lower_ns, upper_ns, threads))| {
            Ok(OwnCpuGroup {
                name: name.to_owned(),
                growth: OwnCpuGrowth {
                    lower_ns: narrow(lower_ns)?,
                    upper_ns: narrow(upper_ns)?,
                },
                threads,
            })
        })
        .collect::<Result<Vec<_>, &'static str>>()?;
    groups.sort_by(|a, b| {
        b.growth
            .lower_ns
            .cmp(&a.growth.lower_ns)
            .then(a.name.cmp(&b.name))
    });
    Ok(OwnCpuChanges {
        process: after.process,
        before_begin_ns: before.begin_ns,
        before_end_ns: before.end_ns,
        after_begin_ns: after.begin_ns,
        after_end_ns: after.end_ns,
        process_growth,
        groups,
        coverage,
    })
}

#[cfg(test)]
#[path = "self_cpu_tests.rs"]
mod tests;
