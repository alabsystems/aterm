// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The fleet's inputs as events rather than a clock (KI-1108-2): what wakes the node job's
//! serving watch and its idle loop, and every window's fleet listener.
//!
//! Every judgment the fleet makes between handshakes (a grant revoked, `fleet-off`, a rotated
//! `fleet.key`, a vault taken, a denial) is a read of files. Those readers once ran on a 2 s tick
//! whatever had changed. Here one [`FileWatch`] per process watches those files instead
//! ([`fleet_files`]) and rings the [`Bell`] of each registration ([`Watched`]) whose file really
//! changed; a ring is a hint, never authority: each waiter judges again through the same readers,
//! so a ring that lies costs a pass and nothing else. The vault's `state.toml` rings only where
//! what a judgment reads of it moved ([`Watched::standing`]): the vault's subscriber rewrites it
//! with heartbeat telemetry every 25-60 s while nothing else in it moves.
//!
//! A waiter keeps its own [`Seen`] of its bell: [`Seen::arm`] either names a generation it has
//! not taken or hands it a one-shot [`Stop`] that the next ring sets. It takes a ring
//! ([`Seen::acknowledge`]) before it reads, so a change made during its read leaves the bell
//! ahead of it and the next arm returns at once: nothing between a read and the next wait is
//! lost. Several waiters on one bell each keep their own [`Seen`], and none takes another's ring.
//!
//! Timed work becomes deadlines in the same wait ([`Duties`]): a heartbeat, a facts push, a
//! census refresh are due on their own periods, not checked every 2 s, and duties due within one
//! old tick of each other share a wake. Inputs that have no event
//! keep a bounded clock: [`BACKSTOP`] for every waiter (a watch that died silently, a kernel
//! event missed), the old pace while a state that has no event lasts, and [`ADVERT_LOOK`] for the
//! advert's route term. Where the watch cannot start ([`Inputs::watching`] false) every caller
//! keeps its old tick exactly.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock, Weak};
use std::time::{Duration, Instant};

use aterm_types::MutexExt as _;
use aterm_uds::exitwatch::ExitMoment;
use aterm_uds::file_watch::FileWatch;
use aterm_uds::stop::Stop;

/// The longest any waiter rests with nothing heard while the watch runs: a watch thread that died
/// silently, or a kernel event missed, costs at most this. It is the member heartbeat's own
/// period, so on a serving node it costs no wake the heartbeat does not already make.
pub(crate) const BACKSTOP: Duration = aterm_net::fleet_member::HEARTBEAT_EVERY;

/// How often a serving node looks at its advert again while it advertises, or wants to: the
/// advert's route term and its 24-hour eligibility have no event (nothing shows macOS posts a
/// route event for every route-only change, and a rewrite of `state.toml` that moves only when
/// the vault last heard this Mac rings nothing, [`Watched::standing`]), and a registration the
/// broker could not make is tried again. Availability, not security: the responder authenticates
/// every peer.
pub(crate) const ADVERT_LOOK: Duration = Duration::from_secs(60);

/// One registration's signal: how many rings it heard, and the one-shot stop the next ring sets.
#[derive(Debug, Default)]
pub(crate) struct Bell {
    signal: Mutex<Signal>,
}

#[derive(Debug, Default)]
struct Signal {
    generation: u64,
    wake: Option<Arc<Stop>>,
}

impl Bell {
    /// Something a waiter reads changed (or may have): every waiter armed on this bell wakes, and
    /// one that arms later finds the ring pending. The new generation.
    pub(crate) fn ring(&self) -> u64 {
        let mut signal = self.signal.lock_or_recover();
        signal.generation = signal.generation.wrapping_add(1);
        let generation = signal.generation;
        let wake = signal.wake.take();
        drop(signal);
        if let Some(wake) = wake {
            wake.set();
        }
        generation
    }

    fn generation(&self) -> u64 {
        self.signal.lock_or_recover().generation
    }
}

/// One waiter's view of a [`Bell`]: the last generation it took. A clone is a second waiter that
/// has taken what this one took: a loop hands its view to a worker it starts, so a ring made after
/// the loop's reads is pending for the worker too.
#[derive(Clone, Debug)]
pub(crate) struct Seen {
    bell: Arc<Bell>,
    seen: u64,
}

impl Seen {
    /// A view that has taken every ring so far.
    pub(crate) fn new(bell: &Arc<Bell>) -> Self {
        Self {
            seen: bell.generation(),
            bell: Arc::clone(bell),
        }
    }

    /// Atomically with the generation check: `Err` with a generation this view has not taken
    /// (pending input, not cancellation), or the stop the next ring sets.
    pub(crate) fn arm(&self) -> Result<Arc<Stop>, u64> {
        let mut signal = self.bell.signal.lock_or_recover();
        if signal.generation != self.seen {
            return Err(signal.generation);
        }
        Ok(Arc::clone(
            signal.wake.get_or_insert_with(|| Arc::new(Stop::new())),
        ))
    }

    /// Take `generation`: called before the waiter reads, so a ring during the read stays newer.
    pub(crate) fn acknowledge(&mut self, generation: u64) {
        self.seen = generation;
    }

    /// Take whatever rang since the last take; whether anything had.
    pub(crate) fn take(&mut self) -> bool {
        match self.arm() {
            Err(generation) => {
                self.acknowledge(generation);
                true
            }
            Ok(_) => false,
        }
    }
}

/// What ended a [`wait`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Wake {
    /// A watched input rang (taken already: the caller reads now).
    Inputs,
    /// The network moved ([`crate::network_moved`]); nothing else is due for that.
    Moved(crate::network_moved::Woke),
    /// The deadline came.
    Due,
    /// One of the caller's stops is set.
    Stopped,
}

/// Wait for a ring of `seen`'s bell, a move `network` hears, one of `stops`, or `deadline`,
/// whichever is first. A ring is taken before this returns ([`Seen::acknowledge`]), so the
/// caller's read that follows sees everything up to it, and a later ring is pending at the next
/// wait.
///
/// # Errors
/// The network watch's native wait failed; the caller keeps its timed fallback.
pub(crate) fn wait(
    seen: &mut Seen,
    network: &mut crate::network_moved::Watch,
    stops: &[&Stop],
    deadline: Instant,
) -> std::io::Result<Wake> {
    let stopped = |stops: &[&Stop]| stops.iter().any(|stop| stop.is_set());
    if stopped(stops) {
        return Ok(Wake::Stopped);
    }
    let rung = match seen.arm() {
        Ok(rung) => rung,
        Err(generation) => {
            seen.acknowledge(generation);
            return Ok(Wake::Inputs);
        }
    };
    let mut all: Vec<&Stop> = stops.to_vec();
    all.push(&rung);
    match network.wait_until(&all, Some(deadline))? {
        // A stop that is not the caller's is the ring's (the converge lane's reading of a
        // `None` with its real stops unset).
        None if stopped(stops) => Ok(Wake::Stopped),
        None => {
            seen.take();
            Ok(Wake::Inputs)
        }
        Some(crate::network_moved::Woke::Timer) => Ok(Wake::Due),
        Some(woke) => Ok(Wake::Moved(woke)),
    }
}

/// Wait for a ring of `seen`'s bell, `stop`, or `deadline`, with no network watch: [`wait`] for
/// a loop that does nothing on a move. Whether a ring ended it (taken already).
pub(crate) fn wait_rung(seen: &mut Seen, stop: &Stop, deadline: Instant) -> bool {
    if stop.is_set() {
        return false;
    }
    let rung = match seen.arm() {
        Ok(rung) => rung,
        Err(generation) => {
            seen.acknowledge(generation);
            return true;
        }
    };
    let bell = match stop.bell() {
        Ok(bell) => bell,
        Err(_) => {
            // No descriptor for the stop's bell: wait on the ring alone, which a stop does not
            // set, at most to the deadline.
            rung.sleep(deadline.saturating_duration_since(Instant::now()));
            return seen.take();
        }
    };
    if rung.wait_ready(&[bell], Some(deadline)).is_err() {
        stop.sleep(deadline.saturating_duration_since(Instant::now()));
    }
    !stop.is_set() && seen.take()
}

/// Where a watched key's tell goes: the bell of the registration that holds it, rung at each
/// tell of a file's, only where the entries it keeps moved for a folder's ([`Entries`]), and only
/// where its standing moved for the vault's `state.toml` ([`Standing`]).
#[derive(Clone)]
enum Route {
    Bell(Weak<Bell>),
    Entries(Arc<Entries>),
    Standing(Arc<Standing>),
}

impl Route {
    /// The bell this tell rings, if any: a folder's only where its kept entries moved, the
    /// vault's state only where its standing did.
    fn rung(&self) -> Option<Arc<Bell>> {
        match self {
            Self::Bell(bell) => bell.upgrade(),
            Self::Entries(entries) => entries.moved().then(|| entries.bell.upgrade()).flatten(),
            Self::Standing(standing) => standing.moved().then(|| standing.bell.upgrade()).flatten(),
        }
    }
}

/// The vault's applied state, `state.toml`, whose standing
/// ([`aterm_secrets::client::state_standing`]) is all a fleet judgment reads of it.
fn state_file(vault: &aterm_secrets::client::Paths) -> PathBuf {
    vault.dir.join("state.toml")
}

/// The vault's `state.toml` watched for its standing alone ([`Watched::standing`]): what it holds
/// but when the vault last heard this Mac and the receipts of its rounds
/// ([`aterm_secrets::client::state_standing`], one `lstat` while the file's stamp stands). The
/// vault's subscriber rewrites those fields every 25-60 s while nothing else in the file moves,
/// and each rewrite ran a full pass of the node job and of every window's listener for nothing.
/// No judgment their passes make reads them: the vault's memos, its members desk
/// (`aterm_secrets::client::desk_inputs`) and the membership log
/// (`aterm_agent::fleet_log::LocalLog`) keep what they read by this same standing, and the one
/// reader of `heard` a pass asks, the node's `_alab._tcp` advert, keeps its own clock
/// ([`ADVERT_LOOK`]).
struct Standing {
    bell: Weak<Bell>,
    vault: aterm_secrets::client::Paths,
    /// What the last ring found (`None`: not told yet).
    last: Mutex<Option<Told>>,
}

/// What a tell of the vault's `state.toml` found.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Told {
    /// Its standing; `None` with no file.
    Read(Option<[u8; 32]>),
    /// It did not read (a write met mid-way, a file refused).
    Unread,
}

impl Standing {
    /// Whether this tell rings: where the standing moved since the last ring, the first tell,
    /// and a tell that finds the file made or removed. One that cannot read it rings (fail open:
    /// a ring is a hint, a lost ring is not), and so does the next whatever it reads, since the
    /// waiter's pass read what this one could not.
    fn moved(&self) -> bool {
        let now = match aterm_secrets::client::state_standing(&self.vault) {
            Ok(standing) => Told::Read(standing),
            Err(_) => Told::Unread,
        };
        let mut last = self.last.lock_or_recover();
        if now != Told::Unread && last.as_ref() == Some(&now) {
            return false;
        }
        *last = Some(now);
        true
    }
}

/// A folder watched for some of its entries alone ([`Watched::entries`]): the node's relay
/// folder, which on macOS is the state folder itself, where every aterm process writes its census,
/// vitals, journals and control sockets beside the windows' relays.
struct Entries {
    bell: Weak<Bell>,
    dir: PathBuf,
    keep: fn(&str) -> bool,
    /// The kept entries at the last tell, by name and inode (`None`: not told yet; `Some(None)`:
    /// the folder did not list).
    last: Mutex<Option<Option<Vec<(String, u64)>>>>,
}

impl Entries {
    /// Whether the kept entries moved since the last tell: made, removed or replaced (a new inode
    /// under the same name). The first tell has, as a first look rings whatever is there.
    fn moved(&self) -> bool {
        let now = self.listing();
        let mut last = self.last.lock_or_recover();
        if last.as_ref() == Some(&now) {
            return false;
        }
        *last = Some(now);
        true
    }

    /// The kept entries now, sorted; `None` where the folder does not list.
    fn listing(&self) -> Option<Vec<(String, u64)>> {
        use std::os::unix::fs::DirEntryExt as _;
        let mut kept: Vec<(String, u64)> = std::fs::read_dir(&self.dir)
            .ok()?
            .flatten()
            .filter_map(|entry| {
                let name = entry.file_name().into_string().ok()?;
                (self.keep)(&name).then(|| (name, entry.ino()))
            })
            .collect();
        kept.sort_unstable();
        Some(kept)
    }
}

/// The routes from a watched key to where its tell goes.
type Routes = Arc<Mutex<HashMap<u64, Route>>>;

/// One process's file watch over the fleet's inputs, shared by every registration in it.
pub(crate) struct Inputs {
    watch: Option<FileWatch>,
    routes: Routes,
    next_key: AtomicU64,
    /// How many times the watch told each key: a test waits for a job's first looks to be told
    /// ([`first_looks_told`]) before it counts the passes its own changes make, and for a tell
    /// it caused before it says that tell rang nothing.
    #[cfg(test)]
    told: Arc<Mutex<HashMap<u64, usize>>>,
}

impl Inputs {
    /// Start over the watch `make` builds, told each changed key; one that fails leaves a set
    /// that watches nothing ([`Self::watching`] false), and its error.
    pub(crate) fn start(
        make: impl FnOnce(Box<dyn FnMut(u64) + Send>) -> std::io::Result<FileWatch>,
    ) -> (Self, Option<std::io::Error>) {
        let routes: Routes = Arc::default();
        let to = Arc::clone(&routes);
        #[cfg(test)]
        let told: Arc<Mutex<HashMap<u64, usize>>> = Arc::default();
        #[cfg(test)]
        let noted = Arc::clone(&told);
        let notify = Box::new(move |key: u64| {
            let route = to.lock_or_recover().get(&key).cloned();
            if let Some(bell) = route.as_ref().and_then(Route::rung) {
                bell.ring();
            }
            // Noted once its ring is made: a test that finds every first look told takes its
            // view after each of their rings.
            #[cfg(test)]
            {
                *noted.lock_or_recover().entry(key).or_default() += 1;
            }
        });
        let (watch, error) = match make(notify) {
            Ok(watch) => (Some(watch), None),
            Err(error) => (None, Some(error)),
        };
        (
            Self {
                watch,
                routes,
                next_key: AtomicU64::new(1),
                #[cfg(test)]
                told,
            },
            error,
        )
    }

    /// Whether the watch runs. While it does not, every caller keeps its old tick exactly.
    pub(crate) fn watching(&self) -> bool {
        self.watch.is_some()
    }

    /// A registration of its own: its own bell, rung by its own files alone.
    pub(crate) fn register(self: &Arc<Self>) -> Watched {
        Watched {
            inputs: Arc::clone(self),
            bell: Arc::default(),
            keys: Mutex::default(),
            #[cfg(test)]
            named: Mutex::default(),
        }
    }

    fn watch(&self, route: Route, path: PathBuf, directory: bool) -> Option<u64> {
        let watch = self.watch.as_ref()?;
        let key = self.next_key.fetch_add(1, Ordering::Relaxed);
        self.routes.lock_or_recover().insert(key, route);
        if directory {
            watch.watch_directory(key, path);
        } else {
            watch.watch(key, path);
        }
        Some(key)
    }

    fn unwatch(&self, key: u64) {
        self.routes.lock_or_recover().remove(&key);
        if let Some(watch) = &self.watch {
            watch.unwatch(key);
        }
    }
}

/// This process's inputs, started on first use over the platform's file events. A watch that
/// cannot start is said once in aterm.log, and its callers keep their old tick.
pub(crate) fn process() -> &'static Arc<Inputs> {
    static PROCESS: OnceLock<Arc<Inputs>> = OnceLock::new();
    PROCESS.get_or_init(|| {
        let (inputs, error) =
            Inputs::start(|notify| FileWatch::platform("aterm-fleet-inputs", notify));
        if let Some(error) = error {
            aterm_log::warn!(
                "fleet: the file watch over the fleet's inputs did not start ({error}); the fleet \
                 keeps its 2 s checks"
            );
        }
        Arc::new(inputs)
    })
}

/// One waiter's files and the bell they ring. Dropped, it watches none of them.
pub(crate) struct Watched {
    inputs: Arc<Inputs>,
    bell: Arc<Bell>,
    keys: Mutex<Vec<u64>>,
    /// Every path asked for, watched or not: a test holds a production composition to the files
    /// it must name ([`WindowInputs::named`]) with no file watched.
    #[cfg(test)]
    named: Mutex<Vec<PathBuf>>,
}

impl Watched {
    /// The bell these files ring.
    pub(crate) fn bell(&self) -> &Arc<Bell> {
        &self.bell
    }

    /// Watch the file at `path` for as long as this registration lasts. Its first look rings,
    /// so a change between the caller's read and this call is not lost.
    pub(crate) fn file(&self, path: PathBuf) {
        #[cfg(test)]
        self.named.lock_or_recover().push(path.clone());
        let route = Route::Bell(Arc::downgrade(&self.bell));
        if let Some(key) = self.inputs.watch(route, path, false) {
            self.keys.lock_or_recover().push(key);
        }
    }

    /// Watch the folder at `path` for the entries whose names `keep` takes, for as long as this
    /// registration lasts: a tell of the folder rings only where those entries (by name and
    /// inode) were made, removed or replaced since its last, and at its first look. A folder every
    /// process writes into then wakes the waiter for its own entries, not for each write beside
    /// them.
    pub(crate) fn entries(&self, path: PathBuf, keep: fn(&str) -> bool) {
        #[cfg(test)]
        self.named.lock_or_recover().push(path.clone());
        let route = Route::Entries(Arc::new(Entries {
            bell: Arc::downgrade(&self.bell),
            dir: path.clone(),
            keep,
            last: Mutex::default(),
        }));
        if let Some(key) = self.inputs.watch(route, path, true) {
            self.keys.lock_or_recover().push(key);
        }
    }

    /// Watch `vault`'s `state.toml` for its standing alone, for as long as this registration
    /// lasts: a tell rings only where the standing moved since the last ring ([`Standing`]), so
    /// a rewrite that moves only when the vault last heard this Mac or its rounds' receipts wakes
    /// nothing. Its first look rings, as a file's does.
    fn standing(&self, vault: &aterm_secrets::client::Paths) {
        let path = state_file(vault);
        #[cfg(test)]
        self.named.lock_or_recover().push(path.clone());
        let route = Route::Standing(Arc::new(Standing {
            bell: Arc::downgrade(&self.bell),
            vault: vault.clone(),
            last: Mutex::default(),
        }));
        if let Some(key) = self.inputs.watch(route, path, false) {
            self.keys.lock_or_recover().push(key);
        }
    }

    /// Watch each of `files` once: `vault`'s `state.toml`, where `vault` names one, by its
    /// standing ([`Self::standing`]), and every other file by its change ([`Self::file`]).
    fn files(&self, mut files: Vec<PathBuf>, vault: Option<&aterm_secrets::client::Paths>) {
        files.sort();
        files.dedup();
        let state = vault.map(|vault| (state_file(vault), vault));
        for file in files {
            match &state {
                Some((at, vault)) if *at == file => self.standing(vault),
                _ => self.file(file),
            }
        }
    }

    /// Whether the watch told each of these files at least once: their first looks, which ring
    /// whatever the files hold, are behind the waiter.
    #[cfg(test)]
    fn all_told(&self) -> bool {
        let told = self.inputs.told.lock_or_recover();
        self.keys
            .lock_or_recover()
            .iter()
            .all(|key| told.contains_key(key))
    }

    /// How many tells the watch made of these files so far.
    #[cfg(test)]
    fn tells(&self) -> usize {
        let told = self.inputs.told.lock_or_recover();
        self.keys
            .lock_or_recover()
            .iter()
            .filter_map(|key| told.get(key))
            .sum()
    }
}

impl Drop for Watched {
    fn drop(&mut self) {
        for key in self.keys.lock_or_recover().drain(..) {
            self.inputs.unwatch(key);
        }
    }
}

/// The files every fleet listener judges by, in `state_dir`, the `aterm.toml` at `config_path`
/// and the vault at `vault`: the fleet's key, `fleet-off`, the node identity and its key, the
/// vault tenure record, the book and the issuer's copy of it, `aterm.toml` (grants, listen,
/// `[swarm] enabled`), and every vault file a judgment re-reads
/// ([`aterm_secrets::client::Paths::fleet_judgment_files`]). Those name the records' entry and
/// both of the places its current data can be (the entry itself, or the layout's data file
/// inside it) whatever the layout now, so a layout move is heard with nothing re-resolved.
pub(crate) fn fleet_files(
    state_dir: &Path,
    config_path: Option<&Path>,
    vault: Option<&aterm_secrets::client::Paths>,
) -> Vec<PathBuf> {
    use aterm_net::fleet_member::{
        ISSUER_BOOK_FILE, NODE_FILE, NODE_KEY_FILE, OFF_FILE, PEERS_FILE, VAULT_FILE,
    };
    let mut files: Vec<PathBuf> = [
        aterm_net::fleet::KEY_FILE,
        OFF_FILE,
        NODE_FILE,
        NODE_KEY_FILE,
        VAULT_FILE,
        PEERS_FILE,
        ISSUER_BOOK_FILE,
    ]
    .into_iter()
    .map(|name| state_dir.join(name))
    .collect();
    files.extend(config_path.map(Path::to_path_buf));
    if let Some(vault) = vault {
        files.extend(vault.fleet_judgment_files());
    }
    files
}

/// The node job's inputs: [`fleet_files`], its windows' relays in their folder, and each live
/// window's exit (a window that crashed leaves its relay socket behind, so the folder alone does
/// not hear it).
pub(crate) struct NodeInputs {
    watched: Watched,
    exits: Mutex<HashMap<u32, Option<ExitMoment<()>>>>,
}

/// An exit's stamp, which nothing reads: the exit's ring is what matters.
fn no_stamp() {}

/// This process's node inputs ([`NodeInputs`]), by their job's state folder, whose bells
/// [`wake_node_maintenance`] rings.
static NODES: Mutex<Vec<(PathBuf, Weak<NodeInputs>)>> = Mutex::new(Vec::new());

/// Ring every node job's inputs in this process: its local upkeep
/// ([`super::node_serve::NodeMaintenance`]) is owed now though no deadline it named came (a
/// worker it keeps ended). A ring only brings the job's next pass sooner.
pub fn wake_node_maintenance() {
    ring_nodes(|_| true);
}

/// Ring the inputs of the node job on `state_dir` alone: a test's ring that changes nothing, which
/// no other test's job hears.
#[cfg(test)]
pub(crate) fn ring_node(state_dir: &Path) {
    ring_nodes(|at| at == state_dir);
}

/// Ring the inputs of each node job in this process whose state folder `which` takes, letting
/// go of those dropped.
fn ring_nodes(which: impl Fn(&Path) -> bool) {
    for node in nodes(which) {
        node.bell().ring();
    }
}

/// Each node job's inputs in this process whose state folder `which` takes, letting go of those
/// dropped.
fn nodes(which: impl Fn(&Path) -> bool) -> Vec<Arc<NodeInputs>> {
    let mut held = NODES.lock_or_recover();
    held.retain(|(_, node)| node.strong_count() > 0);
    held.iter()
        .filter(|(at, _)| which(at))
        .filter_map(|(_, node)| node.upgrade())
        .collect()
}

/// Whether the watch told every file of the node job on `state_dir` at least once: a test that
/// counts the passes its own change makes waits for this first, since each first look rings.
#[cfg(test)]
pub(crate) fn first_looks_told(state_dir: &Path) -> bool {
    let jobs = nodes(|at| at == state_dir);
    !jobs.is_empty() && jobs.iter().all(|node| node.watched.all_told())
}

impl NodeInputs {
    /// Watch the node's inputs on `inputs`: [`fleet_files`], `also` (the files its membership
    /// log reads, `aterm_agent::fleet_log::LocalLog::read_files`), and its windows' relays: the
    /// entries of `relay_dir` whose names `relay` takes ([`Watched::entries`]). On macOS that
    /// folder is the state folder, so a write beside the relays wakes nothing. The vault's
    /// `state.toml` rings only where its standing moved ([`Watched::standing`]).
    pub(crate) fn start(
        inputs: &Arc<Inputs>,
        state_dir: &Path,
        config_path: Option<&Path>,
        vault: Option<&aterm_secrets::client::Paths>,
        also: Vec<PathBuf>,
        (relay_dir, relay): (&Path, fn(&str) -> bool),
    ) -> Arc<Self> {
        let watched = inputs.register();
        let mut files = fleet_files(state_dir, config_path, vault);
        files.extend(also);
        watched.files(files, vault);
        watched.entries(relay_dir.to_path_buf(), relay);
        let node = Arc::new(Self {
            watched,
            exits: Mutex::default(),
        });
        NODES
            .lock_or_recover()
            .push((state_dir.to_path_buf(), Arc::downgrade(&node)));
        node
    }

    /// The bell the node's inputs ring.
    pub(crate) fn bell(&self) -> &Arc<Bell> {
        self.watched.bell()
    }

    /// Whether the watch runs ([`Inputs::watching`]).
    pub(crate) fn watching(&self) -> bool {
        self.watched.inputs.watching()
    }

    /// Watch the exit of each window whose relay is live now (`relays`, by pid): a window that
    /// crashed leaves its relay socket behind, so the folder alone does not hear it go. A window
    /// whose exit cannot be watched (it already ended) rings once, so the pass that follows lists
    /// the folder again; one that is no longer live is forgotten.
    pub(crate) fn refresh(&self, relays: impl IntoIterator<Item = u32>) {
        if !self.watching() {
            return;
        }
        let live: Vec<u32> = relays.into_iter().collect();
        let mut exits = self.exits.lock_or_recover();
        exits.retain(|pid, _| live.contains(pid));
        for pid in live {
            if exits.contains_key(&pid) {
                continue;
            }
            let bell = Arc::downgrade(self.bell());
            let watch = ExitMoment::watch_then(pid, no_stamp, move || {
                if let Some(bell) = bell.upgrade() {
                    bell.ring();
                }
            });
            if watch.is_none() {
                self.bell().ring();
            }
            exits.insert(pid, watch);
        }
    }
}

/// A window process's fleet inputs (KI-1108-2): [`fleet_files`] of its state, `aterm.toml` and
/// vault, the files its membership log reads, and `node-port.lock`, which the node job's take of
/// the port writes (`aterm_agent::node_service::NodeJobLock::try_acquire_port`), so a serving
/// window hears the node take the port, where a flock alone moves nothing a file watch sees. Its
/// bell rings too for each change of the window's `[fleet]` ([`ring_window`]) and for the port
/// waiters its rests arm ([`ring_on_release`]).
pub(crate) struct WindowInputs {
    watched: Watched,
}

impl WindowInputs {
    /// Watch a window's inputs on `inputs`: [`fleet_files`] of `state_dir` (none where no state
    /// folder resolves, so no fleet is served), `config_path` and `vault`, its `node-port.lock`,
    /// and `also` (the files its membership log reads,
    /// `aterm_agent::fleet_log::LocalLog::read_files`). The vault's `state.toml` rings only where
    /// its standing moved ([`Watched::standing`]).
    pub(crate) fn start(
        inputs: &Arc<Inputs>,
        state_dir: Option<&Path>,
        config_path: Option<&Path>,
        vault: Option<&aterm_secrets::client::Paths>,
        also: Vec<PathBuf>,
    ) -> Arc<Self> {
        let watched = inputs.register();
        let mut files = match state_dir {
            Some(state) => {
                let mut files = fleet_files(state, config_path, vault);
                files.push(state.join(aterm_agent::node_service::NODE_PORT_LOCK));
                files
            }
            None => config_path
                .map(Path::to_path_buf)
                .into_iter()
                .chain(
                    vault
                        .map(aterm_secrets::client::Paths::fleet_judgment_files)
                        .unwrap_or_default(),
                )
                .collect(),
        };
        files.extend(also);
        watched.files(files, vault);
        Arc::new(Self { watched })
    }

    /// The bell the window's inputs ring.
    pub(crate) fn bell(&self) -> &Arc<Bell> {
        self.watched.bell()
    }

    /// Whether the watch runs ([`Inputs::watching`]).
    pub(crate) fn watching(&self) -> bool {
        self.watched.inputs.watching()
    }

    /// Whether the watch told each of the window's files at least once ([`first_looks_told`]).
    #[cfg(test)]
    pub(crate) fn first_looks_told(&self) -> bool {
        self.watched.all_told()
    }

    /// Every path the window's inputs asked to watch, watched or not.
    #[cfg(test)]
    pub(crate) fn named(&self) -> Vec<PathBuf> {
        self.watched.named.lock_or_recover().clone()
    }
}

/// This process's window inputs ([`window`]).
static WINDOW: OnceLock<Arc<WindowInputs>> = OnceLock::new();

/// This window process's fleet inputs, started at the first ask over [`process`]'s watch, `start`
/// naming what they watch.
pub(crate) fn window(
    start: impl FnOnce(&Arc<Inputs>) -> Arc<WindowInputs>,
) -> &'static Arc<WindowInputs> {
    WINDOW.get_or_init(|| start(process()))
}

/// The window's `[fleet]` changed (a reload in place, a move to a new generation): ring this
/// process's window inputs, where they started, so every wait of its listener judges again at
/// once. Inputs not started yet have no waiter to ring.
pub(crate) fn ring_window() {
    ring_window_where(|_| true);
}

/// [`ring_window`] where the window's inputs watch their files: for an input of the listener's
/// that its old tick met at its next look (a remote hold, `super::remote_hold_moved`), which off
/// the watch wakes nothing, as it did.
pub(crate) fn ring_window_watching() {
    ring_window_where(WindowInputs::watching);
}

/// Ring this process's window inputs where they started and `which` takes them.
fn ring_window_where(which: impl Fn(&WindowInputs) -> bool) {
    #[cfg(test)]
    WINDOW_RINGS.with(|rings| rings.set(rings.get() + 1));
    if let Some(window) = WINDOW.get().filter(|window| which(window)) {
        window.bell().ring();
    }
}

#[cfg(test)]
thread_local! {
    /// How many times this thread asked [`ring_window`] or [`ring_window_watching`]: a test of
    /// what rings the window's listener counts its own thread's asks, which no other test's ring
    /// moves.
    static WINDOW_RINGS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// How many times this thread asked [`ring_window`] or [`ring_window_watching`] so far.
#[cfg(test)]
pub(crate) fn window_rings_on_this_thread() -> usize {
    WINDOW_RINGS.with(std::cell::Cell::get)
}

/// When each of a waiter's timed duties is next due (`None`: not on a clock now).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Duties {
    /// The member heartbeat ([`super::HeartbeatClock`]'s next round).
    pub(crate) heartbeat: Option<Instant>,
    /// The facts push, while a deny list is read.
    pub(crate) facts: Option<Instant>,
    /// The home's checkouts read, or while a window answers surveys the survey that clocks the
    /// window's read (`super::node_serve::read_checkouts_when_due`).
    pub(crate) checkouts: Option<Instant>,
    /// The local census refresh.
    pub(crate) census: Option<Instant>,
    /// The job's local upkeep ([`super::node_serve::NodeMaintenance`]).
    pub(crate) maintenance: Option<Instant>,
    /// The advert's look, while it advertises or wants to ([`ADVERT_LOOK`]).
    pub(crate) advert: Option<Instant>,
    /// The fleet routes' next refresh, while the job browses and reads a members log
    /// (`aterm_agent::node_service::DISCOVERY_PERIOD` after the last).
    pub(crate) routes: Option<Instant>,
    /// The idle loop's next look where what it waits for has no event of its own: the broker's
    /// ([`LAN_LOOK`]) or a foreign holder of the port's ([`FOREIGN_MOST`]).
    pub(crate) retry: Option<Instant>,
    /// The old pace, while a state with no event lasts (a boot trial, an owed look).
    pub(crate) paced: Option<Instant>,
    /// An `aterm.toml` that did not read, judged again once a write in progress settles
    /// ([`Unreadable`]).
    pub(crate) settle: Option<Instant>,
    /// [`BACKSTOP`] after the last pass.
    pub(crate) backstop: Option<Instant>,
}

impl Duties {
    /// Every deadline, named field by field so a duty added is one the wake keeps.
    fn each(&self) -> [Option<Instant>; 11] {
        let Self {
            heartbeat,
            facts,
            checkouts,
            census,
            maintenance,
            advert,
            routes,
            retry,
            paced,
            settle,
            backstop,
        } = *self;
        [
            heartbeat,
            facts,
            checkouts,
            census,
            maintenance,
            advert,
            routes,
            retry,
            paced,
            settle,
            backstop,
        ]
    }

    /// When to wake for them: the latest deadline within `reach` (the waiter's old tick) of the
    /// earliest, so every duty due that close is due at the one wake, at most one old tick late
    /// as the tick left it; but never past the old pace (`paced`) or a settling `aterm.toml`
    /// (`settle`), which are kept exactly: a later duty is what moves to them, not they to it.
    /// `None` with none on a clock.
    pub(crate) fn earliest(&self, reach: Duration) -> Option<Instant> {
        let first = self.each().into_iter().flatten().min()?;
        let reach = first.checked_add(reach).unwrap_or(first);
        let shared = self
            .each()
            .into_iter()
            .flatten()
            .filter(|at| *at <= reach)
            .max()?;
        let exact = self.paced.into_iter().chain(self.settle).min();
        Some(exact.map_or(shared, |exact| shared.min(exact)))
    }
}

/// When the member heartbeat is next due after a pass: the clock's own answer (`next`,
/// [`super::HeartbeatClock::next_due`]) but for a round the pass owed (`owed`, due when the pass
/// asked) and could not start (no route to register from, no node identity yet), which the clock
/// still names at its past instant: every wake would meet that, so it is asked again `every`
/// after `now`. What it waits on wakes the pass that asks sooner: a move (the route back) through
/// the network watch, the identity's file through its ring.
pub(crate) fn heartbeat_after(
    next: Option<Instant>,
    owed: bool,
    now: Instant,
    every: Duration,
) -> Option<Instant> {
    match next {
        Some(at) if owed && at <= now => Some(now + every),
        next => next,
    }
}

/// Whether a deny-list read (`observed`) settled: one that met a vault writer
/// (`WriterBusy`), the vault configuration moving under it, or an error is read again at the
/// next call, with no watched file moving to bring that call.
pub(crate) fn denial_settled(
    observed: &aterm_secrets::Result<aterm_secrets::client::SwarmDenialSnapshot>,
) -> bool {
    use aterm_secrets::client::{SwarmDenialSnapshot, SwarmDenialUnavailable};
    !matches!(
        observed,
        Err(_)
            | Ok(SwarmDenialSnapshot::Unavailable(
                SwarmDenialUnavailable::WriterBusy | SwarmDenialUnavailable::ConfigurationChanged
            ))
    )
}

/// What a [`Pace::wait`] woke for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Pass {
    /// The network move heard, if one was.
    pub(crate) moved: Option<crate::network_moved::Woke>,
    /// Whether a pass runs: an input rang or a duty came. A move alone refreshes admission.
    pub(crate) run: bool,
}

/// The longest a pace or a rest waits: the same finite cap as the stop-only sleep the tick once
/// was.
const LONGEST: Duration = Duration::from_secs(86_400 * 365);

/// A serving watch's pace (KI-1108-2), the node job's ([`Pace::node`]) and a window's
/// ([`Pace::window`]): it wakes for a ring of its inputs, a network move, a stop, or the earliest
/// of its duties ([`Duties`], with its backstop after each pass), guarded by a [`Pacer`]. Where
/// the file watch did not start it keeps the old tick exactly: a pass every `tick`, and between
/// them only what woke it before the watch: a move, which refreshes the node's admission, or a
/// change of the window's `[fleet]` ([`ring_window`]), which runs the window's pass at once.
pub(crate) struct Pace {
    seen: Seen,
    watching: bool,
    /// A window's: off the watch its bell rings only for a change of its `[fleet]`, which its old
    /// wait took at once, and that wait heard no move.
    desk: bool,
    tick: Duration,
    backstop: Duration,
    duties: Duties,
    pacer: Pacer,
    next_tick: Instant,
    wait_failed: bool,
}

impl Pace {
    /// A node job's pace over `node`'s inputs from `seen` (the view the job took before its
    /// reads, so a ring since is pending), whose first pass comes at the old tick `tick` unless an
    /// input rings first, as the tick's first pass did, and which rests at most [`BACKSTOP`] with
    /// nothing heard.
    pub(crate) fn node(node: &NodeInputs, seen: Seen, tick: Duration) -> Self {
        Self::new(node.watching(), false, seen, tick, BACKSTOP)
    }

    /// A window's serving pace over `window`'s inputs, as [`Self::node`], resting at most
    /// `backstop` with nothing heard.
    pub(crate) fn window(
        window: &WindowInputs,
        seen: Seen,
        tick: Duration,
        backstop: Duration,
    ) -> Self {
        Self::new(window.watching(), true, seen, tick, backstop)
    }

    fn new(watching: bool, desk: bool, seen: Seen, tick: Duration, backstop: Duration) -> Self {
        let tick = tick.min(LONGEST);
        let first = Instant::now() + tick;
        Self {
            seen,
            watching,
            desk,
            tick,
            backstop: backstop.min(LONGEST),
            duties: Duties {
                paced: Some(first),
                ..Duties::default()
            },
            pacer: Pacer::default(),
            next_tick: first,
            wait_failed: false,
        }
    }

    /// Whether the file watch runs ([`Inputs::watching`]).
    pub(crate) fn watching(&self) -> bool {
        self.watching
    }

    /// Wait for the next thing to do: `None` once one of `stops` is set. Where the network watch
    /// cannot wait (said once), it sleeps on the first of `stops` to its deadline, at most the
    /// old tick, and runs a pass then, as the tick did.
    pub(crate) fn wait(
        &mut self,
        network: &mut crate::network_moved::Watch,
        stops: &[&Stop],
    ) -> Option<Pass> {
        let now = Instant::now();
        let deadline = if self.watching {
            let at = self
                .duties
                .earliest(self.tick)
                .unwrap_or(now + self.backstop);
            self.pacer.next(now, at, self.tick)
        } else {
            self.next_tick
        };
        let woke = if self.watching {
            wait(&mut self.seen, network, stops, deadline)
        } else if self.desk {
            Ok(desk_wait(&mut self.seen, stops, deadline))
        } else {
            network
                .wait_until(stops, Some(deadline))
                .map(|woke| match woke {
                    None => Wake::Stopped,
                    Some(crate::network_moved::Woke::Timer) => Wake::Due,
                    Some(moved) => Wake::Moved(moved),
                })
        };
        let run = match woke {
            Ok(Wake::Stopped) => return None,
            Ok(Wake::Moved(moved)) => {
                self.wait_failed = false;
                return Some(Pass {
                    moved: Some(moved),
                    run: false,
                });
            }
            Ok(Wake::Inputs | Wake::Due) => {
                self.wait_failed = false;
                Pass {
                    moved: None,
                    run: true,
                }
            }
            Err(error) => {
                if !std::mem::replace(&mut self.wait_failed, true) {
                    aterm_log::warn!(
                        "fleet: the network wait failed: {error}; keeping its timed checks"
                    );
                }
                let until = deadline.min(Instant::now() + self.tick);
                let left = until.saturating_duration_since(Instant::now());
                if stops.first().is_some_and(|stop| stop.sleep(left))
                    || stops.iter().any(|stop| stop.is_set())
                {
                    return None;
                }
                self.seen.take();
                Pass {
                    moved: network.take_at(Instant::now()),
                    run: true,
                }
            }
        };
        Some(run)
    }

    /// A move made the heartbeat's next round due at `at` (`None`: one is in flight, whose end
    /// rings).
    pub(crate) fn heartbeat(&mut self, at: Option<Instant>) {
        self.duties.heartbeat = at;
    }

    /// A pass ran: `duties` say when each duty is next due, and `paced` whether work no file
    /// event tells lasts, which keeps the old tick while it does.
    pub(crate) fn passed(&mut self, duties: Duties, paced: bool) {
        let now = Instant::now();
        self.next_tick = now + self.tick;
        self.duties = Duties {
            paced: paced.then_some(self.next_tick),
            backstop: Some(now + self.backstop),
            ..duties
        };
    }
}

/// A window's wait off the file watch, as before it: a change of its `[fleet]` (a ring of
/// `seen`'s bell), its generation's stop (the one of `stops`), or `deadline`, and no move.
fn desk_wait(seen: &mut Seen, stops: &[&Stop], deadline: Instant) -> Wake {
    let unset = Stop::new();
    let stop = stops.first().copied().unwrap_or(&unset);
    if wait_rung(seen, stop, deadline) {
        Wake::Inputs
    } else if stops.iter().any(|stop| stop.is_set()) {
        Wake::Stopped
    } else {
        Wake::Due
    }
}

/// How many passes each serving watch made, by its job's state folder: a test waits on the count
/// to know a wake was judged.
#[cfg(test)]
static PASSES: Mutex<Vec<(PathBuf, usize)>> = Mutex::new(Vec::new());

/// How many rests each node job's outer loop began, by its job's state folder ([`idles`]).
#[cfg(test)]
static IDLES: Mutex<Vec<(PathBuf, usize)>> = Mutex::new(Vec::new());

/// One more in `counts` for the job on `state_dir`.
#[cfg(test)]
fn count(counts: &Mutex<Vec<(PathBuf, usize)>>, state_dir: &Path) {
    let mut counts = counts.lock_or_recover();
    match counts.iter_mut().find(|(at, _)| at == state_dir) {
        Some((_, count)) => *count += 1,
        None => counts.push((state_dir.to_path_buf(), 1)),
    }
}

/// How many `counts` holds for the job on `state_dir`.
#[cfg(test)]
fn counted(counts: &Mutex<Vec<(PathBuf, usize)>>, state_dir: &Path) -> usize {
    counts
        .lock_or_recover()
        .iter()
        .find(|(at, _)| at == state_dir)
        .map_or(0, |(_, count)| *count)
}

/// Count a pass of the serving watch of the job on `state_dir` ([`passes`]).
#[cfg(test)]
pub(crate) fn count_pass(state_dir: &Path) {
    count(&PASSES, state_dir);
}

/// The deny list each serving watch's last pass judged its open reads and drives by (`None`:
/// none read), by its job's state folder.
#[cfg(test)]
static JUDGED: Mutex<Vec<(PathBuf, Option<Vec<String>>)>> = Mutex::new(Vec::new());

/// Note the deny list a pass of the job on `state_dir` judged by ([`judged_by`]).
#[cfg(test)]
pub(crate) fn judged(state_dir: &Path, denied: Option<&[String]>) {
    let denied = denied.map(<[String]>::to_vec);
    let mut judged = JUDGED.lock_or_recover();
    match judged.iter_mut().find(|(at, _)| at == state_dir) {
        Some((_, last)) => *last = denied,
        None => judged.push((state_dir.to_path_buf(), denied)),
    }
}

/// The deny list the last pass of the job on `state_dir` judged by: `None` before any pass,
/// `Some(None)` where that pass read none.
#[cfg(test)]
pub(crate) fn judged_by(state_dir: &Path) -> Option<Option<Vec<String>>> {
    JUDGED
        .lock_or_recover()
        .iter()
        .find(|(at, _)| at == state_dir)
        .map(|(_, denied)| denied.clone())
}

/// How many passes the serving watches of the job on `state_dir` made so far.
#[cfg(test)]
pub(crate) fn passes(state_dir: &Path) -> usize {
    counted(&PASSES, state_dir)
}

/// How many rests the outer loop of the job on `state_dir` began so far.
#[cfg(test)]
pub(crate) fn idles(state_dir: &Path) -> usize {
    counted(&IDLES, state_dir)
}

/// Guards a waiter against a duty that forgets to move its deadline: a deadline at or before now
/// on two passes running is put `pace` ahead (said once in aterm.log), so a forgotten deadline
/// costs the old tick, never a spin.
#[derive(Debug, Default)]
pub(crate) struct Pacer {
    past: u8,
    said: bool,
}

impl Pacer {
    /// The instant to wait until for a computed deadline `at`.
    pub(crate) fn next(&mut self, now: Instant, at: Instant, pace: Duration) -> Instant {
        if at > now {
            self.past = 0;
            return at;
        }
        self.past = self.past.saturating_add(1);
        if self.past < 2 {
            return now;
        }
        if !std::mem::replace(&mut self.said, true) {
            aterm_log::debug!(
                "fleet: a duty's deadline stayed past on two passes running; it waits the old \
                 pace instead"
            );
        }
        now + pace
    }
}

/// An `aterm.toml` met mid-write (an editor that writes in place and pauses), judged again once
/// the write settles: a switch that does not read is acted on only once it has not read for
/// [`aterm_uds::file_watch::SETTLE_CAP`], or at once where the waiter has no event to wake it for
/// the judgment ([`Self::act`]). A file left unreadable is acted on as before: fail closed.
#[derive(Debug, Default)]
pub(crate) struct Unreadable {
    since: Option<Instant>,
}

impl Unreadable {
    /// Whether `[swarm] enabled` holds sharing off now, given its reading `switch` (`None`: a
    /// waiter told of no `aterm.toml`, which has no switch): off reads held, and one that did not
    /// read holds once it has not read for the settle cap where the waiter hears the file's
    /// writes (`evented`), at once where it does not. A reading ends the episode.
    pub(crate) fn holds<E>(
        &mut self,
        switch: Option<Result<bool, E>>,
        evented: bool,
        now: Instant,
    ) -> bool {
        match switch {
            None => {
                self.since = None;
                false
            }
            Some(Ok(enabled)) => {
                self.since = None;
                !enabled
            }
            Some(Err(_)) if !evented => true,
            Some(Err(_)) => {
                let since = *self.since.get_or_insert(now);
                now.saturating_duration_since(since) >= aterm_uds::file_watch::SETTLE_CAP
            }
        }
    }

    /// When the episode is judged again (`None`: none).
    pub(crate) fn settle(&self) -> Option<Instant> {
        self.since
            .map(|since| since + aterm_uds::file_watch::SETTLE_CAP)
    }
}

/// How long a job that waits for the root LAN broker rests before it asks again with no new
/// broker socket heard ([`aterm_uds::lan::Arrivals`]): a broker whose socket answered the watch's
/// hello but not the job's (its code check did not finish, or it was serving its most
/// connections) answers a later hello with no new socket. Availability, not security.
pub(crate) const LAN_LOOK: Duration = Duration::from_secs(60);

/// The longest a job whose bind meets the fleet port held by a program that is no aterm window
/// rests before it tries again: that program's close of its socket has no event.
pub(crate) const FOREIGN_MOST: Duration = Duration::from_secs(60);

/// Why the node job's outer loop idles, which names what wakes it (KI-1108-2).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Idle {
    /// Nothing to serve now: the fleet left or sharing off, no `[fleet]` to serve, no take, no
    /// key, a listen address that does not parse or that the loopback rule refuses. Each is a
    /// read of a watched file: its next ring.
    Inputs,
    /// The root LAN broker does not answer: a new broker socket that answers, or [`LAN_LOOK`].
    Broker,
    /// Another holds `node-port.lock` (a window's probe, another job): its release.
    PortHeld,
    /// The bind met the port in use: the release of `window-port.lock` by the window that has
    /// the port bound, or, where no window holds that, a backoff from the old tick to
    /// [`FOREIGN_MOST`] that any ring resets.
    PortInUse,
    /// A state no event tells (the port's custody unreadable, a bind refused otherwise, the bound
    /// address unreadable): the old tick.
    Paced,
}

/// The node job's outer loop's rest between its looks (KI-1108-2): a ring of the job's inputs,
/// its stop, the event its reason to idle names ([`Idle`]), its upkeep's deadline, the old tick
/// while work no event tells lasts (a boot trial, an owed look), and [`BACKSTOP`]. Where the file
/// watch did not start it rests the old tick exactly, as the loop did.
pub(crate) struct NodeIdle {
    node: Arc<NodeInputs>,
    state_dir: PathBuf,
    tick: Duration,
    upkeep: Option<Instant>,
    paced: bool,
    pacer: Pacer,
    foreign: Option<Duration>,
    #[cfg(target_os = "macos")]
    arrivals: Option<aterm_uds::lan::Arrivals>,
}

impl NodeIdle {
    /// A rest over `node`'s inputs for the job on `state_dir`, whose old tick is `tick`.
    pub(crate) fn new(node: &Arc<NodeInputs>, state_dir: &Path, tick: Duration) -> Self {
        Self {
            node: Arc::clone(node),
            state_dir: state_dir.to_path_buf(),
            tick: tick.min(LONGEST),
            upkeep: None,
            paced: false,
            pacer: Pacer::default(),
            foreign: None,
            #[cfg(target_os = "macos")]
            arrivals: None,
        }
    }

    /// What this round of the loop found that no event tells: when its local upkeep is next
    /// owed (`upkeep`, [`super::node_serve::NodeMaintenance`]), and whether work no file event
    /// tells lasts (`paced`: a boot trial, an owed look), which keeps the old tick while it does.
    pub(crate) fn round(&mut self, upkeep: Option<Instant>, paced: bool) {
        self.upkeep = upkeep;
        self.paced = paced;
    }

    /// A generation starts: a later bind that meets the port in use backs off from the old tick
    /// again.
    pub(crate) fn serving(&mut self) {
        self.foreign = None;
    }

    /// Rest for `why`, `seen` the view the job took before this round's reads: a ring since then
    /// ends the rest at once. Where the file watch did not start, the old tick on `stop` exactly.
    pub(crate) fn wait(&mut self, stop: &Stop, seen: &mut Seen, why: Idle) {
        #[cfg(test)]
        count(&IDLES, &self.state_dir);
        if !self.node.watching() {
            stop.sleep(self.tick);
            return;
        }
        let now = Instant::now();
        let Some(duties) = self.arm(now, why) else {
            return;
        };
        let at = duties.earliest(self.tick).unwrap_or(now + BACKSTOP);
        let deadline = self.pacer.next(now, at, self.tick);
        let rung = match seen.arm() {
            Ok(rung) => rung,
            Err(generation) => {
                seen.acknowledge(generation);
                self.foreign = None;
                return;
            }
        };
        let waited = stop.bell().is_ok_and(|bell| {
            let fds: Vec<_> = std::iter::once(bell).chain(self.broker_fd(why)).collect();
            rung.wait_ready(&fds, Some(deadline)).is_ok()
        });
        if !waited {
            // No descriptor to wait on: the stop alone, at most the old tick, so a wait that
            // cannot be made is never a spin.
            let until = deadline.min(Instant::now() + self.tick);
            stop.sleep(until.saturating_duration_since(Instant::now()));
        }
        if !stop.is_set() && seen.take() {
            self.foreign = None;
        }
    }

    /// Arm what a rest for `why` waits for, and name the deadlines it keeps at `now`: `None` where
    /// a broker socket answered already, so the job asks again at once. A rest whose event could
    /// not be armed (a waiter thread that did not start, a broker watch that cannot be made)
    /// keeps the old tick.
    fn arm(&mut self, now: Instant, why: Idle) -> Option<Duties> {
        use aterm_agent::node_service::{NODE_PORT_LOCK, WINDOW_PORT_LOCK, window_port_holds_lock};
        if why != Idle::PortInUse {
            self.foreign = None;
        }
        let bell = self.node.bell();
        let (armed, retry) = match why {
            Idle::Inputs => (true, None),
            Idle::Paced => (false, None),
            Idle::PortHeld => (ring_on_release(&self.state_dir, NODE_PORT_LOCK, bell), None),
            Idle::PortInUse if window_port_holds_lock(&self.state_dir) => {
                self.foreign = None;
                (
                    ring_on_release(&self.state_dir, WINDOW_PORT_LOCK, bell),
                    None,
                )
            }
            Idle::PortInUse => {
                let after = self.foreign.map_or(self.tick, |last| {
                    last.saturating_mul(2).min(FOREIGN_MOST.max(self.tick))
                });
                self.foreign = Some(after);
                (true, Some(now + after))
            }
            Idle::Broker => match self.broker() {
                Ok(true) => return None,
                Ok(false) => (true, Some(now + self.broker_look())),
                Err(_) => (false, None),
            },
        };
        Some(Duties {
            maintenance: self.upkeep,
            paced: (!armed || self.paced).then(|| now + self.tick),
            retry,
            backstop: Some(now + BACKSTOP),
            ..Duties::default()
        })
    }

    /// Whether a broker socket not answered before answers now ([`aterm_uds::lan::Arrivals`]),
    /// the watch made at the first ask, on the job's thread, whose endpoint it captures.
    #[cfg(target_os = "macos")]
    fn broker(&mut self) -> std::io::Result<bool> {
        self.arrivals
            .get_or_insert_with(aterm_uds::lan::Arrivals::new)
            .poll()
    }

    /// How long a rest for the broker lasts with nothing heard: [`LAN_LOOK`], or less while the
    /// watch owes a socket that did not answer yet another hello.
    #[cfg(target_os = "macos")]
    fn broker_look(&self) -> Duration {
        self.arrivals
            .as_ref()
            .map_or(LAN_LOOK, |arrivals| arrivals.wait_for(LAN_LOOK))
    }

    /// The new broker sockets' descriptor a rest for `why` waits on beside its stop: a rest for
    /// the broker's, once its watch is made.
    #[cfg(target_os = "macos")]
    fn broker_fd(&self, why: Idle) -> Option<std::os::fd::BorrowedFd<'_>> {
        self.arrivals
            .as_ref()
            .filter(|_| why == Idle::Broker)
            .and_then(aterm_uds::lan::Arrivals::watch_fd)
    }

    /// Only macOS gates the LAN on a broker ([`crate::network_moved::lan_unavailable`]): elsewhere
    /// a rest for it keeps the old tick.
    #[cfg(not(target_os = "macos"))]
    fn broker(&mut self) -> std::io::Result<bool> {
        Err(std::io::Error::from(std::io::ErrorKind::Unsupported))
    }

    #[cfg(not(target_os = "macos"))]
    fn broker_look(&self) -> Duration {
        LAN_LOOK
    }

    #[cfg(not(target_os = "macos"))]
    fn broker_fd(&self, _why: Idle) -> Option<std::os::fd::BorrowedFd<'_>> {
        None
    }
}

/// Why a window's fleet listener rests between its looks, which names what wakes it (KI-1108-2).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Rest {
    /// A read of a watched file stood in its way (a take that does not match the vault, a key
    /// that cannot be used), or it leaves the port to the node job with the mesh on: its inputs'
    /// next ring.
    Inputs,
    /// It leaves the port to the node job, which holds `node-port.lock`: that lock's release (a
    /// crash's included), or a ring.
    NodeHolds,
    /// Its bind was refused (the port in use, the address not this machine's yet): the release
    /// of `window-port.lock` where another window has the port bound, a network move, a ring, and
    /// a backoff from the old tick to [`FOREIGN_MOST`] that each of those resets, since a program
    /// that is no aterm window closes its socket with no event.
    Unbound,
    /// Its reads met a state no event tells (a vault writer holding its lock, a deny read that did
    /// not settle): the old tick.
    Paced,
}

/// A window's fleet listener's rest between its looks (KI-1108-2): a ring of the window's inputs
/// (a change of its `[fleet]` among them, [`ring_window`]), the event its reason names ([`Rest`]),
/// and its backstop. Where the file watch did not start it rests the old tick exactly, ended
/// early only by a change of what it serves, as it did.
pub(crate) struct WindowRest {
    window: Arc<WindowInputs>,
    state_dir: Option<PathBuf>,
    tick: Duration,
    backstop: Duration,
    pacer: Pacer,
    foreign: Option<Duration>,
    network: crate::network_moved::Watch,
}

impl WindowRest {
    /// A rest over `window`'s inputs for the listener on `state_dir`, whose old tick is `tick`,
    /// resting at most `backstop` with nothing heard, a refused bind woken by the moves `network`
    /// hears.
    pub(crate) fn new(
        window: &Arc<WindowInputs>,
        state_dir: Option<&Path>,
        (tick, backstop): (Duration, Duration),
        network: crate::network_moved::Watch,
    ) -> Self {
        Self {
            window: Arc::clone(window),
            state_dir: state_dir.map(Path::to_path_buf),
            tick: tick.min(LONGEST),
            backstop: backstop.min(LONGEST),
            pacer: Pacer::default(),
            foreign: None,
            network,
        }
    }

    /// Whether the file watch runs ([`Inputs::watching`]).
    pub(crate) fn watching(&self) -> bool {
        self.window.watching()
    }

    /// Rest for `why` while `unchanged` holds (the listener's generation, and its `[fleet]` where
    /// the caller follows that in place), `seen` the view the listener took before this round's
    /// reads: a ring since then ends the rest at once. Where the file watch did not start, the old
    /// tick, ended early only once `unchanged` does not hold.
    pub(crate) fn wait(&mut self, seen: &mut Seen, why: Rest, unchanged: &dyn Fn() -> bool) {
        #[cfg(test)]
        if let Some(state) = &self.state_dir {
            count(&RESTS, state);
        }
        if !self.watching() {
            self.tick_wait(seen, unchanged);
            return;
        }
        let now = Instant::now();
        let (armed, retry) = self.arm(now, why);
        let duties = Duties {
            retry,
            paced: (!armed).then(|| now + self.tick),
            backstop: Some(now + self.backstop),
            ..Duties::default()
        };
        let at = duties.earliest(self.tick).unwrap_or(now + self.backstop);
        let deadline = self.pacer.next(now, at, self.tick);
        let rung = match seen.arm() {
            Ok(rung) => rung,
            Err(generation) => {
                seen.acknowledge(generation);
                self.foreign = None;
                return;
            }
        };
        // Armed first, then judged: a change made since is the ring the wait takes.
        if !unchanged() {
            return;
        }
        let moved = if why == Rest::Unbound {
            match self.network.wait_until(&[&rung], Some(deadline)) {
                Ok(woke) => woke.is_some_and(|woke| woke != crate::network_moved::Woke::Timer),
                Err(_) => {
                    // No native wait: the ring alone, at most the old tick, never a spin.
                    let until = deadline.min(Instant::now() + self.tick);
                    rung.sleep(until.saturating_duration_since(Instant::now()));
                    false
                }
            }
        } else {
            rung.sleep(deadline.saturating_duration_since(Instant::now()));
            false
        };
        if seen.take() || moved {
            self.foreign = None;
        }
    }

    /// The rest before the watch: the old tick, ended early only by a ring after which
    /// `unchanged` does not hold. Off the watch the bell rings only for a change of the window's
    /// `[fleet]`, which the old wait was woken for.
    fn tick_wait(&self, seen: &mut Seen, unchanged: &dyn Fn() -> bool) {
        let until = Instant::now() + self.tick;
        loop {
            let rung = match seen.arm() {
                Ok(rung) => rung,
                Err(generation) => {
                    seen.acknowledge(generation);
                    continue;
                }
            };
            if !unchanged() {
                return;
            }
            let left = until.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return;
            }
            rung.sleep(left);
        }
    }

    /// Arm what a rest for `why` waits for at `now`: whether an event answers for it (a waiter
    /// thread that did not start, or a state no event tells, keeps the old tick), and when a
    /// refused bind is tried again with nothing heard.
    fn arm(&mut self, now: Instant, why: Rest) -> (bool, Option<Instant>) {
        use aterm_agent::node_service::{NODE_PORT_LOCK, WINDOW_PORT_LOCK, window_port_holds_lock};
        if why != Rest::Unbound {
            self.foreign = None;
        }
        let bell = self.window.bell();
        match (why, self.state_dir.as_deref()) {
            (Rest::Inputs, _) => (true, None),
            (Rest::Paced, _) | (Rest::NodeHolds, None) => (false, None),
            (Rest::NodeHolds, Some(state)) => (ring_on_release(state, NODE_PORT_LOCK, bell), None),
            (Rest::Unbound, state) => {
                let after = self.foreign.map_or(self.tick, |last| {
                    last.saturating_mul(2).min(FOREIGN_MOST.max(self.tick))
                });
                self.foreign = Some(after);
                // Another window that has the port bound lets it go with an event; the backoff
                // stands beside it, as the listener's line says.
                if let Some(state) = state
                    && window_port_holds_lock(state)
                {
                    ring_on_release(state, WINDOW_PORT_LOCK, bell);
                }
                (true, Some(now + after))
            }
        }
    }
}

/// How many rests each window listener began, by its state folder ([`rests`]).
#[cfg(test)]
static RESTS: Mutex<Vec<(PathBuf, usize)>> = Mutex::new(Vec::new());

/// How many rests the window listener on `state_dir` began so far.
#[cfg(test)]
pub(crate) fn rests(state_dir: &Path) -> usize {
    counted(&RESTS, state_dir)
}

/// How long a port waiter whose wait failed rests before it rings: the old tick.
const FAILED_WAIT_PAUSE: Duration = Duration::from_secs(2);

/// The bells waiting for the release of each port lock (by its path), at most one waiter thread
/// per path in this process.
static PORT_WAITERS: Mutex<Vec<(PathBuf, Vec<Weak<Bell>>)>> = Mutex::new(Vec::new());

/// Ring `bell` once no process holds `<state_dir>/<name>` (a port lock,
/// [`aterm_agent::node_service::wait_port_released`]). Called once a probe found the lock held: a
/// release between that probe and this call rings at once. At most one waiter thread per lock
/// in this process, never joined; it holds only weak bells, so it outlives none of them in
/// effect. Whether a waiter is armed (a thread that could not start arms none, and the caller
/// keeps its pace).
pub(crate) fn ring_on_release(state_dir: &Path, name: &'static str, bell: &Arc<Bell>) -> bool {
    let path = state_dir.join(name);
    {
        let mut waiters = PORT_WAITERS.lock_or_recover();
        if let Some((_, bells)) = waiters.iter_mut().find(|(at, _)| *at == path) {
            bells.retain(|bell| bell.strong_count() > 0);
            if !bells.iter().any(|held| held.ptr_eq(&Arc::downgrade(bell))) {
                bells.push(Arc::downgrade(bell));
            }
            return true;
        }
        waiters.push((path.clone(), vec![Arc::downgrade(bell)]));
    }
    let state = state_dir.to_path_buf();
    let released = path.clone();
    let spawned = std::thread::Builder::new()
        .name("aterm-fleet-port-wait".to_owned())
        .spawn(move || {
            if let Err(error) = aterm_agent::node_service::wait_port_released(&state, name) {
                // The probe that armed this read the file; a wait that cannot is a race with its
                // mode or owner changing. Ring at the old pace, so a caller that arms again on
                // each ring retries no faster than its tick did.
                aterm_log::debug!("fleet: the wait for {name}'s release failed: {error}");
                std::thread::sleep(FAILED_WAIT_PAUSE);
            }
            let bells = {
                let mut waiters = PORT_WAITERS.lock_or_recover();
                let at = waiters.iter().position(|(at, _)| *at == released);
                at.map(|at| waiters.swap_remove(at).1).unwrap_or_default()
            };
            for bell in bells.iter().filter_map(Weak::upgrade) {
                bell.ring();
            }
        });
    if spawned.is_err() {
        PORT_WAITERS.lock_or_recover().retain(|(at, _)| *at != path);
        return false;
    }
    true
}

/// Whether a waiter for the release of `<state_dir>/<name>` is armed in this process.
#[cfg(test)]
pub(crate) fn waiting_on(state_dir: &Path, name: &str) -> bool {
    let path = state_dir.join(name);
    PORT_WAITERS
        .lock_or_recover()
        .iter()
        .any(|(at, _)| *at == path)
}

#[cfg(test)]
#[path = "fleet_inputs_tests.rs"]
mod tests;
