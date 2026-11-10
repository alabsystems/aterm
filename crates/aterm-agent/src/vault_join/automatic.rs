// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The node generation's automatic admission worker. It keeps a current snapshot of this
//! member's membership and home ALab LAN (at most `REFRESH_EVERY` old, looked at again at once on
//! a network move or a membership change). A look reads the vault again only where a file the
//! desk is read from moved since the last read settled (`Backend::unmoved`); otherwise it keeps
//! the member and reads again only what no vault file decides: the network and the home LAN.
//! A Mac on that LAN, or one recovering an existing seat from elsewhere, proves itself on the
//! connection (`prove`), as a window's desk lets it in (`aterm_secrets::client::readmit`): being
//! on the LAN is the admission, so a member with no window open lets new Macs in too.

use aterm_secrets::client::{DeskInputs, Paths};
use aterm_secrets::converge::{Last, RunnerRetry};
use aterm_secrets::home_lan::HomeNet;
use aterm_secrets::members::desk::{self, Desk, HomeFrom, Local};
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
    ready: Arc<dyn Fn() + Send + Sync>,
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

/// What one look of the worker found ([`Shared::run`]).
enum Looked {
    /// Nothing the published member was read from moved ([`Backend::unmoved`]): its home LAN as
    /// read again now.
    Unmoved(Option<HomeNet>),
    /// The vault read again ([`Backend::read`]), its member boxed: a look that reads nothing
    /// carries a few bytes.
    Read(Result<Option<(Box<Local>, String)>>),
}

trait Backend: Send + Sync + 'static {
    fn read(&self) -> Result<Option<(Local, String)>>;
    /// Whether the member the last [`Backend::read`] gave stands: that read settled, no file it
    /// was read from moved since, no writer holds the vault, and this Mac is on `network` still.
    /// `Some` holds its home LAN as read again now (what [`Desk::on_home`] takes), the one part
    /// of its policy no vault file decides; `None` reads the vault again.
    fn unmoved(&self, _network: &str) -> Option<Option<HomeNet>> {
        None
    }
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
    /// Public acknowledged membership evidence, before the joining Mac sends possession.
    fn authority(&self, _proving: &desk::Proving<'_>) -> Result<Vec<u8>> {
        Err(Error::new(UNAVAILABLE))
    }
    #[cfg(test)]
    fn waiting(&self, _until: Instant) {}
}

struct Vault {
    paths: Paths,
    /// The last read, where it settled ([`Settled`]).
    settled: Mutex<Option<Settled>>,
    /// The network this Mac is on now: [`aterm_secrets::heard::network_key`], a test's own.
    network: fn() -> Option<String>,
    /// This Mac's home ALab LAN as its place gives it now: [`aterm_secrets::home_lan::home_net`],
    /// a test's own.
    home: fn() -> Option<HomeNet>,
    /// This Mac as a member: [`Local::of`], a test's own.
    local: fn(&Paths) -> Result<Option<Local>>,
}

/// A read of this Mac's membership that settled: a whole member ([`Local::whole`]) read on one
/// network while none of the files it is read from moved ([`aterm_secrets::client::desk_inputs`],
/// the stamps a window's desk lane keeps its member by, taken before the read and equal after
/// it), whose home LAN its place gives or none does ([`HomeFrom`]; a lab vault's, which its vault
/// and route give together, is read only with the vault). Stamps taken before the read alone do
/// not do: a write moves a file's stamp for good (a new inode, a change time nothing sets back),
/// but `state.toml` is stamped by a digest of what it holds
/// ([`aterm_secrets::client::state_standing`]), which can go from A to B during the read and back
/// to A after it, and a member read under B (an apply that failed partway, its roster unread)
/// would then stand under A's stamps until a file moved, which a vault round does not.
struct Settled {
    inputs: DeskInputs,
    network: String,
    home_from: HomeFrom,
}

impl Vault {
    fn new(paths: Paths) -> Self {
        Self {
            paths,
            settled: Mutex::new(None),
            network: aterm_secrets::heard::network_key,
            home: aterm_secrets::home_lan::home_net,
            local: Local::of,
        }
    }
}

impl Backend for Vault {
    fn read(&self) -> Result<Option<(Local, String)>> {
        let mut settled = self.settled.lock_or_recover();
        *settled = None;
        // Taken before the read, and again after it: the read settles only where they are equal
        // ([`Settled`]: the state's digest can go from A to B during the read and back to A).
        let inputs = aterm_secrets::client::desk_inputs(&self.paths);
        // A move while the local vault was read cannot publish a mixed-network snapshot.
        let before = (self.network)();
        let local = (self.local)(&self.paths)?;
        let after = (self.network)();
        let (local, network) = match (local, before, after) {
            (Some(local), Some(before), Some(after)) if before == after => (local, after),
            _ => return Ok(None),
        };
        let home_from = local.home_from();
        if local.whole()
            && home_from != HomeFrom::Vault
            && let Some(inputs) = inputs
            && aterm_secrets::client::desk_inputs(&self.paths).as_ref() == Some(&inputs)
        {
            *settled = Some(Settled {
                inputs,
                network: network.clone(),
                home_from,
            });
        }
        Ok(Some((local, network)))
    }

    fn unmoved(&self, network: &str) -> Option<Option<HomeNet>> {
        let settled = self.settled.lock_or_recover();
        let settled = settled
            .as_ref()
            .filter(|settled| settled.network == network)?;
        // Under a shared observation of the vault's state lock: a writer that holds it meets a
        // fresh read, as a quiet memo's reader does.
        let unmoved = aterm_secrets::client::desk_inputs_quiet(&self.paths)? == settled.inputs
            && (self.network)().as_deref() == Some(network);
        unmoved.then(|| match settled.home_from {
            HomeFrom::Place => (self.home)(),
            HomeFrom::None | HomeFrom::Vault => None,
        })
    }

    fn prove(&self, proving: &desk::Proving<'_>, peer: IpAddr) -> Result<SecretBytes> {
        aterm_secrets::client::prove_at(&self.paths, proving, book(proving, peer))
    }

    fn refuses(&self, proving: &desk::Proving<'_>, peer: IpAddr) -> Option<String> {
        aterm_secrets::client::refuses_before_possession(&self.paths, proving, book(proving, peer))
    }

    fn authority(&self, proving: &desk::Proving<'_>) -> Result<Vec<u8>> {
        aterm_secrets::client::member_authority(&self.paths, proving)
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
    /// membership and network succeeds. `ready` signals an unusable-to-usable publication,
    /// outside the policy lock; healthy renewals do not call it. The generation must close its
    /// notifier before joining this worker or releasing its listener.
    ///
    /// # Errors
    /// The worker thread could not start.
    pub fn start(paths: Paths, ready: Arc<dyn Fn() + Send + Sync>) -> std::io::Result<Self> {
        Self::with_backend(Arc::new(Vault::new(paths)), ready)
    }

    fn with_backend(
        backend: Arc<dyn Backend>,
        ready: Arc<dyn Fn() + Send + Sync>,
    ) -> std::io::Result<Self> {
        let shared = Arc::new(Shared {
            state: Mutex::new(State::default()),
            changed: Condvar::new(),
            backend,
            ready,
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
    /// This invalidates cached policy; the worker looks again before publishing it again, and
    /// reads the vault where a file the desk is read from moved (`Backend::unmoved`).
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
            let mut state = self.shared.state.lock_or_recover();
            let available = self.shared.observe_expiry(&mut state);
            state
                .snapshot
                .as_ref()
                .filter(|_| available)
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
            let mut state = self.shared.state.lock_or_recover();
            self.shared.observe_expiry(&mut state)
                && state.epoch == epoch
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
        let authority = |proving: &desk::Proving<'_>| {
            if proving.lan != home || !current() {
                return Err(Error::new(UNAVAILABLE));
            }
            let witness = backend.authority(proving)?;
            if proving.lan == home && current() {
                Ok(witness)
            } else {
                Err(Error::new(UNAVAILABLE))
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
                authority: Some(&authority),
                fleet: None,
            }),
            &request_policy,
        )?;
        Ok(())
    }
}

impl Shared {
    fn observe_expiry(&self, state: &mut State) -> bool {
        if state.stopped {
            return false;
        }
        let Some(until) = state.valid_until else {
            return false;
        };
        if self.backend.now() < until {
            return true;
        }
        // The same clock reading both rejects this phase and marks it unavailable.
        // An unchanged in-flight read may finish and publish the recovery edge.
        state.valid_until = None;
        self.changed.notify_all();
        false
    }

    fn run(&self) {
        let started = self.backend.now();
        let mut retry = RunnerRetry::default();
        let mut retry_epoch = None;
        // Whether the snapshot is the one the backend's last read gave: a read whose epoch passed
        // before it published leaves the snapshot an older read's, which no look may keep.
        let mut published = false;
        loop {
            let (epoch, kept) = {
                let state = self.state.lock_or_recover();
                if state.stopped {
                    return;
                }
                let kept = state.snapshot.as_ref().filter(|_| published);
                (state.epoch, kept.map(|snapshot| snapshot.network.clone()))
            };
            if retry_epoch != Some(epoch) {
                // Changed membership or place gets a fresh failure budget and an immediate
                // read; an earlier context's unavailable state owes it no delay.
                retry = RunnerRetry::default();
                retry_epoch = Some(epoch);
            }
            retry.started();
            // A look at once after a move or a membership change, or at the next refresh, keeps
            // the published member where nothing it was read from moved, its home LAN read again.
            let current = match kept.as_deref().and_then(|net| self.backend.unmoved(net)) {
                Some(home) => Looked::Unmoved(home),
                None => Looked::Read(
                    self.backend
                        .read()
                        .map(|read| read.map(|(local, network)| (Box::new(local), network))),
                ),
            };
            let now = self.backend.now();
            let now_ms = u64::try_from(now.saturating_duration_since(started).as_millis())
                .unwrap_or(u64::MAX);
            let mut next_refresh = now + REFRESH_EVERY;
            let became_ready;
            {
                let mut state = self.state.lock_or_recover();
                if state.stopped {
                    return;
                }
                if state.epoch != epoch {
                    published &= matches!(current, Looked::Unmoved(_));
                    continue;
                }
                match current {
                    Looked::Unmoved(home) => {
                        became_ready = state.valid_until.is_none();
                        // The snapshot `kept` named: only this worker replaces it.
                        if let Some(snapshot) = state.snapshot.as_ref() {
                            snapshot.desk.lock_or_recover().on_home(home);
                        }
                        state.valid_until = Some(next_refresh);
                        retry.reaped(Last::Ended, now_ms);
                        self.changed.notify_all();
                    }
                    Looked::Read(Ok(Some((local, network)))) => {
                        published = true;
                        became_ready = state.valid_until.is_none();
                        let same = state.snapshot.as_ref().filter(|snapshot| {
                            snapshot.network == network
                                && snapshot.desk.lock_or_recover().local().key() == local.key()
                        });
                        let desk = if let Some(snapshot) = same {
                            snapshot.desk.lock_or_recover().refresh(*local);
                            Arc::clone(&snapshot.desk)
                        } else {
                            let mut desk = Desk::new(*local);
                            desk.on_network(Some(network.clone()));
                            Arc::new(Mutex::new(desk))
                        };
                        // A network or identity change gets a new desk. An ask already reading
                        // the old greeting can finish only on its old desk, never into this one.
                        state.snapshot = Some(Snapshot { network, desk });
                        state.valid_until = Some(next_refresh);
                        retry.reaped(Last::Ended, now_ms);
                        self.changed.notify_all();
                    }
                    Looked::Read(result) => {
                        published = false;
                        became_ready = false;
                        state.valid_until = None;
                        // An invalid read supplies no usable policy to cache for 30 seconds.
                        // The same owner retries it, never waiting longer than a healthy refresh.
                        retry.reaped(Last::Unfinished, now_ms);
                        next_refresh = now
                            + retry
                                .remaining(now_ms)
                                .unwrap_or(REFRESH_EVERY)
                                .min(REFRESH_EVERY);
                        if let Err(error) = result {
                            aterm_log::info!("node: automatic admission state will retry: {error}");
                        }
                    }
                }
            }
            // A hint is not admission authority. Its generation owner decides whether it
            // still exists, and every connection independently rechecks the current policy.
            // Never call that owner while holding the cached policy or a desk lock.
            if became_ready {
                (self.ready)();
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
