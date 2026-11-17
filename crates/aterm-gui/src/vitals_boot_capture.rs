// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Boot checkpoint collection on the existing vitals worker. Native persistence remains
//! asynchronous; a retry keeps the same observed cut and adds no periodic wake of its own.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use aterm_vitals::boot_cost::{
    self, Book, Collector, Coverage, Observation, ProcessKey, RawCounters, WakeCount, WakeCounts,
};
use aterm_vitals::explain::HistoricalReported;
use aterm_vitals::model::Sample;
use aterm_vitals::sys::boot::BootReport;

use crate::vitals_host::boot_cost::{BootStore, Persistence};

static EPOCH: AtomicU64 = AtomicU64::new(1);
const RETRY_NS: u64 = 60_000_000_000;
type Wakes = Option<(Vec<(&'static str, u64)>, bool)>;

#[derive(Clone, Copy)]
struct Probes {
    clock: fn() -> Option<u64>,
    wakes: fn() -> Wakes,
}

impl Probes {
    const INERT: Self = Self {
        clock: || None,
        wakes: || None,
    };
    const NATIVE: Self = Self {
        clock: aterm_uds::process::continuous_ns,
        wakes: || Some(crate::metrics::lifetime_wakes()),
    };
}

pub(crate) struct BootCapture {
    boot: Option<String>,
    epoch: u64,
    probes: Probes,
    collector: Collector,
    store: BootStore,
    retry_at: Option<u64>,
    refused: Option<String>,
    coverage: Coverage,
    clock_missing: bool,
    report: Option<Arc<BootReport>>,
    report_dirty: bool,
}

impl BootCapture {
    pub(crate) fn native() -> Self {
        Self::new(
            aterm_types::dirs::state_dir().map(|p| p.join(crate::vitals_host::boot_cost::FILE)),
            aterm_uds::process::boot_id(),
            Probes::NATIVE,
        )
    }

    pub(crate) fn inert() -> Self {
        Self::new(None, None, Probes::INERT)
    }

    fn new(path: Option<PathBuf>, boot: Option<String>, probes: Probes) -> Self {
        let epoch = EPOCH
            .try_update(Ordering::Relaxed, Ordering::Relaxed, |n| n.checked_add(1))
            .unwrap_or(u64::MAX);
        Self {
            boot,
            epoch,
            probes,
            collector: Collector::new(Book::default()),
            store: BootStore::new(path),
            retry_at: None,
            refused: None,
            coverage: Coverage::default(),
            clock_missing: false,
            report: None,
            report_dirty: true,
        }
    }

    pub(crate) fn set_wake(&mut self, wake: Arc<dyn Fn() + Send + Sync>) {
        self.store.set_wake(wake);
    }

    fn poll(&mut self) {
        if self.store.poll() {
            self.report_dirty = true;
            if let Err(error) = self.collector.restore(&self.store.view().book) {
                self.refused = Some(error.to_string());
            }
        }
    }

    fn capture_pending(&self) -> bool {
        self.store.view().persistence != Persistence::Loading
            && self
                .boot
                .as_deref()
                .is_some_and(|boot| self.collector.book().next_target_ns(boot).is_some())
    }

    fn read_clock(&mut self) -> Option<u64> {
        let now = (self.probes.clock)();
        self.report_dirty |= self.clock_missing != now.is_none();
        self.clock_missing = now.is_none();
        if now.is_none() {
            self.coverage = self
                .coverage
                .with(Coverage::PARTIAL)
                .with(Coverage::COLLECTION_GAP);
        }
        now
    }

    /// No timer until restore finishes, without a native clock/boot identity, or after both cuts.
    pub(crate) fn after(&mut self) -> Option<Duration> {
        self.poll();
        if self.refused.is_some() || self.store.view().persistence == Persistence::Loading {
            return None;
        }
        let boot = self.boot.as_deref()?;
        let target = self.collector.book().next_target_ns(boot)?;
        let now = self.read_clock()?;
        Some(Duration::from_nanos(target.saturating_sub(now)))
    }

    /// The native bracket starts before the whole reader, not at its later publication.
    pub(crate) fn begin(&mut self) -> Option<u64> {
        self.poll();
        if !self.capture_pending() {
            return None;
        }
        self.read_clock()
    }

    pub(crate) fn finish(
        &mut self,
        sample: &Sample,
        history: HistoricalReported,
        before: Option<u64>,
    ) -> Arc<BootReport> {
        self.poll();
        if self.capture_pending() {
            let process = sample
                .procs
                .as_deref()
                .and_then(|rows| rows.iter().find(|p| p.pid == sample.self_pid));
            let coalition_id = process.and_then(|p| p.coalition);
            let coalition = sample
                .coalitions
                .as_ref()
                .and_then(|rows| rows.rows.iter().find(|r| Some(r.id) == coalition_id));
            let mut coverage = self.coverage;
            if before.is_none() {
                coverage = coverage
                    .with(Coverage::PARTIAL)
                    .with(Coverage::COLLECTION_GAP);
            }
            if sample.procs_truncated
                || sample.procs.is_none()
                || sample.ownership_incomplete
                || sample.native_incomplete
                || sample.native_source_lost
                || !sample.unproved_tabs.is_empty()
                || sample
                    .coalitions
                    .as_ref()
                    .is_some_and(|c| c.truncated || c.rows.len() < c.listed as usize)
                || self.epoch == u64::MAX
            {
                coverage = coverage.with(Coverage::PARTIAL);
            }
            let wakes = (self.probes.wakes)().and_then(|(variants, saturated)| {
                if saturated {
                    coverage = coverage.with(Coverage::PARTIAL);
                }
                if variants.len() > boot_cost::MAX_VARIANTS
                    || variants.iter().enumerate().any(|(index, (kind, _))| {
                        kind.is_empty()
                            || kind.len() > boot_cost::MAX_WAKE_KIND_BYTES
                            || !kind.bytes().all(|b| b.is_ascii_graphic())
                            || variants[..index].iter().any(|(prior, _)| prior == kind)
                    })
                {
                    coverage = coverage.with(Coverage::PARTIAL);
                    return None;
                }
                Some(WakeCounts {
                    epoch: self.epoch,
                    variants: variants
                        .into_iter()
                        .map(|(kind, count)| WakeCount {
                            kind: kind.to_owned(),
                            count,
                        })
                        .collect(),
                })
            });
            // Wake reads belong inside the bracket too. No counter is execution confined to it.
            let Some(cut) = self.read_clock() else {
                return self.report();
            };
            coverage = coverage.with(self.coverage);
            let observation = Observation {
                at_ns: cut,
                before_ns: before,
                after_ns: before.map(|_| cut),
                process: process.and_then(|p| {
                    p.start.map(|start| ProcessKey {
                        pid: p.pid,
                        start,
                        version: p.version,
                    })
                }),
                counter_epoch: self.epoch,
                process_cpu: RawCounters {
                    own_ns: process.and_then(|p| p.cpu_ns),
                    inherited_ns: process.and_then(|p| p.child_cpu_ns),
                },
                coalition_id,
                coalition_cpu: RawCounters {
                    own_ns: coalition.map(|c| c.cpu_ns),
                    inherited_ns: None,
                },
                historical_reported: history,
                wakes,
                coverage,
            };
            match self.collector.observe(self.boot.as_deref(), observation) {
                Ok(changed) => {
                    self.report_dirty |= self.refused.take().is_some();
                    if changed {
                        self.report_dirty = true;
                        self.store.submit(self.collector.book().clone());
                        self.retry_at = Some(cut.saturating_add(RETRY_NS));
                    }
                }
                Err(error) => {
                    self.refused = Some(error.to_string());
                    self.report_dirty = true;
                }
            }
        }
        self.report()
    }

    fn retry(&mut self) {
        if self.store.view().dirty
            && !self.store.view().busy
            && let Some(now) = (self.probes.clock)()
            && self.retry_at.is_none_or(|when| now >= when)
        {
            self.store.retry();
            self.retry_at = Some(now.saturating_add(RETRY_NS));
            self.report_dirty = true;
        }
    }

    pub(crate) fn report(&mut self) -> Arc<BootReport> {
        self.poll();
        // Both full and machine-only samples are persistence opportunities. This adds no
        // deadline and never recaptures the immutable pending observation.
        self.retry();
        if !self.report_dirty
            && let Some(report) = &self.report
        {
            return Arc::clone(report);
        }
        let view = self.store.view();
        let failure = self
            .refused
            .clone()
            .or_else(|| {
                self.clock_missing
                    .then(|| "continuous_clock_unavailable".to_owned())
            })
            .or_else(|| match view.persistence {
                Persistence::Failed(error) | Persistence::PublishedDurabilityUnknown(error) => {
                    Some(format!("{}:{:?}", error.operation, error.kind))
                }
                _ => None,
            });
        let report = Arc::new(BootReport {
            book: self.collector.book().clone(),
            boot_id: self.boot.clone(),
            persistence: view.persistence.word(),
            busy: view.busy,
            dirty: view.dirty,
            failure,
        });
        self.report_dirty = false;
        self.report = Some(Arc::clone(&report));
        report
    }
}

#[cfg(test)]
#[path = "vitals_boot_capture_tests.rs"]
mod tests;
