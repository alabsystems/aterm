// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Dated resource-wait observations, without resource or scheduling authority.

use std::path::{Path, PathBuf};
use std::sync::{Mutex, PoisonError};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crate::test_plan::{DispatchResources, Plan, Planned};
use crate::testrun::Binary;

#[derive(Clone, Debug)]
pub(super) struct Reason {
    source: &'static str,
    kinds: Vec<&'static str>,
    detail: String,
}

impl Reason {
    fn new(source: &'static str, mut kinds: Vec<&'static str>, detail: String) -> Self {
        kinds.sort_unstable();
        kinds.dedup();
        Self {
            source,
            kinds,
            detail,
        }
    }

    pub(super) fn fifo() -> Self {
        Self::new("jobserver", vec!["core"], "the private FIFO could not supply the complete core charge; partial tokens were returned".into())
    }

    pub(super) fn partial_core_grant() -> Self {
        Self::new(
            "ledger",
            vec!["core"],
            "the ledger's partial grant was smaller than the binary's complete core charge".into(),
        )
    }

    pub(super) fn unreachable(why: String) -> Self {
        Self::new("ledger", vec!["unreachable"], why)
    }

    pub(super) fn ledger(short: &aterm_capacity::admit::Shortfall) -> Self {
        use aterm_capacity::admit::Short;
        let kinds = short
            .shorts
            .iter()
            .map(|short| match short {
                Short::Memory { .. }
                | Short::Pressure
                | Short::Alone { .. }
                | Short::WantsAlone { .. } => "memory",
                Short::Cores => "core",
                Short::Turn { .. } | Short::Ledger(_) => "ledger",
                Short::Disk { .. } => "disk",
                Short::Release => "preemption",
                _ => "admission",
            })
            .collect();
        Self::new("ledger", kinds, short.to_string())
    }

    pub(super) fn local(b: &Planned, resources: DispatchResources) -> Self {
        let mut kinds = Vec::new();
        if resources.held.saturating_add(b.charge) > resources.share
            || (resources.held > 0
                && resources.live + f64::from(b.charge) > f64::from(resources.share) + 1.0)
        {
            kinds.push("core");
        }
        if resources
            .memory
            .is_some_and(|budget| resources.held_bytes.saturating_add(b.peak) > budget)
        {
            kinds.push("memory");
        }
        Self::new(
            "dispatcher",
            kinds,
            format!(
                "charge={} held={} share={} peak_bytes={} held_bytes={} memory_limit={:?} live_cores={}",
                b.charge,
                resources.held,
                resources.share,
                b.peak,
                resources.held_bytes,
                resources.memory,
                resources.live
            ),
        )
    }

    fn ordered() -> Self {
        Self::new(
            "dispatcher",
            vec!["dispatch-order"],
            "queued before this binary's next resource decision".into(),
        )
    }
}

struct Waiting {
    since: Duration,
    first_detail: String,
    reason: Reason,
}

struct Closed {
    index: usize,
    from: Duration,
    to: Duration,
    first_detail: String,
    reason: Reason,
}

struct Book(Vec<Option<Waiting>>);

impl Book {
    fn new(plan: &Plan, count: usize) -> Self {
        let mut rows: Vec<_> = (0..count).map(|_| None).collect();
        for b in &plan.order {
            let Some(slot) = rows.get_mut(b.index) else {
                continue;
            };
            let reason = Reason::ordered();
            *slot = Some(Waiting {
                since: Duration::ZERO,
                first_detail: reason.detail.clone(),
                reason,
            });
        }
        Self(rows)
    }

    fn change(&mut self, index: usize, next: Option<Reason>, at: Duration) -> Option<Closed> {
        let slot = self.0.get_mut(index)?;
        if let (Some(current), Some(next)) = (slot.as_mut(), next.as_ref())
            && current.reason.source == next.source
            && current.reason.kinds == next.kinds
        {
            current.reason.detail.clone_from(&next.detail);
            return None;
        }
        let previous = slot.take();
        *slot = next.map(|reason| Waiting {
            since: at,
            first_detail: reason.detail.clone(),
            reason,
        });
        previous.map(|previous| Closed {
            index,
            from: previous.since,
            to: at,
            first_detail: previous.first_detail,
            reason: previous.reason,
        })
    }
}

pub(super) struct Trace<'a> {
    ctx: Option<&'a crate::Ctx>,
    bins: &'a [Binary],
    began: Instant,
    epoch_ms: Option<u64>,
    book: Mutex<Book>,
}

impl<'a> Trace<'a> {
    pub(super) fn new(
        ctx: Option<&'a crate::Ctx>,
        bins: &'a [Binary],
        plan: &Plan,
        began: Instant,
    ) -> Self {
        Self {
            ctx,
            bins,
            began,
            epoch_ms: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .ok()
                .and_then(|d| u64::try_from(d.as_millis()).ok()),
            book: Mutex::new(Book::new(plan, bins.len())),
        }
    }

    pub(super) fn probe(ctx: &'a crate::Ctx, bins: &'a [Binary], index: usize) -> Self {
        let began = Instant::now();
        let reason = Reason::ordered();
        let mut rows: Vec<_> = (0..bins.len()).map(|_| None).collect();
        rows[index] = Some(Waiting {
            since: Duration::ZERO,
            first_detail: reason.detail.clone(),
            reason,
        });
        Self {
            ctx: Some(ctx),
            bins,
            began,
            epoch_ms: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .ok()
                .and_then(|d| u64::try_from(d.as_millis()).ok()),
            book: Mutex::new(Book(rows)),
        }
    }

    pub(super) fn change(&self, index: usize, next: Option<Reason>) {
        let Some(ctx) = self.ctx else { return };
        let row = self
            .book
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .change(index, next, self.began.elapsed());
        if let Some(row) = row {
            let stamp = |offset: Duration| {
                self.epoch_ms.map(|base| {
                    base.saturating_add(u64::try_from(offset.as_millis()).unwrap_or(u64::MAX))
                })
            };
            let detail =
                |text: &str| crate::report::clip_json(text, crate::report::EVENT_DETAIL_BYTES);
            let event = crate::event_log::event_value(
                crate::event_log::EventKind::TestDispatchWait,
                aterm_json::json!({
                    "test":self.bins[row.index].spec,
                    "process_id":std::process::id(),
                    "source":row.reason.source,"resources":row.reason.kinds,
                    "started_at_unix_ms":stamp(row.from),"ended_at_unix_ms":stamp(row.to),
                    "waiting_s":row.to.saturating_sub(row.from).as_secs_f64(),
                    "first_detail":detail(&row.first_detail),"last_detail":detail(&row.reason.detail),
                    "bounds":"resource decisions; availability may change between looks",
                }),
            );
            crate::event_log::emit(ctx.events.as_ref(), &format!("verify-event: {event}"));
        }
    }

    pub(super) fn finish_pending(&self) {
        if self.ctx.is_none() {
            return;
        }
        let pending: Vec<_> = self
            .book
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .0
            .iter()
            .enumerate()
            .filter_map(|(index, row)| row.as_ref().map(|_| index))
            .collect();
        for index in pending {
            self.change(index, None);
        }
    }
}

impl Drop for Trace<'_> {
    fn drop(&mut self) {
        self.finish_pending();
    }
}

/// One compiler's complete admission interval, including unidentified compiler calls.
pub(super) struct UnitTrace {
    task: String,
    ledger: PathBuf,
    began: Instant,
    epoch_ms: Option<u64>,
    book: Book,
}

impl UnitTrace {
    pub(super) fn new(task: &str, ledger: &Path) -> Self {
        let reason = Reason::new(
            "admission",
            vec!["admission-io"],
            "before the first resource decision".into(),
        );
        Self {
            task: task.to_owned(),
            ledger: ledger.to_owned(),
            began: Instant::now(),
            epoch_ms: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .ok()
                .and_then(|d| u64::try_from(d.as_millis()).ok()),
            book: Book(vec![Some(Waiting {
                since: Duration::ZERO,
                first_detail: reason.detail.clone(),
                reason,
            })]),
        }
    }

    pub(super) fn observe(&mut self, short: &aterm_capacity::admit::Shortfall) {
        self.change(Some(Reason::ledger(short)));
    }

    fn change(&mut self, next: Option<Reason>) {
        if let Some(row) = self.book.change(0, next, self.began.elapsed()) {
            let stamp = |offset: Duration| {
                self.epoch_ms.map(|base| {
                    base.saturating_add(u64::try_from(offset.as_millis()).unwrap_or(u64::MAX))
                })
            };
            let detail =
                |text: &str| crate::report::clip_json(text, crate::report::EVENT_DETAIL_BYTES);
            let event = crate::event_log::event_value(
                crate::event_log::EventKind::CompileAdmissionWait,
                aterm_json::json!({
                    "task":self.task,"ledger":self.ledger,"process_id":std::process::id(),
                    "source":row.reason.source,"resources":row.reason.kinds,
                    "started_at_unix_ms":stamp(row.from),"ended_at_unix_ms":stamp(row.to),
                    "waiting_s":row.to.saturating_sub(row.from).as_secs_f64(),
                    "first_detail":detail(&row.first_detail),"last_detail":detail(&row.reason.detail),
                    "bounds":"resource decisions; availability may change between looks",
                }),
            );
            crate::event_log::emit(None, &format!("verify-event: {event}"));
        }
    }
}

impl Drop for UnitTrace {
    fn drop(&mut self) {
        self.change(None);
    }
}

#[cfg(test)]
#[path = "resource_waits_tests.rs"]
mod tests;
