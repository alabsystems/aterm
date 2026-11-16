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
    pub(super) fn classification(&self) -> (&'static str, &[&'static str]) {
        (self.source, &self.kinds)
    }

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

    pub(super) fn zero_core_grant() -> Self {
        Self::new(
            "ledger",
            vec!["core"],
            "the ledger granted no cores to the binary".into(),
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
                Short::Heavy { .. } => "turn",
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

    /// A start the run's live share refused ([`crate::test_share::Live::refuses`]).
    pub(super) fn share(
        b: &Planned,
        held: u32,
        live: f64,
        refused: crate::test_share::Refused,
    ) -> Self {
        Self::new(
            "dispatcher",
            vec!["core"],
            format!(
                "charge={} held={held} live_share={} asked={} live_cores={live}",
                b.charge, refused.share, refused.asked
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

    /// The ledger's lock met at every look for `span` since the binary's last decision, by
    /// `holder` where it wrote its pid ([`LOCK_HELD`]).
    fn lock_held(holder: Option<u32>, span: Duration) -> Self {
        let holder = holder.map_or_else(|| "another waiter".to_owned(), |pid| format!("pid {pid}"));
        Self::new(
            "ledger",
            vec!["ledger"],
            format!(
                "{holder} holds the ledger's lock: no look has read the ledger for {}",
                aterm_toolchain::stall::words(span)
            ),
        )
    }
}

/// How long the looks for a binary may meet only the ledger's lock, from the first of them since
/// its last decision, before the lock is its reason ([`Wait::Unread`]): a minute.
pub(super) const LOCK_HELD: Duration = Duration::from_secs(60);

/// WHAT A LOOK FOUND for a binary it did not start ([`Trace::waits`]).
#[derive(Clone, Debug)]
pub(super) enum Wait {
    /// A decision: the binary waits for this reason.
    For(Reason),
    /// NO DECISION: the look met another admission's hold of the ledger's lock (that holder,
    /// where it wrote its pid) and read nothing, so it says nothing of why the binary waits. A
    /// row keeps its reason across such looks and counts them; once they have gone on for
    /// [`LOCK_HELD`] from the first of them, with no decision between, the lock is the reason.
    Unread(Option<u32>),
}

impl Wait {
    /// What a ledger look short of `short` found: nothing decided where it met only the
    /// ledger's lock ([`aterm_capacity::admit::Short::Turn`]); any shortfall it read, an I/O
    /// fault of the ledger among them, is a reason.
    pub(super) fn ledger(short: &aterm_capacity::admit::Shortfall) -> Self {
        use aterm_capacity::admit::Short;
        match short.shorts[..] {
            [Short::Turn { holder }, ..]
                if short
                    .shorts
                    .iter()
                    .all(|short| matches!(short, Short::Turn { .. })) =>
            {
                Self::Unread(holder)
            }
            _ => Self::For(Reason::ledger(short)),
        }
    }
}

struct Waiting {
    since: Duration,
    first_detail: String,
    reason: Reason,
    /// When the first look since its last decision that met only the ledger's lock was made,
    /// where the looks since that decision met only the lock ([`Wait::Unread`]).
    unread_since: Option<Duration>,
    /// The looks since it opened that decided nothing ([`Wait::Unread`]).
    undecided: u64,
}

impl Waiting {
    fn opened(reason: Reason, at: Duration) -> Self {
        Self {
            since: at,
            first_detail: reason.detail.clone(),
            reason,
            unread_since: None,
            undecided: 0,
        }
    }
}

struct Closed {
    index: usize,
    from: Duration,
    to: Duration,
    first_detail: String,
    reason: Reason,
    /// The looks it saw that decided nothing.
    undecided: u64,
}

struct Book(Vec<Option<Waiting>>);

impl Book {
    fn new(plan: &Plan, count: usize) -> Self {
        let mut rows: Vec<_> = (0..count).map(|_| None).collect();
        for b in &plan.order {
            let Some(slot) = rows.get_mut(b.index) else {
                continue;
            };
            *slot = Some(Waiting::opened(Reason::ordered(), Duration::ZERO));
        }
        Self(rows)
    }

    /// Row `index` at `at`: `next` its reason from now on, or none (closed). A reason of the class
    /// it has updates its detail; any other closes it and opens one.
    fn change(&mut self, index: usize, next: Option<Reason>, at: Duration) -> Option<Closed> {
        let slot = self.0.get_mut(index)?;
        if let (Some(current), Some(next)) = (slot.as_mut(), next.as_ref())
            && current.reason.classification() == next.classification()
        {
            current.reason.detail.clone_from(&next.detail);
            current.unread_since = None;
            return None;
        }
        let previous = slot.take();
        *slot = next.map(|reason| Waiting::opened(reason, at));
        previous.map(|previous| Closed {
            index,
            from: previous.since,
            to: at,
            first_detail: previous.first_detail,
            reason: previous.reason,
            undecided: previous.undecided,
        })
    }

    /// What a look at `at` found for the binary of row `index`: a reason is a decision
    /// ([`Self::change`]); a look that met only the ledger's lock leaves the row as it is and
    /// is counted in it, until such looks have gone on for [`LOCK_HELD`] from the first of them
    /// since its last decision, when the lock is its reason. The lock's span runs on from that
    /// first look while the looks meet only the lock. A closed row stays closed.
    fn wait(&mut self, index: usize, wait: Wait, at: Duration) -> Option<Closed> {
        match wait {
            Wait::For(reason) => self.change(index, Some(reason), at),
            Wait::Unread(holder) => {
                let row = self.0.get_mut(index)?.as_mut()?;
                let first = *row.unread_since.get_or_insert(at);
                let unread = at.saturating_sub(first);
                if unread < LOCK_HELD {
                    row.undecided = row.undecided.saturating_add(1);
                    return None;
                }
                let closed = self.change(index, Some(Reason::lock_held(holder, unread)), at);
                if let Some(row) = self.0.get_mut(index).and_then(Option::as_mut) {
                    row.unread_since = Some(first);
                }
                closed
            }
        }
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
        let mut rows: Vec<_> = (0..bins.len()).map(|_| None).collect();
        rows[index] = Some(Waiting::opened(Reason::ordered(), Duration::ZERO));
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

    /// A look did not start binary `index`, having found `wait` ([`Book::wait`]).
    pub(super) fn waits(&self, index: usize, wait: Wait) {
        self.waits_at(index, wait, self.began.elapsed());
    }

    /// [`Self::waits`] at `at` since the trace began.
    fn waits_at(&self, index: usize, wait: Wait, at: Duration) {
        if self.ctx.is_none() {
            return;
        }
        let row = self
            .book
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .wait(index, wait, at);
        self.closed(row);
    }

    /// Binary `index` left the queue: its permit came back, granted or refused (a run its
    /// pool paid no tokens for). Its wait ends.
    pub(super) fn started(&self, index: usize) {
        self.started_at(index, self.began.elapsed());
    }

    /// [`Self::started`] at `at` since the trace began.
    fn started_at(&self, index: usize, at: Duration) {
        if self.ctx.is_none() {
            return;
        }
        let row = self
            .book
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .change(index, None, at);
        self.closed(row);
    }

    /// Write the event of a row that closed, with the looks it saw that decided nothing.
    fn closed(&self, row: Option<Closed>) {
        let Some(ctx) = self.ctx else { return };
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
                    "undecided_looks":row.undecided,
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
            self.started(index);
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
            book: Book(vec![Some(Waiting::opened(reason, Duration::ZERO))]),
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
