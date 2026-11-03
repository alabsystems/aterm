// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The node generation's automatic admission worker. It keeps a current snapshot of this
//! member's membership and home ALab LAN (at most `REFRESH_EVERY` old, read again at once on a
//! network move or a membership change). A Mac on that LAN, or one recovering an existing seat
//! from elsewhere, proves itself on the connection (`prove`), as a window's desk lets it in
//! (`aterm_secrets::client::readmit`): being on the LAN is the admission, so a member with no
//! window open lets new Macs in too.

use aterm_secrets::client::Paths;
use aterm_secrets::members::desk::{self, Desk, Local};
use aterm_secrets::{Error, Result, SecretBytes};
use aterm_types::MutexExt as _;
use std::net::IpAddr;
use std::sync::{Arc, Condvar, Mutex, PoisonError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

const REFRESH_EVERY: Duration = Duration::from_secs(30);
const UNAVAILABLE: &str = "automatic LAN admission is unavailable on this Mac";

/// One node generation's worker: current membership and a bounded desk. Dropping it refuses new
/// requests, wakes the worker and joins it.
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
    /// `LocalLog` retains the same allocation until its policy inputs change.
    membership: Option<Arc<aterm_net::fleet_member::MemberKeys>>,
    /// Invalidates an in-flight state read on a network move or stop.
    epoch: u64,
    stopped: bool,
}

impl State {
    fn invalidate(&mut self) {
        self.valid_until = None;
        self.epoch = self.epoch.wrapping_add(1);
    }
}

struct Snapshot {
    network: String,
    desk: Arc<Mutex<Desk>>,
}

trait Backend: Send + Sync + 'static {
    fn read(&self) -> Result<Option<(Local, String)>>;
    fn now(&self) -> Instant {
        Instant::now()
    }
    /// Decide a Mac that proved itself from `peer` on this connection
    /// (`aterm_secrets::client::prove_at`): its log bundle where this member lets it in.
    ///
    /// # Errors
    /// It is not let in here, or local state failed.
    fn prove(&self, _proving: &desk::Proving<'_>, _peer: IpAddr) -> Result<SecretBytes> {
        Err(Error::new(UNAVAILABLE))
    }
    /// Why this member refuses `proving` from `peer` before the possession challenge
    /// (`aterm_secrets::client::refuses_before_possession`), `None` where it goes on.
    fn refuses(&self, _proving: &desk::Proving<'_>, _peer: IpAddr) -> Option<String> {
        Some(UNAVAILABLE.to_owned())
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

    fn prove(&self, proving: &desk::Proving<'_>, peer: IpAddr) -> Result<SecretBytes> {
        aterm_secrets::client::prove_at(&self.paths, proving, book(proving, peer))
    }

    fn refuses(&self, proving: &desk::Proving<'_>, peer: IpAddr) -> Option<String> {
        aterm_secrets::client::refuses_before_possession(&self.paths, proving, book(proving, peer))
    }
}

/// What this Mac's fleet book says of a Mac proving itself from `peer`, read on the connection's
/// thread, as a window's desk reads it.
fn book(proving: &desk::Proving<'_>, peer: IpAddr) -> aterm_secrets::client::Book {
    let peers = aterm_types::dirs::state_dir()
        .and_then(|state| aterm_net::fleet_member::read_peers(&state).ok());
    super::book_word_read(
        peers.as_deref(),
        &proving.node,
        Some(peer),
        aterm_time::unix_millis().unwrap_or(0),
    )
}

impl AutomaticJoins {
    /// Start the worker for this node generation. It serves asks once its first read of local
    /// membership and network succeeds.
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
            state.invalidate();
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
        state.invalidate();
        self.shared.changed.notify_all();
    }

    /// Wake admission when the node watcher's cached membership verdict changes, including
    /// arrival or loss of the log. `LocalLog::keys` reuses its allocation for unchanged inputs;
    /// a new allocation can reflect changed vault policy even when the node keys are equal.
    /// This invalidates cached policy; the worker reads fresh policy before publishing it again.
    pub fn observe_membership(&self, keys: Option<&Arc<aterm_net::fleet_member::MemberKeys>>) {
        let mut state = self.shared.state.lock_or_recover();
        let same = match (&state.membership, keys) {
            (Some(previous), Some(keys)) => Arc::ptr_eq(previous, keys),
            (None, None) => true,
            _ => false,
        };
        if state.stopped || same {
            return;
        }
        state.membership = keys.cloned();
        state.invalidate();
        self.shared.changed.notify_all();
    }

    /// Let a Mac prove itself (`prove`), using current cached membership and LAN policy:
    /// a `prove` reads and signs the vault's records here, within the
    /// listener's handshake watchdog, as a window's desk does. A plain `ask` is verified and
    /// acknowledged only from the home LAN onto the headless desk, with no vault I/O, where nothing forwards it and no
    /// window shows it: the asking Mac gets in by its own `prove`. It offers no manual approval
    /// phrase.
    ///
    /// # Errors
    /// Current policy is unavailable, a plain ask is outside home, or the signed exchange fails.
    pub fn serve(&self, stream: &mut dyn aterm_net::fleet::Duplex, peer: IpAddr) -> Result<()> {
        let session = {
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
                .map(|snapshot| (Arc::clone(&snapshot.desk), state.epoch))
        };
        let Some((desk, epoch)) = session else {
            let _ = aterm_secrets::members::refuse(stream, UNAVAILABLE);
            return Err(Error::new(UNAVAILABLE));
        };
        let home = desk.lock_or_recover().local().automatically_admits(peer);
        let backend = &self.shared.backend;
        // A greeting reserves no policy. Recheck at each vault phase: the client can wait
        // between them while the node moves, loses membership or stops. A periodic read can
        // also replace the desk or its home policy without a watcher invalidating the epoch.
        // Release both locks before entering the vault; its own state lock guards its writes.
        let current = || {
            let state = self.shared.state.lock_or_recover();
            !state.stopped
                && state.epoch == epoch
                && state.valid_until.is_some_and(|until| backend.now() < until)
                && state
                    .snapshot
                    .as_ref()
                    .is_some_and(|snapshot| Arc::ptr_eq(&snapshot.desk, &desk))
                && desk.lock_or_recover().local().automatically_admits(peer) == home
        };
        let request_policy = || {
            if home && current() {
                Ok(())
            } else {
                Err(Error::new(UNAVAILABLE))
            }
        };
        // The desk captures `lan` while parsing the proof. Periodic refreshes can move its
        // policy away and back without replacing the desk, so bind that captured word too.
        let decide = |proving: &desk::Proving<'_>| {
            if proving.lan == home && current() {
                backend.prove(proving, peer)
            } else {
                Err(Error::new(UNAVAILABLE))
            }
        };
        let refuses = |proving: &desk::Proving<'_>| {
            if proving.lan == home && current() {
                backend.refuses(proving, peer)
            } else {
                Some(UNAVAILABLE.to_owned())
            }
        };
        desk::serve_with_request_policy(
            &desk,
            stream,
            peer,
            &|_| Err(Error::new("manual admission requires a window")),
            Some(desk::Proof {
                decide: &decide,
                refuses: &refuses,
                fleet: None,
            }),
            &request_policy,
        )?;
        Ok(())
    }
}

impl Shared {
    fn run(&self) {
        loop {
            let epoch = {
                let state = self.state.lock_or_recover();
                if state.stopped {
                    return;
                }
                state.epoch
            };
            let current = self.backend.read();
            let next_refresh = self.backend.now() + REFRESH_EVERY;
            {
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
                        state.snapshot = Some(Snapshot { network, desk });
                        state.valid_until = Some(next_refresh);
                        self.changed.notify_all();
                    }
                    result => {
                        state.valid_until = None;
                        if let Err(error) = result {
                            aterm_log::info!("node: automatic admission state will retry: {error}");
                        }
                    }
                }
            }
            let mut state = self.state.lock_or_recover();
            loop {
                if state.stopped {
                    return;
                }
                if state.epoch != epoch {
                    break;
                }
                let remaining = next_refresh.saturating_duration_since(self.backend.now());
                if remaining.is_zero() {
                    break;
                }
                #[cfg(test)]
                self.backend.waiting(next_refresh);
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
