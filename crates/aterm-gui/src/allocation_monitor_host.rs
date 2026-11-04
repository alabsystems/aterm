// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Bounded monitor attachment worker, independent of system vitals and native jobs.
//! Only control workers attach sockets; UI publication clones immutable state.

use aterm_capacity::host_protocol::{Failure, Generation};
use atpkg::build_admit::service::{
    Gap,
    monitor::{Client, EventKind, Observation, RecordSummary},
};
use std::{
    io,
    net::Shutdown,
    sync::{Arc, Condvar, Mutex, PoisonError},
    time::{Duration, Instant},
};

const READ_WITHIN: Duration = Duration::from_secs(2);
const RECENT_TERMINALS: usize = 64;
/// The production notifier cannot retain Host or arbitrary callbacks. A generic
/// retained closure could otherwise form a cycle back into its own worker.
#[derive(Clone)]
pub(crate) enum Notify {
    EventLoop(aterm_window::event_loop::EventLoopProxy<crate::Wake>),
    #[cfg(test)]
    Callback(Arc<dyn Fn() + Send + Sync>),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Phase {
    Unattached,
    Queued,
    Reading,
    Observed,
    Failed(Failure),
}

#[derive(Clone, Debug)]
pub(crate) struct Reading {
    pub(crate) attachment: u64,
    pub(crate) observation: Arc<Observation>,
    /// At most 64 recent delivered terminal outcomes in ascending source revision.
    /// These records belong to this exact attachment and source generation.
    pub(crate) recent: Arc<[RecordSummary]>,
    /// Delivered terminal records evicted locally, not missing source events.
    pub(crate) omitted_terminals: u64,
    /// Most recent explicit lost range for this attachment, retained even when
    /// subsequent pages or heartbeats carry no new gap. This is not a union.
    pub(crate) last_gap: Option<Gap>,
}

#[derive(Clone, Debug)]
pub(crate) struct Presentation {
    pub(crate) requested: u64,
    pub(crate) desired_generation: Option<Generation>,
    pub(crate) phase: Phase,
    /// Last actual reading, retaining its original attachment and receipt age.
    /// A pending/failed replacement never relabels this as its own observation.
    pub(crate) last: Option<Reading>,
}

impl Default for Presentation {
    fn default() -> Self {
        Self {
            requested: 0,
            desired_generation: None,
            phase: Phase::Unattached,
            last: None,
        }
    }
}

struct Request {
    revision: u64,
    client: Client,
    cancel: aterm_uds::CtlStream,
}

struct State {
    view: Arc<Presentation>,
    pending: Option<Request>,
    active: Option<u64>,
    cancel: Option<aterm_uds::CtlStream>,
    stopped: bool,
    notify: Option<Notify>,
    wake_pending: bool,
}

struct Shared {
    state: Mutex<State>,
    changed: Condvar,
}

struct Control {
    shared: Arc<Shared>,
    // Separate from publication: OS thread creation never holds the UI state lock.
    started: Mutex<bool>,
}

/// Clones share one lazy I/O worker and one replaceable pending request. Last
/// handle drop stops/cancels its own monitor sockets without joining on the UI.
#[derive(Clone)]
pub(crate) struct Host(Arc<Control>);

impl Default for Host {
    fn default() -> Self {
        Self(Arc::new(Control {
            shared: Arc::new(Shared {
                state: Mutex::new(State {
                    view: Arc::new(Presentation::default()),
                    pending: None,
                    active: None,
                    cancel: None,
                    stopped: false,
                    notify: None,
                    wake_pending: false,
                }),
                changed: Condvar::new(),
            }),
            started: Mutex::new(false),
        }))
    }
}

impl Host {
    /// Called by a control wait worker, never the UI or vitals thread. Trusted
    /// descriptor delivery and generation metadata precede this boundary.
    /// Successful return means queued; only a later Observed phase proves a read.
    pub(crate) fn attach(
        &self,
        client: Client,
        generation: Generation,
        notify: Notify,
    ) -> io::Result<u64> {
        let stream = client.into_stream();
        let cancel = aterm_uds::kept_out(|| stream.try_clone())?;
        let client = Client::from_trusted_stream(stream, generation)?;
        self.start()?;
        let (revision, old, active_cancel, old_notify) = {
            let mut state = self
                .0
                .shared
                .state
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            if state.stopped {
                return Err(io::Error::other("allocation monitor worker stopped"));
            }
            let revision = state.view.requested.checked_add(1).ok_or_else(|| {
                io::Error::other("allocation monitor attachment revision exhausted")
            })?;
            let last = state.view.last.clone();
            state.view = Arc::new(Presentation {
                requested: revision,
                desired_generation: Some(generation),
                phase: Phase::Queued,
                last,
            });
            let old = state.pending.replace(Request {
                revision,
                client,
                cancel,
            });
            let active_cancel = state.cancel.take();
            let old_notify = state.notify.replace(notify.clone());
            // Each newly admitted attachment must notify its current consumer;
            // subsequent worker phase notifications coalesce until take_view.
            state.wake_pending = true;
            (revision, old, active_cancel, old_notify)
        };
        drop(old_notify);
        drop(old);
        cancel_socket(active_cancel);
        self.0.shared.changed.notify_one();
        post(notify);
        Ok(revision)
    }

    fn start(&self) -> io::Result<()> {
        let mut started = self
            .0
            .started
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if !*started {
            let shared = Arc::clone(&self.0.shared);
            std::thread::Builder::new()
                .name("allocation-monitor".into())
                .spawn(move || worker(shared))?;
            *started = true;
        }
        Ok(())
    }

    /// Immutable authoritative state; no socket I/O or probing occurs here.
    #[cfg(test)]
    pub(crate) fn view(&self) -> Arc<Presentation> {
        Arc::clone(
            &self
                .0
                .shared
                .state
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .view,
        )
    }

    /// UI wake consumption also acknowledges the coalesced phase notification.
    pub(crate) fn take_view(&self) -> Arc<Presentation> {
        let mut state = self
            .0
            .shared
            .state
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        state.wake_pending = false;
        Arc::clone(&state.view)
    }
}

impl Drop for Control {
    fn drop(&mut self) {
        let (pending, cancel, notify) = {
            let mut state = self
                .shared
                .state
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            state.stopped = true;
            (
                state.pending.take(),
                state.cancel.take(),
                state.notify.take(),
            )
        };
        drop(notify);
        drop(pending);
        cancel_socket(cancel);
        self.shared.changed.notify_one();
    }
}

fn cancel_socket(cancel: Option<aterm_uds::CtlStream>) {
    if let Some(cancel) = cancel {
        // This actual duplicate controls only its delegated read connection.
        let _ = cancel.shutdown(Shutdown::Both);
    }
}

fn post(notify: Notify) {
    // Notification is best effort; a caller callback cannot strand the worker.
    // The immutable view remains authoritative if a UI event loop has closed.
    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| match notify {
        Notify::EventLoop(proxy) => {
            let _ = proxy.send_event(crate::Wake::AllocationMonitorChanged);
        }
        #[cfg(test)]
        Notify::Callback(callback) => callback(),
    }));
}

fn notification(state: &mut State) -> Option<Notify> {
    if state.wake_pending {
        return None;
    }
    state.wake_pending = true;
    state.notify.clone()
}

fn worker(shared: Arc<Shared>) {
    loop {
        let (request, notify) = {
            let mut state = shared.state.lock().unwrap_or_else(PoisonError::into_inner);
            while state.pending.is_none() && !state.stopped {
                state = shared
                    .changed
                    .wait(state)
                    .unwrap_or_else(PoisonError::into_inner);
            }
            if state.stopped {
                return;
            }
            let Request {
                revision,
                client,
                cancel,
            } = state.pending.take().expect("pending request");
            state.active = Some(revision);
            state.cancel = Some(cancel);
            let mut view = (*state.view).clone();
            view.phase = Phase::Reading;
            state.view = Arc::new(view);
            ((revision, client), notification(&mut state))
        };
        if let Some(notify) = notify {
            post(notify);
        }
        let (revision, mut client) = request;
        loop {
            let result = client.read_next(Instant::now() + READ_WITHIN);
            let delivery = complete(&shared, revision, result);
            if let Some(notify) = delivery.notify {
                post(notify);
            }
            if !delivery.keep_reading {
                break;
            }
        }
    }
}

struct Delivery {
    notify: Option<Notify>,
    keep_reading: bool,
}

fn complete(shared: &Shared, revision: u64, result: io::Result<Observation>) -> Delivery {
    let mut state = shared.state.lock().unwrap_or_else(PoisonError::into_inner);
    let current = !state.stopped && state.view.requested == revision;
    let keep_reading = current
        && result.as_ref().is_ok_and(|observation| {
            // Closure does not skip history still behind the current snapshot.
            !observation.snapshot.closed
                || observation.next.revision < observation.snapshot.revision
        });
    if !keep_reading && state.active == Some(revision) {
        state.active = None;
        state.cancel.take();
    }
    if !current {
        return Delivery {
            notify: None,
            keep_reading: false,
        };
    }
    let mut view = (*state.view).clone();
    match result {
        Ok(observation) => {
            let prior = view.last.as_ref().filter(|last| {
                last.attachment == revision
                    && last.observation.snapshot.generation == observation.snapshot.generation
            });
            if keep_reading
                && view.phase == Phase::Observed
                && prior.is_some_and(|last| {
                    last.observation.snapshot == observation.snapshot
                        && last.observation.next == observation.next
                        && observation.gap.is_none_or(|gap| Some(gap) == last.last_gap)
                })
            {
                // An unchanged heartbeat advances only wire liveness. Keep the
                // original Arc/age interval and don't schedule a repaint for it.
                return Delivery {
                    notify: None,
                    keep_reading,
                };
            }
            let last_gap = observation.gap.or(prior.and_then(|last| last.last_gap));
            let (recent, omitted_terminals) = recent_terminals(prior, &observation);
            view.phase = Phase::Observed;
            view.last = Some(Reading {
                attachment: revision,
                observation: Arc::new(observation),
                recent,
                omitted_terminals,
                last_gap,
            });
        }
        Err(error) => view.phase = Phase::Failed(Failure::of(&error)),
    }
    state.view = Arc::new(view);
    let notify = notification(&mut state);
    // Retire callbacks after releasing the lock: the last Host captured by one
    // can drop Control, whose shutdown takes this same state lock.
    let retired = if keep_reading {
        None
    } else {
        state.notify.take()
    };
    drop(state);
    drop(retired);
    Delivery {
        notify,
        keep_reading,
    }
}

fn recent_terminals(
    prior: Option<&Reading>,
    observation: &Observation,
) -> (Arc<[RecordSummary]>, u64) {
    // The snapshot can be ahead of the delivered page. Only the continuation
    // cursor excludes records already delivered, including locally evicted ones.
    let mut delivered = prior.map_or(0, |last| last.observation.next.revision);
    let mut omitted = prior.map_or(0, |last| last.omitted_terminals);
    let mut recent = None;
    for record in &observation.history {
        if record.revision <= delivered {
            continue;
        }
        delivered = record.revision;
        if !matches!(record.kind, EventKind::Finished { .. } | EventKind::Failed) {
            continue;
        }
        let retained = recent.get_or_insert_with(|| {
            prior.map_or_else(std::collections::VecDeque::new, |last| {
                last.recent.iter().cloned().collect()
            })
        });
        if retained.len() == RECENT_TERMINALS {
            retained.pop_front();
            omitted = omitted.saturating_add(1);
        }
        retained.push_back(record.clone());
    }
    let recent = recent.map_or_else(
        || prior.map_or_else(|| Arc::from([]), |last| Arc::clone(&last.recent)),
        |records| Arc::from(Vec::from(records)),
    );
    (recent, omitted)
}

#[cfg(test)]
#[path = "allocation_monitor_host_tests.rs"]
mod tests;
