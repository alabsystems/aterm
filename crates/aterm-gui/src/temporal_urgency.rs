// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Scheduling demand owned by real recorder work and its waiting consumers.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock, PoisonError};
use std::thread::{JoinHandle, Thread};

use crate::qos::{Role, RoleObservation};

/// Why a consumer needs the shared writer to run responsively.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Reason {
    Drain,
    View,
    Pressure,
}

impl Reason {
    const fn index(self) -> usize {
        match self {
            Self::Drain => 0,
            Self::View => 1,
            Self::Pressure => 2,
        }
    }
}

/// Live demands and the last role transition made by this writer.
pub(super) struct Demands {
    total: AtomicUsize,
    counts: [AtomicUsize; 3],
    observed: Mutex<Option<RoleObservation>>,
    /// The writer these demands are for, once it has adopted them ([`Self::adopt_writer`]).
    writer: OnceLock<Thread>,
    /// Keep the unjoined worker handle valid while another thread donates priority.
    priority: Mutex<WriterPriority>,
}

#[derive(Default)]
struct WriterPriority {
    // Fields drop in declaration order: end the override before releasing its thread handle.
    boost: Option<crate::qos::Boost>,
    worker: Option<Arc<JoinHandle<()>>>,
}

impl Demands {
    pub(super) fn new() -> Arc<Self> {
        Arc::new(Self {
            total: AtomicUsize::new(0),
            counts: std::array::from_fn(|_| AtomicUsize::new(0)),
            observed: Mutex::new(None),
            writer: OnceLock::new(),
            priority: Mutex::new(WriterPriority::default()),
        })
    }

    /// Name `writer` as the thread these demands are for, so each new demand
    /// unparks it ([`Self::acquire`]): a writer napping off its channel
    /// (`temporal::serve`) then serves the demand at once. The first adoption
    /// stands.
    pub(super) fn adopt_writer(&self, writer: Thread) {
        let _ = self.writer.set(writer);
    }

    /// Publish the owned handle before readers can send work. A demand may predate startup.
    pub(super) fn adopt_worker(&self, worker: Arc<JoinHandle<()>>) {
        let mut priority = self.priority.lock().unwrap_or_else(PoisonError::into_inner);
        if priority.worker.is_none() {
            priority.worker = Some(worker);
        }
        self.refresh_priority(&mut priority);
    }

    fn refresh_priority(&self, priority: &mut WriterPriority) {
        if self.is_urgent() {
            if priority.boost.is_none()
                && let Some(worker) = &priority.worker
            {
                priority.boost = Some(crate::qos::Boost::start(Some(worker)));
            }
        } else {
            drop(priority.boost.take());
        }
    }

    fn priority_changed(&self) {
        let mut priority = self.priority.lock().unwrap_or_else(PoisonError::into_inner);
        self.refresh_priority(&mut priority);
    }

    pub(super) fn acquire(self: &Arc<Self>, reason: Reason) -> Demand {
        self.acquire_with(reason, Thread::unpark)
    }

    /// Inject the wake operation at the adopted-writer boundary.
    pub(super) fn acquire_with(
        self: &Arc<Self>,
        reason: Reason,
        wake: impl FnOnce(&Thread),
    ) -> Demand {
        // Publish urgency before its diagnostic reason, so a handoff between
        // different reason counters cannot produce a false idle observation.
        let previous = self
            .total
            .try_update(Ordering::AcqRel, Ordering::Acquire, |count| {
                count.checked_add(1)
            })
            .expect("recorder urgency count exhausted");
        self.counts[reason.index()].fetch_add(1, Ordering::AcqRel);
        if previous == 0 {
            self.priority_changed();
        }
        if let Some(writer) = self.writer.get() {
            wake(writer);
        }
        Demand {
            owner: self.clone(),
            reason,
        }
    }

    pub(super) fn is_urgent(&self) -> bool {
        self.is_urgent_with(|| {})
    }

    pub(super) fn is_urgent_with(&self, after_read: impl FnOnce()) -> bool {
        let urgent = self.total.load(Ordering::Acquire) != 0;
        after_read();
        urgent
    }

    pub(super) fn snapshot(&self) -> Snapshot {
        Snapshot {
            counts: std::array::from_fn(|i| self.counts[i].load(Ordering::Acquire)),
            observation: *self.observed.lock().unwrap_or_else(PoisonError::into_inner),
            priority_override: {
                #[cfg(target_os = "macos")]
                {
                    let priority = self.priority.lock().unwrap_or_else(PoisonError::into_inner);
                    priority.worker.as_ref().map(|_| {
                        priority
                            .boost
                            .as_ref()
                            .is_some_and(crate::qos::Boost::armed)
                    })
                }
                #[cfg(not(target_os = "macos"))]
                {
                    None
                }
            },
        }
    }
}

/// An owned reason to finish work promptly; dropping it releases that reason.
pub(crate) struct Demand {
    owner: Arc<Demands>,
    reason: Reason,
}

impl Drop for Demand {
    fn drop(&mut self) {
        let previous = self.owner.counts[self.reason.index()].fetch_sub(1, Ordering::AcqRel);
        debug_assert!(previous != 0, "unbalanced recorder urgency");
        let previous = self.owner.total.fetch_sub(1, Ordering::AcqRel);
        debug_assert!(previous != 0, "unbalanced aggregate recorder urgency");
        if previous == 1 {
            self.owner.priority_changed();
        }
    }
}

pub(super) fn global() -> &'static Arc<Demands> {
    static DEMANDS: OnceLock<Arc<Demands>> = OnceLock::new();
    DEMANDS.get_or_init(Demands::new)
}

/// Urgency belongs to the view, not to a timer or an observation of status.
pub(super) fn consumer() -> Demand {
    global().acquire(Reason::View)
}

/// The shared writer alone owns this role cache and changes its own role.
pub(super) struct WriterRole {
    demands: Arc<Demands>,
    attempted: Option<Role>,
}

impl WriterRole {
    pub(super) fn new(demands: Arc<Demands>) -> Self {
        Self {
            demands,
            attempted: None,
        }
    }

    pub(super) fn poll(&mut self) {
        self.poll_with(cfg!(target_os = "macos"), crate::qos::set_self_observed);
    }

    /// The role setter is injected only at this writer-owned boundary.
    pub(super) fn poll_with(
        &mut self,
        dynamic: bool,
        mut apply: impl FnMut(Role) -> RoleObservation,
    ) {
        // Other platforms retain the existing Responsive role. In particular,
        // Linux's Background setter does not undo an EEVDF slice request.
        let wanted = if dynamic && !self.demands.is_urgent() {
            Role::Background
        } else {
            Role::Responsive
        };
        if self.attempted != Some(wanted) {
            let reading = apply(wanted);
            *self
                .demands
                .observed
                .lock()
                .unwrap_or_else(PoisonError::into_inner) = Some(reading);
            // This caches the attempt, not success. A native error remains in
            // the observation; an unchanged demand must not spin on syscalls.
            self.attempted = Some(wanted);
        }
    }
}

/// Value-free scheduling evidence; requested and observed roles are distinct.
#[derive(Clone, Copy, Debug)]
pub(super) struct Snapshot {
    pub(super) counts: [usize; 3],
    pub(super) observation: Option<RoleObservation>,
    pub(super) priority_override: Option<bool>,
}

impl Snapshot {
    pub(super) fn fields(self) -> String {
        let number = |value: Option<i32>| value.map_or_else(|| "-".to_owned(), |v| v.to_string());
        let (requested, observed, class, set_errno, read_errno) = match self.observation {
            Some(reading) => (
                name(reading.requested),
                reading.observed.map_or(
                    if reading.class.is_some() {
                        "other"
                    } else {
                        "unknown"
                    },
                    name,
                ),
                reading
                    .class
                    .map_or_else(|| "-".to_owned(), |v| v.to_string()),
                number(reading.set_errno),
                number(reading.read_errno),
            ),
            None => ("unknown", "unknown", "-".into(), "-".into(), "-".into()),
        };
        format!(
            " writer_role_requested={requested} writer_qos_observed={observed} \
             writer_qos_class={class} writer_qos_set_errno={set_errno} \
             writer_qos_read_errno={read_errno} writer_urgent_drain={} \
             writer_urgent_view={} writer_urgent_pressure={} writer_priority_override={}",
            self.counts[0],
            self.counts[1],
            self.counts[2],
            self.priority_override
                .map_or("unavailable", |active| if active {
                    "active"
                } else {
                    "inactive"
                })
        )
    }
}

const fn name(role: Role) -> &'static str {
    match role {
        Role::Background => "utility",
        Role::Responsive => "responsive",
        Role::Interactive => "interactive",
        Role::Housekeeping => "background",
    }
}
