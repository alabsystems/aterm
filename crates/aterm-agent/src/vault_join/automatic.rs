// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The node generation's automatic admission worker. An ask connection only verifies and
//! acknowledges a request; reading membership and posting to the vault happen on this worker.

use aterm_secrets::client::Paths;
use aterm_secrets::members::desk::{self, Desk, Local};
use aterm_secrets::{Error, Result};
use aterm_types::MutexExt as _;
use std::net::IpAddr;
use std::sync::{Arc, Condvar, Mutex, PoisonError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

const REFRESH_EVERY: Duration = Duration::from_secs(30);
const UNAVAILABLE: &str = "automatic LAN admission is unavailable on this Mac";

/// One node generation's worker: current membership, a bounded desk and automatic submission.
/// Dropping it refuses new requests, wakes the worker and joins its bounded in-flight work.
pub struct AutomaticJoins {
    handle: AutomaticJoinHandle,
    worker: Option<JoinHandle<()>>,
}

/// A connection's access to the node generation's automatic desk. Clones own no worker.
#[derive(Clone)]
pub struct AutomaticJoinHandle {
    shared: Arc<Shared>,
}

struct Shared {
    state: Mutex<State>,
    changed: Condvar,
    backend: Arc<dyn Backend>,
}

#[derive(Default)]
struct State {
    snapshot: Option<Snapshot>,
    valid_until: Option<Instant>,
    /// Invalidates an in-flight state read on a network move or stop.
    epoch: u64,
    /// Coalesces asks while preserving one that arrives during a read or a post.
    requested: u64,
    stopped: bool,
}

struct Snapshot {
    network: String,
    desk: Arc<Mutex<Desk>>,
}

trait Backend: Send + Sync + 'static {
    fn read(&self) -> Result<Option<(Local, String)>>;
    fn forward(
        &self,
        desk: &Mutex<Desk>,
        now: Instant,
        current: &dyn Fn() -> bool,
    ) -> Result<usize>;
    fn now(&self) -> Instant {
        Instant::now()
    }
    #[cfg(test)]
    fn waiting(&self, _until: Instant) {}
}

struct Vault {
    paths: Paths,
}

impl Backend for Vault {
    fn read(&self) -> Result<Option<(Local, String)>> {
        // A move while the local vault was read cannot publish a mixed-network snapshot.
        let before = aterm_secrets::heard::network_key();
        let local = Local::of(&self.paths)?;
        let after = aterm_secrets::heard::network_key();
        Ok(match (local, before, after) {
            (Some(local), Some(before), Some(after)) if before == after => Some((local, after)),
            _ => None,
        })
    }

    fn forward(
        &self,
        desk: &Mutex<Desk>,
        _now: Instant,
        current: &dyn Fn() -> bool,
    ) -> Result<usize> {
        desk::forward_automatic_requests_if(desk, &self.paths, current)
    }
}

impl AutomaticJoins {
    /// Start the worker for this node generation. It serves asks once its first read of local
    /// membership and network succeeds. Every post is preceded by a fresh read, and the desk's
    /// retry deadlines wake it even if no further Mac asks.
    ///
    /// # Errors
    /// The worker thread could not start.
    pub fn start(paths: Paths) -> std::io::Result<Self> {
        Self::with_backend(Arc::new(Vault { paths }))
    }

    fn with_backend(backend: Arc<dyn Backend>) -> std::io::Result<Self> {
        let shared = Arc::new(Shared {
            state: Mutex::new(State::default()),
            changed: Condvar::new(),
            backend,
        });
        let worker = std::thread::Builder::new()
            .name("aterm-auto-join".to_owned())
            .spawn({
                let shared = Arc::clone(&shared);
                move || shared.run()
            })?;
        Ok(Self {
            handle: AutomaticJoinHandle { shared },
            worker: Some(worker),
        })
    }

    /// The handle retained by the listener's ask callback and network watcher.
    #[must_use]
    pub fn handle(&self) -> AutomaticJoinHandle {
        self.handle.clone()
    }
}

impl Drop for AutomaticJoins {
    fn drop(&mut self) {
        {
            let mut state = self.handle.shared.state.lock_or_recover();
            state.stopped = true;
            state.valid_until = None;
            state.epoch = state.epoch.wrapping_add(1);
            self.handle.shared.changed.notify_all();
        }
        if let Some(worker) = self.worker.take()
            && worker.join().is_err()
        {
            aterm_log::warn!("node: the automatic admission worker ended unexpectedly");
        }
    }
}

impl AutomaticJoinHandle {
    /// Invalidate the cached policy and wake its owner after a network move. A state read that
    /// began before this call cannot make the old snapshot available again.
    pub fn refresh(&self) {
        let mut state = self.shared.state.lock_or_recover();
        state.valid_until = None;
        state.epoch = state.epoch.wrapping_add(1);
        self.shared.changed.notify_all();
    }

    /// Verify and acknowledge a new Mac's signed request using current cached LAN policy.
    /// This connection performs no vault I/O, membership read, or wait for a submission. It
    /// offers no manual approval phrase or bundle; the vault's approver decides membership.
    ///
    /// # Errors
    /// Current policy is unavailable, does not cover `peer`, or the signed exchange fails.
    pub fn serve(&self, stream: &mut dyn aterm_net::fleet::Duplex, peer: IpAddr) -> Result<()> {
        let desk = {
            let state = self.shared.state.lock_or_recover();
            state
                .snapshot
                .as_ref()
                .filter(|_| {
                    !state.stopped
                        && state
                            .valid_until
                            .is_some_and(|until| self.shared.backend.now() < until)
                })
                .map(|snapshot| Arc::clone(&snapshot.desk))
        };
        let Some(desk) =
            desk.filter(|desk| desk.lock_or_recover().local().automatically_admits(peer))
        else {
            let _ = aterm_secrets::members::refuse(stream, UNAVAILABLE);
            return Err(Error::new(UNAVAILABLE));
        };
        desk::serve(&desk, stream, peer, &|_| {
            Err(Error::new("manual admission requires a window"))
        })?;
        let mut state = self.shared.state.lock_or_recover();
        state.requested = state.requested.wrapping_add(1);
        self.shared.changed.notify_all();
        Ok(())
    }
}

impl Shared {
    fn run(&self) {
        loop {
            let (epoch, requested) = {
                let state = self.state.lock_or_recover();
                if state.stopped {
                    return;
                }
                (state.epoch, state.requested)
            };
            let current = self.backend.read();
            let next_refresh = self.backend.now() + REFRESH_EVERY;
            let desk = {
                let mut state = self.state.lock_or_recover();
                if state.stopped {
                    return;
                }
                if state.epoch != epoch {
                    continue;
                }
                match current {
                    Ok(Some((local, network))) => {
                        let same = state.snapshot.as_ref().filter(|snapshot| {
                            snapshot.network == network
                                && snapshot.desk.lock_or_recover().local().key() == local.key()
                        });
                        let desk = if let Some(snapshot) = same {
                            snapshot.desk.lock_or_recover().refresh(local);
                            Arc::clone(&snapshot.desk)
                        } else {
                            let mut desk = Desk::new(local);
                            desk.on_network(Some(network.clone()));
                            Arc::new(Mutex::new(desk))
                        };
                        // A network or identity change gets a new desk. An ask already reading
                        // the old greeting can finish only on its old desk, never into this one.
                        state.snapshot = Some(Snapshot {
                            network,
                            desk: Arc::clone(&desk),
                        });
                        state.valid_until = Some(next_refresh);
                        self.changed.notify_all();
                        Some(desk)
                    }
                    result => {
                        state.valid_until = None;
                        if let Err(error) = result {
                            aterm_log::info!("node: automatic admission state will retry: {error}");
                        }
                        None
                    }
                }
            };
            if let Some(desk) = &desk {
                let current = || {
                    let state = self.state.lock_or_recover();
                    !state.stopped && state.epoch == epoch
                };
                match self.backend.forward(desk, self.backend.now(), &current) {
                    Ok(0) => {}
                    Ok(count) => aterm_log::info!(
                        "node: queued {count} LAN requests for automatic ALab admission"
                    ),
                    Err(error) => {
                        aterm_log::info!("node: automatic ALab admission will retry: {error}")
                    }
                }
            }
            let until = desk
                .as_ref()
                .and_then(|desk| {
                    desk.lock_or_recover()
                        .next_automatic_request(self.backend.now())
                })
                .map_or(next_refresh, |due| due.min(next_refresh));
            let mut state = self.state.lock_or_recover();
            loop {
                if state.stopped {
                    return;
                }
                if state.epoch != epoch || state.requested != requested {
                    break;
                }
                let remaining = until.saturating_duration_since(self.backend.now());
                if remaining.is_zero() {
                    break;
                }
                #[cfg(test)]
                self.backend.waiting(until);
                state = self
                    .changed
                    .wait_timeout(state, remaining)
                    .unwrap_or_else(PoisonError::into_inner)
                    .0;
            }
        }
    }
}

#[cfg(test)]
mod tests;
