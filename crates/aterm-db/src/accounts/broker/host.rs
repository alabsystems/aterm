// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A bounded, poll-driven owner for the private request transport. The embedding
//! application's service loop calls `poll`; this module starts no daemon.

use super::{
    Broker, Selection, Token,
    transport::{self, AdapterReply, Endpoint},
};
use crate::resources::BoundRequest;
use aterm_uds::CtlStream;
use std::{
    io,
    net::Shutdown,
    path::Path,
    sync::{Arc, Mutex},
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

type Prepared = Box<dyn FnOnce(&BoundRequest, &[u8], Instant) -> Option<AdapterReply> + Send>;
type Adapter = dyn Fn(&str, &[u8], Instant) -> io::Result<Prepared> + Send + Sync;

/// Hard bounds on resident handlers and one accepted exchange.
#[derive(Clone, Copy, Debug)]
pub struct Limits {
    workers: usize,
    exchange: Duration,
}

impl Limits {
    /// # Errors
    /// Refuses zero or more than sixteen workers, or an exchange outside one
    /// millisecond through sixty seconds.
    pub fn new(workers: usize, exchange: Duration) -> io::Result<Self> {
        if !(1..=16).contains(&workers)
            || !(Duration::from_millis(1)..=Duration::from_secs(60)).contains(&exchange)
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "broker host limits",
            ));
        }
        Ok(Self { workers, exchange })
    }
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            workers: 4,
            exchange: Duration::from_secs(60),
        }
    }
}

/// Local lifecycle evidence. Counts contain no account, request or secret data.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Snapshot {
    pub accepting: bool,
    /// Includes completed threads until `poll` reaps them.
    pub workers: usize,
    pub finished: u64,
    /// Failed exchanges and panicked handlers; neither implies a refund.
    pub failed: u64,
}

#[derive(Default)]
struct Gate {
    stopped: Mutex<bool>,
}

impl Gate {
    fn enter(&self, deadline: Instant) -> bool {
        // Never hold the gate across storage, provider code or a join. Entry is
        // authorized here; an authorized callback may finish after stop.
        let stopped = self.stopped.lock().unwrap_or_else(|p| p.into_inner());
        !*stopped && Instant::now() < deadline
    }

    fn stop(&self) {
        *self.stopped.lock().unwrap_or_else(|p| p.into_inner()) = true;
    }
}

struct Worker {
    cancel: CtlStream,
    task: JoinHandle<io::Result<()>>,
}

/// Trusted local service owner. Consumes its broker so stopping the host leaves
/// no public issuance path. A new host needs a new broker and new tokens.
///
/// The adapter must honor its absolute deadline. Shutdown closes sockets and
/// admission, but cannot interrupt an arbitrary in-process callback. `Drop`
/// stops without waiting; call `drain_until` to observe all workers finishing.
pub struct Host {
    endpoint: Option<Endpoint>,
    broker: Arc<Broker>,
    adapter: Arc<Adapter>,
    gate: Arc<Gate>,
    limits: Limits,
    workers: Vec<Worker>,
    finished: u64,
    failed: u64,
}

impl Host {
    /// Bind an existing private directory without replacing any previous path.
    /// # Errors
    /// The endpoint's privacy, identity and socket errors.
    pub fn bind(
        dir: &Path,
        broker: Broker,
        limits: Limits,
        adapter: impl Fn(&BoundRequest, &[u8], Instant) -> Option<AdapterReply> + Send + Sync + 'static,
    ) -> io::Result<Self> {
        Self::bind_prepared(
            dir,
            broker,
            limits,
            |_, _, _| Ok(()),
            move |(), request, credential, until| adapter(request, credential, until),
        )
    }

    /// Bind an adapter that parses authenticated input before billing. The
    /// resulting owned value is delivered once, only after durable admission.
    /// Preparation must perform no provider effect and honor its deadline.
    /// # Errors
    /// The endpoint's privacy, identity and socket errors.
    pub fn bind_prepared<T: Send + 'static>(
        dir: &Path,
        broker: Broker,
        limits: Limits,
        prepare: impl Fn(&str, &[u8], Instant) -> io::Result<T> + Send + Sync + 'static,
        run: impl Fn(T, &BoundRequest, &[u8], Instant) -> Option<AdapterReply> + Send + Sync + 'static,
    ) -> io::Result<Self> {
        let run = Arc::new(run);
        let adapter = move |name: &str, bytes: &[u8], until: Instant| -> io::Result<Prepared> {
            let prepared = prepare(name, bytes, until)?;
            let run = run.clone();
            Ok(Box::new(move |request, credential, deadline| {
                run(prepared, request, credential, deadline)
            }))
        };
        Ok(Self {
            endpoint: Some(Endpoint::bind(dir)?),
            broker: Arc::new(broker),
            adapter: Arc::new(adapter),
            gate: Arc::new(Gate::default()),
            limits,
            workers: Vec::with_capacity(limits.workers),
            finished: 0,
            failed: 0,
        })
    }

    /// Trusted control only. No socket request can call this operation.
    /// # Errors
    /// A stopped host or the broker's current grant/billing/lease refusals.
    pub fn issue_until(
        &self,
        grant: [u8; 16],
        within: Duration,
        deadline: Instant,
    ) -> crate::Result<Token> {
        self.issue_selected_until(&Selection::Grant(grant), within, deadline)
    }

    /// Trusted selection follows the same current-policy checks as explicit IDs.
    /// # Errors
    /// A stopped host, ambiguous selection, or current grant/billing refusal.
    pub fn issue_selected_until(
        &self,
        selection: &Selection,
        within: Duration,
        deadline: Instant,
    ) -> crate::Result<Token> {
        if self.endpoint.is_none() {
            return Err(crate::Error::new("broker host stopped"));
        }
        self.broker
            .issue_selected_until(selection, within, deadline)
    }

    /// Trusted control revokes a particular launch without revoking a newer
    /// token issued from the same grant. May also be called after stopping.
    /// # Errors
    /// Invalid token shape or broker authentication-state errors.
    pub fn revoke(&self, token: &[u8]) -> crate::Result<bool> {
        self.broker.revoke(token)
    }

    /// Trusted control revokes the current lease for a grant. Use `revoke`
    /// when an exiting old launch must not revoke its replacement.
    /// # Errors
    /// Broker authentication-state errors.
    pub fn revoke_grant(&self, grant: [u8; 16]) -> crate::Result<bool> {
        self.broker.revoke_grant(grant)
    }

    pub fn snapshot(&self) -> Snapshot {
        Snapshot {
            accepting: self.endpoint.is_some(),
            workers: self.workers.len(),
            finished: self.finished,
            failed: self.failed,
        }
    }

    /// Reap finished handlers and accept only the available slots. Never waits
    /// for a connection or worker; there is no accepted-stream queue. Kernel
    /// backlog delay remains possible and is not a cancellation guarantee.
    /// # Errors
    /// Accept/identity, descriptor or thread creation errors. These stop intake
    /// and cancel all current sockets; admitted effects remain charged.
    pub fn poll(&mut self) -> io::Result<Snapshot> {
        self.poll_with(true, || {})
    }

    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbBrokerHost",
            action = "Saturated",
            project = "aterm_db::accounts::billing::observation::tests::host_transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbBrokerHost",
            action = "StopWaiting",
            project = "aterm_db::accounts::billing::observation::tests::host_transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbBrokerHost",
            action = "StopAdmitted",
            project = "aterm_db::accounts::billing::observation::tests::host_transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbBrokerHost",
            action = "StopAtGate",
            project = "aterm_db::accounts::billing::observation::tests::host_transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbBrokerHost",
            action = "PanicReap",
            project = "aterm_db::accounts::billing::observation::tests::host_transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbBrokerHost",
            action = "TokenChild",
            project = "aterm_db::accounts::billing::observation::tests::host_transition"
        )
    )]
    fn poll_with(
        &mut self,
        check_stop: bool,
        before_entry: impl Fn() + Send + Sync + 'static,
    ) -> io::Result<Snapshot> {
        self.reap();
        let before_entry = Arc::new(before_entry);
        while self.endpoint.is_some() && self.workers.len() < self.limits.workers {
            let result = (|| {
                let Some(stream) = self
                    .endpoint
                    .as_ref()
                    .expect("open endpoint")
                    .try_accept()?
                else {
                    return Ok(None);
                };
                let deadline = Instant::now() + self.limits.exchange;
                let cancel = aterm_uds::kept_out(|| stream.try_clone())?;
                let broker = self.broker.clone();
                let adapter = self.adapter.clone();
                let gate = self.gate.clone();
                let before_entry = before_entry.clone();
                let task =
                    thread::Builder::new()
                        .name("account-broker".into())
                        .spawn(move || {
                            transport::serve_prepared_until(
                                stream,
                                &broker,
                                deadline,
                                (
                                    |until| gate.enter(until),
                                    |until| {
                                        before_entry();
                                        !check_stop || gate.enter(until)
                                    },
                                ),
                                |name, bytes, until| adapter(name, bytes, until),
                                |prepared, request, credential, until| {
                                    prepared(request, credential, until)
                                },
                            )
                        })?;
                Ok(Some(Worker { cancel, task }))
            })();
            match result {
                Ok(Some(worker)) => self.workers.push(worker),
                Ok(None) => break,
                Err(error) => {
                    self.stop();
                    return Err(error);
                }
            }
        }
        Ok(self.snapshot())
    }

    fn reap(&mut self) {
        let mut index = 0;
        while index < self.workers.len() {
            if self.workers[index].task.is_finished() {
                let worker = self.workers.swap_remove(index);
                self.finished = self.finished.saturating_add(1);
                if !matches!(worker.task.join(), Ok(Ok(()))) {
                    self.failed = self.failed.saturating_add(1);
                }
            } else {
                index += 1;
            }
        }
    }

    /// Close admission before cancelling sockets, including incomplete frames.
    /// A request charged before the gate closes may remain unknown without
    /// entering the adapter. Shutdown never refunds or retries it.
    pub fn stop(&mut self) -> Snapshot {
        self.gate.stop();
        drop(self.endpoint.take());
        for worker in &self.workers {
            let _ = worker.cancel.shutdown(Shutdown::Both);
        }
        self.reap();
        self.snapshot()
    }

    /// Stop and join completed workers until none remain or the deadline passes.
    /// # Errors
    /// Timeout leaves unfinished workers owned by this host and visible through
    /// `snapshot`; it does not claim their provider work was cancelled.
    pub fn drain_until(&mut self, deadline: Instant) -> io::Result<Snapshot> {
        self.stop();
        loop {
            self.reap();
            if self.workers.is_empty() {
                return Ok(self.snapshot());
            }
            let remaining = deadline
                .checked_duration_since(Instant::now())
                .filter(|n| !n.is_zero())
                .ok_or_else(|| {
                    io::Error::new(io::ErrorKind::TimedOut, "broker workers still running")
                })?;
            thread::sleep(remaining.min(Duration::from_millis(5)));
        }
    }
}

impl Drop for Host {
    fn drop(&mut self) {
        self.stop();
        // Dropping a JoinHandle detaches it. In-process adapter termination is
        // a trusted contract, not a promise Rust can force on arbitrary code.
    }
}

#[cfg(test)]
pub(crate) fn poll_before_entry(
    host: &mut Host,
    check_stop: bool,
    before: impl Fn() + Send + Sync + 'static,
) -> io::Result<Snapshot> {
    host.poll_with(check_stop, before)
}
