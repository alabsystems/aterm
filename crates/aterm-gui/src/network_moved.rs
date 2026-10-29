// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THIS PROCESS'S "NETWORK MOVED" SIGNAL (docs/DESIGN-fluid-swarm-2026-10-28.md §3.5.2, F3c).
//! Nothing carries the signal between processes, so the window and the node job each watch
//! their own sources: the kernel's route socket ([`aterm_uds::network::RouteChanges`]), the one
//! `_alab._tcp` browse a process holds, on a Bonjour connection it owns, which tells each member
//! it did not hold and resolves what it holds to dial hints ([`hear_alab_tcp`]: the node job's,
//! and a window's only while it serves the fleet port, §4.1 F17), and the Commons runtime's peers, one heard again after a silence
//! of [`SILENCE`] ([`Heard`]). Each consumer (the window's converge lane, a fleet heartbeat, the sign-in
//! ladder's waits and the account claims, the fleet glance's backoff) holds its own [`Watch`],
//! is woken once this Mac's spread has passed ([`spread_of`]: a few seconds, the same every time
//! on one Mac and different between Macs, so ~35 Macs back on one LAN do not dial the issuer and
//! the broker at one instant; a share of it as long as the herd this Mac's fleet makes,
//! [`herded`], and none for a Mac in no fleet), and is told which source moved, which it says in
//! aterm.log. The API reach probe, which dials no fleet server, hears each move at once
//! ([`watch_at_once`]).

use std::sync::{Arc, Condvar, Mutex, OnceLock, PoisonError};
use std::time::{Duration, Instant};

/// What ended a [`Watch::wait`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Woke {
    /// Its time ran out with no move.
    Timer,
    /// The kernel told of a change to this Mac's addresses or links.
    #[cfg_attr(
        all(windows, not(test)),
        expect(dead_code, reason = "the route source is macOS's and Linux's")
    )]
    Route,
    /// The process's `_alab._tcp` browse heard a member it did not hold ([`hear_alab_tcp`]).
    #[cfg_attr(
        not(any(target_os = "macos", all(test, unix))),
        expect(dead_code, reason = "the browse source is macOS's")
    )]
    Browse,
    /// The Commons runtime heard a peer again after a silence ([`SILENCE`]), or a new one.
    Beacon,
    /// macOS's Local Network answer for aterm moved as the window heard it
    /// ([`local_network_moved`]): which LAN hosts aterm may reach changed, or may have.
    LocalNetwork,
}

impl Woke {
    /// Its words in an aterm.log line.
    pub(crate) const fn words(self) -> &'static str {
        match self {
            Self::Timer => "its timer",
            Self::Route => "a route change",
            Self::Browse => "a member heard on the _alab._tcp browse",
            Self::Beacon => "a Commons beacon from a peer not heard for two minutes",
            Self::LocalNetwork => "macOS's Local Network answer for aterm",
        }
    }
}

/// The longest a consumer that waits only for moves waits at once; it waits again after.
pub(crate) const LONG: Duration = Duration::from_secs(24 * 60 * 60);

/// One process's signal: how many moves were told, the source of the last, and when its watches
/// are due to hear it (`None`: at once).
#[derive(Debug)]
pub(crate) struct Moved {
    told: Mutex<(u64, Woke, Option<Instant>)>,
    changed: Condvar,
    /// How long after a move its watches hear it, where this Mac shares its moves with a whole
    /// herd ([`HERD`]): this Mac's spread ([`spread_of`]).
    spread: Duration,
    /// How many other Macs share this one's moves, read at each move ([`members_here`]): the
    /// spread is the share of [`Moved::spread`] that herd makes ([`herded`]).
    herd: fn() -> usize,
}

impl Moved {
    /// A signal no source has told yet, whose watches hear each move at once.
    #[cfg(test)]
    pub(crate) const fn new() -> Self {
        Self::spread_by(Duration::ZERO)
    }

    /// A signal no source has told yet, whose watches hear each move `spread` after it was told,
    /// save macOS's Local Network answer ([`Woke::LocalNetwork`]), which they hear at once: a
    /// signal whose Mac shares its moves with a whole herd.
    #[cfg(test)]
    pub(crate) const fn spread_by(spread: Duration) -> Self {
        /// A herd past any [`HERD`].
        const fn whole() -> usize {
            usize::MAX
        }
        Self::herded_by(spread, whole)
    }

    /// A signal no source has told yet, whose watches hear each move the share of `spread` that
    /// `herd` (how many other Macs share this one's moves, read at each move) makes after it was
    /// told ([`herded`]), save macOS's Local Network answer ([`Woke::LocalNetwork`]), which they
    /// hear at once.
    pub(crate) const fn herded_by(spread: Duration, herd: fn() -> usize) -> Self {
        Self {
            told: Mutex::new((0, Woke::Timer, None)),
            changed: Condvar::new(),
            spread,
            herd,
        }
    }

    /// The network moved, as `source` heard now ([`Moved::tell_at`]).
    pub(crate) fn tell(&self, source: Woke) {
        self.tell_at(source, Instant::now());
    }

    /// The network moved, as `source` heard at `at`: every watch wakes once this signal's spread,
    /// the share of it the herd read now makes ([`herded`]), has passed since `at` (a watch made
    /// by [`Moved::watch_at_once`] at once). A move told while another is still to be heard is
    /// one wake with it, at the later one's time. macOS's Local Network answer is this Mac's own,
    /// no event on the LAN every Mac hears at once, so its watches hear it at once.
    pub(crate) fn tell_at(&self, source: Woke, at: Instant) {
        let spread = if self.spread.is_zero() || source == Woke::LocalNetwork {
            Duration::ZERO
        } else {
            herded(self.spread, (self.herd)())
        };
        let due = if spread.is_zero() {
            None
        } else {
            at.checked_add(spread)
        };
        let mut told = self.told.lock().unwrap_or_else(PoisonError::into_inner);
        told.0 = told.0.wrapping_add(1);
        told.1 = source;
        told.2 = due;
        drop(told);
        self.changed.notify_all();
    }

    /// How many moves were told.
    #[cfg(all(test, unix))]
    pub(crate) fn told(&self) -> u64 {
        self.told.lock().unwrap_or_else(PoisonError::into_inner).0
    }

    /// A watch that hears each move told from now on, once it is due (this Mac's spread after it
    /// was told).
    pub(crate) fn watch(self: &Arc<Self>) -> Watch {
        self.watch_spread(true)
    }

    /// A watch that hears each move told from now on at once, never waiting out this Mac's
    /// spread: for a consumer that dials no server a herd shares (the API reach probe).
    pub(crate) fn watch_at_once(self: &Arc<Self>) -> Watch {
        self.watch_spread(false)
    }

    fn watch_spread(self: &Arc<Self>, spread: bool) -> Watch {
        let seen = self.told.lock().unwrap_or_else(PoisonError::into_inner).0;
        Watch {
            moved: Arc::clone(self),
            seen,
            spread,
        }
    }
}

/// One consumer's view of a [`Moved`]: the moves it has not been told of yet, and whether it
/// waits each one's spread out (`spread`; [`Moved::watch_at_once`] makes one that does not).
#[derive(Debug)]
pub(crate) struct Watch {
    moved: Arc<Moved>,
    seen: u64,
    spread: bool,
}

impl Watch {
    /// Wait at most `most` for a move this watch was not told of, once it is due (this Mac's
    /// spread after it was told): the source of the latest one (several told meanwhile are one
    /// wake), else [`Woke::Timer`], which leaves a move not yet due to a later wait.
    pub(crate) fn wait(&mut self, most: Duration) -> Woke {
        let deadline = Instant::now().checked_add(most);
        let mut told = self
            .moved
            .told
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        loop {
            let now = Instant::now();
            let unheard = told.0 != self.seen;
            let until_due = told
                .2
                .filter(|_| self.spread)
                .map_or(Duration::ZERO, |due| due.saturating_duration_since(now));
            if unheard && until_due.is_zero() {
                self.seen = told.0;
                return told.1;
            }
            let left = deadline.map_or(LONG, |at| at.saturating_duration_since(now));
            if left.is_zero() {
                return Woke::Timer;
            }
            let nap = if unheard { until_due.min(left) } else { left };
            told = self
                .moved
                .changed
                .wait_timeout(told, nap.min(LONG))
                .unwrap_or_else(PoisonError::into_inner)
                .0;
        }
    }

    /// The source of the latest move this watch was not told of and is due at `now`, without
    /// waiting.
    #[cfg_attr(
        all(windows, not(test)),
        expect(dead_code, reason = "its one caller, the heartbeat clock, is Unix's")
    )]
    pub(crate) fn take_at(&mut self, now: Instant) -> Option<Woke> {
        let told = self
            .moved
            .told
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        (told.0 != self.seen && told.2.is_none_or(|due| !self.spread || now >= due)).then(|| {
            self.seen = told.0;
            told.1
        })
    }

    /// [`Watch::take_at`] now.
    #[cfg(test)]
    pub(crate) fn take(&mut self) -> Option<Woke> {
        self.take_at(Instant::now())
    }
}

/// This process's signal, spread by this Mac's spread ([`process_spread`]) as its fleet's herd
/// makes it ([`members_here`]), its route and browse sources started with it (none in a test
/// build).
fn process() -> &'static Arc<Moved> {
    static PROCESS: OnceLock<Arc<Moved>> = OnceLock::new();
    PROCESS.get_or_init(|| {
        let moved = Arc::new(Moved::herded_by(process_spread(), members_here));
        start_sources(&moved);
        moved
    })
}

/// The window a Mac's spread falls in: about 230 ms a Mac across ~35 Macs, short beside the 30 s
/// a source rests and the minutes a Mac was away.
const SPREAD: Duration = Duration::from_secs(8);

/// The herd [`SPREAD`] is sized for, about the fleet the owner runs: a Mac that shares its moves
/// with fewer other Macs spreads over their share of it ([`herded`]).
const HERD: u64 = 35;

/// The share of `spread` (a Mac's offset in [`SPREAD`]) a herd of `others` other Macs makes:
/// `others` [`HERD`]ths of it, the whole past a herd, none for a Mac alone, so the window stays
/// about 230 ms a Mac and a Mac with nobody to herd with acts at once.
fn herded(spread: Duration, others: usize) -> Duration {
    let share = u64::try_from(others).unwrap_or(u64::MAX).min(HERD);
    let ms = u64::try_from(spread.as_millis()).unwrap_or(u64::MAX);
    Duration::from_millis(ms.saturating_mul(share) / HERD)
}

/// How many other members share this Mac's moves: the peers its fleet book holds and its copy of
/// its issuer's book, each once ([`aterm_net::fleet_member::answer_key`]), this Mac's own node
/// aside; none in no fleet. Read at each move, so a join or a leave counts from the next one. A
/// member away from its LAN still counts the fleet (the book does not say where each is), and so
/// waits its share, in the consumers that dial the fleet: the API reach probe's watch waits none
/// ([`Moved::watch_at_once`]).
#[cfg(all(not(test), unix))]
fn members_here() -> usize {
    use aterm_net::fleet_member as member;
    let Some(state) = aterm_types::dirs::state_dir() else {
        return 0;
    };
    let own = member::read_node_id(&state).ok().flatten();
    let book = member::read_peers(&state).unwrap_or_default();
    let copies = book
        .iter()
        .find(|peer| peer.via == member::Via::Issuer)
        .and_then(|issuer| issuer.node.as_deref())
        .map(|issuer| member::read_issuer_book(&state, issuer))
        .unwrap_or_default();
    book.iter()
        .chain(&copies)
        .filter(|peer| peer.node.is_none() || peer.node != own)
        .map(member::answer_key)
        .collect::<std::collections::BTreeSet<_>>()
        .len()
}

/// None off Unix, which keeps no fleet book, and in a test build, whose signals name their own
/// herd ([`Moved::herded_by`]).
#[cfg(any(test, not(unix)))]
const fn members_here() -> usize {
    0
}

/// This Mac's spread: [`spread_of`] its node id (read, never minted), else its LAN host name.
#[cfg(not(test))]
fn process_spread() -> Duration {
    spread_of(spread_seed(node_id_here, aterm_uds::process::lan_host_name).as_deref())
}

/// This machine's fleet node id, read and never minted: `None` before any `aterm fleet init` or
/// `join` ran here.
#[cfg(all(not(test), unix))]
fn node_id_here() -> Option<String> {
    aterm_types::dirs::state_dir()
        .and_then(|state| aterm_net::fleet_member::read_node_id(&state).ok().flatten())
}

/// None off Unix, which keeps no fleet node: the host name seeds the spread.
#[cfg(all(not(test), not(unix)))]
const fn node_id_here() -> Option<String> {
    None
}

/// None in a test build: a test that wants one spreads its own [`Moved`].
#[cfg(test)]
const fn process_spread() -> Duration {
    Duration::ZERO
}

/// What a Mac's spread is derived from: `node` (its fleet node id) where it holds one, else
/// `host` (its LAN host name); `None` with neither.
fn spread_seed(
    node: impl FnOnce() -> Option<String>,
    host: impl FnOnce() -> Option<String>,
) -> Option<String> {
    node()
        .filter(|seed| !seed.is_empty())
        .or_else(|| host().filter(|seed| !seed.is_empty()))
}

/// A Mac's spread, from `seed` ([`spread_seed`]): a point in [`SPREAD`] that is the same each
/// time for one seed and lands apart for different ones (FNV-1a, its high bits); none without a
/// seed.
fn spread_of(seed: Option<&str>) -> Duration {
    let Some(seed) = seed else {
        return Duration::ZERO;
    };
    let window = u64::try_from(SPREAD.as_millis()).unwrap_or(u64::MAX).max(1);
    Duration::from_millis((aterm_hash::fnv1a_64(seed.as_bytes()) >> 16) % window)
}

/// A watch on this process's signal ([`process`]).
pub(crate) fn watch() -> Watch {
    process().watch()
}

/// A watch on this process's signal that hears each move at once ([`Moved::watch_at_once`]).
#[cfg_attr(
    not(any(target_os = "macos", target_os = "linux")),
    expect(
        dead_code,
        reason = "its one caller, the API reach probe's route watch, is macOS's and Linux's"
    )
)]
pub(crate) fn watch_at_once() -> Watch {
    process().watch_at_once()
}

/// The window heard macOS's Local Network answer for aterm move (`App::tick_local_network_step`):
/// its question went up, which the access record keeps as asked from then on, or the re-probe
/// after it ended, whatever it heard. Each watch of this process wakes with
/// [`Woke::LocalNetwork`], so what reads the LAN (the converge pass's `sync.peers`, which reads
/// "macOS has not asked" until the record says asked) reads it again now, not at its own time.
pub(crate) fn local_network_moved() {
    process().tell(Woke::LocalNetwork);
}

/// Start this process's route source and its one `_alab._tcp` browse ([`hear_alab_tcp`]), so a
/// window hears members come back while the node job holds the fleet port.
fn start_sources(moved: &Arc<Moved>) {
    #[cfg(all(not(test), any(target_os = "macos", target_os = "linux")))]
    hear_routes(moved);
    hear_alab_tcp_on(moved, &[]);
    let _ = moved;
}

/// This process's one `_alab._tcp` browse, once [`hear_alab_tcp`] started it.
#[cfg(unix)]
static ALAB_TCP: OnceLock<Arc<aterm_secrets::discover::Watch>> = OnceLock::new();

/// Start this process's one `_alab._tcp` browse ([`hear_browse`], read by an
/// [`aterm_secrets::discover::Watch`]), holding `own` (this process's own advert names) as its
/// own: each member it did not hold is a move ([`Woke::Browse`]), and what it holds resolves to
/// dial hints ([`alab_tcp`]). Each process starts it with its sources ([`start_sources`]), so the
/// move detector and the heartbeat share one browse; a later call adds its `own` names. None
/// starts in a test build, outside macOS, or under a private state root or CI (`ATERM_STATE_HOME`,
/// `CI`, as the Commons runtime).
#[cfg(unix)]
pub(crate) fn hear_alab_tcp(own: &[&str]) {
    hear_alab_tcp_on(process(), own);
}

/// [`hear_alab_tcp`], telling `moved` (the process's signal, which [`start_sources`] is starting
/// before [`process`] holds it) of each move.
fn hear_alab_tcp_on(moved: &Arc<Moved>, own: &[&str]) {
    #[cfg(all(not(test), target_os = "macos"))]
    if std::env::var_os("ATERM_STATE_HOME").is_none() && std::env::var_os("CI").is_none() {
        let watch = ALAB_TCP.get_or_init(|| {
            let bell = Arc::new(aterm_secrets::wake::Bell::new());
            let watch = Arc::new(aterm_secrets::discover::Watch::idle_for(
                aterm_agent::node_service::ALAB_TCP_SERVICE,
                &bell,
                own,
            ));
            hear_browse(&watch, moved);
            spawn_source(
                "aterm-moved-browse",
                Arc::clone(moved),
                (Woke::Browse, REST),
                move || Some(bell.nap(LONG) == aterm_secrets::wake::Wake::Browse),
            );
            watch
        });
        for name in own {
            watch.note_own(name);
        }
    }
    let _ = (moved, own);
}

/// This process's `_alab._tcp` browse, once [`hear_alab_tcp`] started it.
#[cfg(unix)]
pub(crate) fn alab_tcp() -> Option<Arc<aterm_secrets::discover::Watch>> {
    ALAB_TCP.get().cloned()
}

/// How long after a browse connection ended another starts.
#[cfg(all(not(test), target_os = "macos"))]
const BROWSE_AGAIN: Duration = Duration::from_secs(60);

/// How long the browse, waiting for System State's Local Network step ([`browse_after_the_step`]),
/// waits before it looks again with no move told: the step's question wakes it sooner
/// ([`local_network_moved`]).
#[cfg(all(not(test), target_os = "macos"))]
const STEP_LOOK: Duration = Duration::from_secs(10);

/// Start the `browse` (a Bonjour browse is a Local Network use: it puts macOS's Local Network
/// question on screen where macOS never asked aterm) only once this process `may_use` the LAN
/// ([`aterm_types::macos_setup::lan_first_use::may_use`]): not while System State's Local Network
/// step is still to come in a window that runs first-run setup, which asks that question first.
/// Until then it `wait`s (for a move told, or a while) and reads `may_use` again. Whether it had
/// to wait.
#[cfg(target_os = "macos")]
fn browse_after_the_step(
    mut may_use: impl FnMut() -> bool,
    mut wait: impl FnMut(),
    browse: impl FnOnce(),
) -> bool {
    let mut waited = false;
    while !may_use() {
        waited = true;
        wait();
    }
    browse();
    waited
}

/// The native `_alab._tcp` browse `watch` reads: a Bonjour connection this process owns
/// ([`aterm_net::bonjour::browse`]), so no browser helper outlives it, read on a thread of its
/// own. Each event reaches the watch ([`aterm_secrets::discover::Watch::hear`]), which keeps the
/// baseline, each instance's interfaces and the bound, and rings its bell for a member it did not
/// hold without blocking the reader. A connection that ended starts again after
/// [`BROWSE_AGAIN`], its first events a baseline again.
#[cfg(all(not(test), target_os = "macos"))]
fn hear_browse(watch: &Arc<aterm_secrets::discover::Watch>, moved: &Arc<Moved>) {
    let watch = Arc::clone(watch);
    let mut step = moved.watch();
    let spawned = std::thread::Builder::new()
        .name("aterm-moved-bonjour".to_owned())
        .spawn(move || {
            crate::qos::set_self(crate::qos::Role::Background);
            let Ok(service) = std::ffi::CString::new(aterm_agent::node_service::ALAB_TCP_SERVICE)
            else {
                aterm_log::warn!("network moved: the browse service name contains a NUL");
                return;
            };
            let waited = browse_after_the_step(
                aterm_types::macos_setup::lan_first_use::may_use,
                || {
                    let _ = step.wait(STEP_LOOK);
                },
                || browse_until_gone(&service, &watch),
            );
            if waited {
                aterm_log::info!(
                    "network moved: the _alab._tcp browse waited for System State's Local \
                     Network step"
                );
            }
        });
    if let Err(error) = spawned {
        aterm_log::warn!("network moved: the browse source did not start: {error}");
    }
}

/// Browse `service` on `local.` for good, each event reaching `watch`
/// ([`aterm_secrets::discover::Watch::hear`]), a fresh connection [`BROWSE_AGAIN`] after one
/// that failed; it returns once a browse ended with no error.
#[cfg(all(not(test), target_os = "macos"))]
fn browse_until_gone(service: &std::ffi::CStr, watch: &aterm_secrets::discover::Watch) {
    use std::ops::ControlFlow;
    let mut said = false;
    loop {
        watch.began_browse();
        let result = aterm_net::bonjour::browse(service, c"local.", |event| {
            if let Ok(name) = event.name.to_str() {
                watch.hear(event.added, event.interface, name);
            }
            ControlFlow::Continue(())
        });
        match result {
            Ok(()) => return,
            Err(error) if !said => {
                said = true;
                aterm_log::warn!("network moved: the _alab._tcp browse ended: {error}");
            }
            Err(_) => {}
        }
        std::thread::sleep(BROWSE_AGAIN);
    }
}

/// How long a source rests after it told a move before it reads again: what it hears meanwhile
/// (the kernel's socket, the browse's queue of rows and the Commons peers keep it) is one more
/// move after the rest, so a flood of browse rows or beacons wakes each consumer at most once a
/// rest.
const REST: Duration = Duration::from_secs(30);

/// Start thread `name`, which tells `moved` of `source` each time `heard` (which blocks until
/// its source says something) says the network moved, then rests `rest`; it ends once `heard`
/// says its source is gone (`None`). Whether it started.
pub(crate) fn spawn_source(
    name: &str,
    moved: Arc<Moved>,
    (source, rest): (Woke, Duration),
    mut heard: impl FnMut() -> Option<bool> + Send + 'static,
) -> bool {
    let spawned = std::thread::Builder::new()
        .name(name.to_owned())
        .spawn(move || {
            crate::qos::set_self(crate::qos::Role::Background);
            while let Some(moved_now) = heard() {
                if moved_now {
                    moved.tell(source);
                    std::thread::sleep(rest);
                }
            }
        });
    if let Err(error) = &spawned {
        aterm_log::warn!("network moved: the {name} source did not start: {error}");
    }
    spawned.is_ok()
}

/// The route source: the kernel's route socket, each change of address or link a move.
#[cfg(all(not(test), any(target_os = "macos", target_os = "linux")))]
fn hear_routes(moved: &Arc<Moved>) {
    let Some(changes) = aterm_uds::network::RouteChanges::open() else {
        aterm_log::warn!(
            "network moved: the kernel gave no route socket, so a change of network is heard \
             only by the browse and the Commons beacon"
        );
        return;
    };
    spawn_source(
        "aterm-moved-routes",
        Arc::clone(moved),
        (Woke::Route, REST),
        move || Some(changes.wait(LONG)),
    );
}

/// How long a peer the Commons runtime heard must go unheard for its return to be a move. A live
/// LAN's peers drop out of Reachable and back about every half minute (an authenticated contact
/// keeps one Reachable 15 s, and the runtime contacts its peers a few at a time), so this sits
/// well above that: a Mac back from away is a move, a live peer's blip is none.
const SILENCE: Duration = Duration::from_secs(2 * 60);

/// What the Commons runtime has told of its peers: when each heard (reachable) within the last
/// [`SILENCE`] was last heard, and how many looks remain of the baseline.
#[derive(Debug)]
pub(crate) struct Heard {
    heard: std::collections::BTreeMap<[u8; 16], Instant>,
    baseline: usize,
}

impl Heard {
    /// A watch whose first `baseline` looks only learn who is heard.
    pub(crate) const fn new(baseline: usize) -> Self {
        Self {
            heard: std::collections::BTreeMap::new(),
            baseline,
        }
    }

    /// One look at the peers now ([`Heard::look_at`]).
    pub(crate) fn look(&mut self, peers: &[aterm_types::network_state::PeerObservation]) -> bool {
        self.look_at(peers, Instant::now())
    }

    /// One look at the peers at `now`: whether one is heard that this watch has not heard
    /// within [`SILENCE`] (a peer back after a silence, or a new one), once the baseline is
    /// over. It forgets each peer unheard for longer, so it holds only those heard lately.
    pub(crate) fn look_at(
        &mut self,
        peers: &[aterm_types::network_state::PeerObservation],
        now: Instant,
    ) -> bool {
        use aterm_types::network_state::Reachability;
        self.heard
            .retain(|_, at| now.saturating_duration_since(*at) <= SILENCE);
        let mut new = false;
        for peer in peers
            .iter()
            .filter(|peer| peer.reachability == Reachability::Reachable)
        {
            new |= self.heard.insert(peer.identity.machine_id, now).is_none();
        }
        if self.baseline > 0 {
            self.baseline -= 1;
            return false;
        }
        new
    }
}

/// How often the beacon source reads the Commons runtime's peers.
const BEACON_LOOK: Duration = Duration::from_secs(2);

/// Hear this process's Commons runtime (`read`: its peers as it observes them now): a peer heard
/// again after a silence is a move ([`Heard`]). Once per process; none in a test build.
pub fn hear_commons(
    read: impl Fn() -> Vec<aterm_types::network_state::PeerObservation> + Send + 'static,
) {
    static STARTED: OnceLock<()> = OnceLock::new();
    if cfg!(test) || STARTED.set(()).is_err() {
        return;
    }
    let mut heard = Heard::new(SETTLE_LOOKS);
    spawn_source(
        "aterm-moved-commons",
        Arc::clone(process()),
        (Woke::Beacon, REST),
        move || {
            std::thread::sleep(BEACON_LOOK);
            Some(heard.look(&read()))
        },
    );
}

/// The beacon source's baseline in looks, half a minute of [`BEACON_LOOK`]s: who the runtime
/// hears then was there before this process looked, so it is no move.
const SETTLE_LOOKS: usize = 15;

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// One minute: a wait for something that must happen.
    pub(crate) const HANG: Duration = Duration::from_secs(60);

    /// The `_alab._tcp` browse is a Local Network use, so it starts only once this process may
    /// use the LAN: while System State's Local Network step is still to come it waits, woken by
    /// a move (the step's question) or its timer, and reads again; once the step asked, or where
    /// nothing waits on a step, it browses at once. NEGATIVE CONTROL: a seam that browses before
    /// `may_use` says yes browses while the step is still to come.
    #[cfg(target_os = "macos")]
    #[test]
    fn the_browse_starts_only_once_the_local_network_step_asked() {
        use std::cell::Cell;
        let (asked, waited, browsed_at) = (Cell::new(0), Cell::new(0), Cell::new(None));
        let to_come_twice = || {
            asked.set(asked.get() + 1);
            asked.get() > 2
        };
        assert!(browse_after_the_step(
            to_come_twice,
            || waited.set(waited.get() + 1),
            || browsed_at.set(Some(asked.get())),
        ));
        assert_eq!(
            waited.get(),
            2,
            "it waited while the step was still to come"
        );
        assert_eq!(
            browsed_at.get(),
            Some(3),
            "it browsed once, only after may_use said yes"
        );

        let browsed = Cell::new(0);
        assert!(!browse_after_the_step(
            || true,
            || panic!("nothing to wait for"),
            || browsed.set(browsed.get() + 1),
        ));
        assert_eq!(browsed.get(), 1, "no step to come: it browses at once");
    }

    /// A move told wakes every watch once, with its source; a watch made after it was not told
    /// of it, and a watch with nothing told waits out its time.
    /// NEGATIVE CONTROL: a `tell` that wakes no watch fails the first wait (it reads `Timer`).
    #[test]
    fn a_move_wakes_each_watch_once_with_its_source() {
        let moved = Arc::new(Moved::new());
        let (mut one, mut two) = (moved.watch(), moved.watch());
        let teller = Arc::clone(&moved);
        let told = std::thread::spawn(move || teller.tell(Woke::Route));
        assert_eq!(one.wait(HANG), Woke::Route);
        told.join().expect("teller");
        assert_eq!(two.take(), Some(Woke::Route));
        assert_eq!(one.take(), None, "told once");
        assert_eq!(moved.watch().take(), None, "a later watch was not told");
        assert_eq!(one.wait(Duration::from_millis(1)), Woke::Timer);
    }

    /// A PER-MAC SPREAD AFTER A MOVE (docs/DESIGN-fluid-swarm-2026-10-28.md, F4's follow-up): on
    /// a signal spread by 3 s, a move is due 3 s after it was told, never before; a second move
    /// told before the first was heard is one wake, at the later one's time; macOS's Local
    /// Network answer, this Mac's own, is due at once; a wait shorter than the spread reads
    /// `Timer` and leaves the move to a later one, and a wait past it is woken by it.
    /// NEGATIVE CONTROL: a `Moved` that ignores its spread (each move due at once) fails "not
    /// before its spread".
    #[test]
    fn a_move_is_due_once_this_macs_spread_has_passed() {
        let secs = Duration::from_secs;
        let spread = secs(3);
        let moved = Arc::new(Moved::spread_by(spread));
        let mut watch = moved.watch();
        let t0 = Instant::now() + secs(3600);
        moved.tell_at(Woke::Route, t0);
        assert_eq!(watch.take_at(t0), None, "not before its spread");
        assert_eq!(
            watch.take_at(t0 + spread - Duration::from_millis(1)),
            None,
            "not before its spread"
        );
        assert_eq!(watch.take_at(t0 + spread), Some(Woke::Route));
        assert_eq!(watch.take_at(t0 + spread), None, "told once");

        moved.tell_at(Woke::Route, t0 + secs(10));
        moved.tell_at(Woke::Beacon, t0 + secs(11));
        assert_eq!(
            watch.take_at(t0 + secs(13)),
            None,
            "the later move's spread"
        );
        assert_eq!(watch.take_at(t0 + secs(14)), Some(Woke::Beacon));
        assert_eq!(watch.take_at(t0 + secs(60)), None, "one wake for both");

        moved.tell_at(Woke::LocalNetwork, t0 + secs(100));
        assert_eq!(
            watch.take_at(t0 + secs(100)),
            Some(Woke::LocalNetwork),
            "this Mac's own answer, at once"
        );

        let hour = Arc::new(Moved::spread_by(secs(3600)));
        let mut waiting = hour.watch();
        hour.tell(Woke::Route);
        assert_eq!(
            waiting.wait(Duration::from_millis(1)),
            Woke::Timer,
            "a wait shorter than the spread"
        );
        assert_eq!(
            waiting.take_at(Instant::now() + secs(3600)),
            Some(Woke::Route),
            "the move is left to a later wait"
        );
        let soon = Arc::new(Moved::spread_by(Duration::from_millis(5)));
        let mut waiting = soon.watch();
        soon.tell(Woke::Beacon);
        assert_eq!(waiting.wait(HANG), Woke::Beacon, "woken once it is due");
    }

    /// THE SPREAD IS THE HERD'S (F4's follow-up review): a Mac whose fleet holds no other member
    /// (in no fleet, or alone in one) hears each move at once, since no other Mac dials with it; a
    /// fleet of 5 other members spreads over 5/35ths of this Mac's offset; a herd of 35 or more
    /// over all of it. A watch made at once (the API reach probe's) hears a move at once on a
    /// spread signal, while a plain watch on it waits the spread out.
    /// NEGATIVE CONTROL: a `herded` that keeps the whole offset whatever the herd fails "a Mac
    /// alone"; a watch that waits the spread out whatever it was made as fails "at once".
    #[test]
    fn a_mac_with_no_herd_hears_a_move_at_once_and_a_herd_spreads_its_share() {
        let secs = Duration::from_secs;
        let t0 = Instant::now() + secs(3600);
        let alone = Arc::new(Moved::herded_by(secs(7), || 0));
        let mut watch = alone.watch();
        alone.tell_at(Woke::Route, t0);
        assert_eq!(watch.take_at(t0), Some(Woke::Route), "a Mac alone");

        let five = Arc::new(Moved::herded_by(secs(7), || 5));
        let mut watch = five.watch();
        five.tell_at(Woke::Route, t0);
        assert_eq!(
            watch.take_at(t0 + Duration::from_millis(999)),
            None,
            "5/35ths of 7 s is 1 s"
        );
        assert_eq!(watch.take_at(t0 + secs(1)), Some(Woke::Route));

        assert_eq!(herded(secs(7), 35), secs(7), "a whole herd");
        assert_eq!(herded(secs(7), 200), secs(7), "past a herd, the whole");
        assert_eq!(herded(secs(7), 0), Duration::ZERO);

        let spread = Arc::new(Moved::spread_by(secs(3)));
        let (mut plain, mut at_once) = (spread.watch(), spread.watch_at_once());
        spread.tell_at(Woke::Route, t0);
        assert_eq!(at_once.take_at(t0), Some(Woke::Route), "at once");
        assert_eq!(
            plain.take_at(t0),
            None,
            "a plain watch waits the spread out"
        );
        assert_eq!(plain.take_at(t0 + secs(3)), Some(Woke::Route));
        let mut waiting = spread.watch_at_once();
        spread.tell_at(Woke::Beacon, Instant::now() + secs(3600));
        assert_eq!(waiting.wait(HANG), Woke::Beacon, "at once, waiting");
    }

    /// THIS MAC'S SPREAD: derived from its node id, else its host name; the same each time for
    /// one Mac, inside the window, and apart for different Macs, so 35 Macs back on one LAN fill
    /// the window rather than one instant; a Mac with neither has none.
    /// NEGATIVE CONTROL: a constant spread fails "35 Macs land apart".
    #[test]
    fn each_mac_spreads_its_wakes_by_an_offset_of_its_own() {
        let node = "3fa2c1d09e8b7a6f5e4d3c2b1a090807".to_owned();
        assert_eq!(
            spread_seed(|| Some(node.clone()), || Some("m22.local".to_owned())).as_deref(),
            Some(node.as_str()),
            "the node id wins"
        );
        assert_eq!(
            spread_seed(|| None, || Some("m22.local".to_owned())).as_deref(),
            Some("m22.local"),
            "else the host name"
        );
        assert_eq!(
            spread_seed(|| Some(String::new()), || Some("m22.local".to_owned())).as_deref(),
            Some("m22.local"),
            "an empty node id is none"
        );
        assert_eq!(spread_seed(|| None, || None), None);
        assert_eq!(spread_of(None), Duration::ZERO, "no seed, no spread");
        assert_eq!(
            spread_of(Some(&node)),
            spread_of(Some(&node)),
            "the same each time"
        );
        let macs: Vec<String> = (1..=35).map(|n| format!("m{n}.local")).collect();
        let offsets: Vec<Duration> = macs.iter().map(|mac| spread_of(Some(mac))).collect();
        assert!(offsets.iter().all(|at| *at < SPREAD), "{offsets:?}");
        let distinct: std::collections::BTreeSet<_> = offsets.iter().collect();
        let seconds: std::collections::BTreeSet<_> =
            offsets.iter().map(Duration::as_secs).collect();
        assert!(
            distinct.len() >= 30 && seconds.len() >= 6,
            "35 Macs land apart: {} distinct offsets in {} of the window's {} seconds: {offsets:?}",
            distinct.len(),
            seconds.len(),
            SPREAD.as_secs()
        );
    }

    /// The window's Local Network answer reaches every watch of this process's signal, the one
    /// the converge lane's live wait holds (`converge_lane::spawn_lane`), so the lane's next pass
    /// runs now rather than at its half hour. No source starts in a test build, so nothing else
    /// tells this signal here.
    /// NEGATIVE CONTROL: a `local_network_moved` that tells a signal of its own, or none, leaves
    /// the process's watch to its hang detector (`Timer`).
    #[test]
    fn the_local_network_answer_wakes_this_processs_watches() {
        let mut watch = watch();
        let teller = std::thread::spawn(local_network_moved);
        assert_eq!(watch.wait(HANG), Woke::LocalNetwork);
        teller.join().expect("teller");
    }

    /// A source thread tells a move each time its source hears one, and nothing for what it
    /// hears that is none; it ends when its source does.
    /// NEGATIVE CONTROL: a source thread that drops what it heard fails the wait (`Timer`).
    #[test]
    fn a_source_tells_each_move_it_hears() {
        let moved = Arc::new(Moved::new());
        let mut watch = moved.watch();
        let (tx, rx) = std::sync::mpsc::channel::<bool>();
        assert!(spawn_source(
            "test-source",
            Arc::clone(&moved),
            (Woke::Beacon, Duration::ZERO),
            move || rx.recv().ok()
        ));
        tx.send(false).expect("source");
        tx.send(true).expect("source");
        assert_eq!(watch.wait(HANG), Woke::Beacon);
        drop(tx);
    }

    /// The browse's first settle window is its baseline, no move; after it, a member not held is,
    /// one held is not (nor one heard on another interface), one back on an interface within
    /// [`aterm_secrets::discover::REREGISTERING_WITHIN`] of leaving it is re-registering and no
    /// move (a peer's window opening restarts its node's registration under the same name), and
    /// one that left (its last interface) and came back after that is a move again. The silence
    /// runs at its real length, on injected times.
    /// NEGATIVE CONTROL: a watch that ignores its settle fails "baseline"; one that keeps no
    /// silence fails "re-registering".
    #[test]
    #[cfg(unix)]
    fn a_member_the_browse_did_not_hold_is_a_move() {
        let secs = Duration::from_secs;
        let silence = aterm_secrets::discover::REREGISTERING_WITHIN;
        let bell = Arc::new(aterm_secrets::wake::Bell::new());
        let browse = aterm_secrets::discover::Watch::idle_for(
            aterm_agent::node_service::ALAB_TCP_SERVICE,
            &bell,
            &[],
        );
        let rang = || bell.nap(Duration::ZERO) == aterm_secrets::wake::Wake::Browse;
        let start = Instant::now();
        browse.began_browse();
        browse.hear_at(true, 4, "a1b2", start);
        assert!(!rang(), "baseline");
        browse.child_began(secs(3600));
        browse.hear_at(true, 4, "", start);
        assert!(!rang(), "no name");
        browse.hear_at(true, 4, "a1b2", start);
        assert!(!rang(), "held");
        browse.hear_at(true, 4, "Desk A", start);
        assert!(rang(), "a new member");
        browse.hear_at(true, 9, "Desk A", start);
        assert!(!rang(), "another interface");
        browse.hear_at(false, 4, "Desk A", start);
        browse.hear_at(true, 4, "Desk A", start);
        assert!(!rang(), "still on 9");
        let gone = start + secs(10);
        browse.hear_at(false, 4, "Desk A", gone);
        browse.hear_at(false, 9, "Desk A", gone);
        assert!(!rang(), "gone");
        browse.hear_at(true, 9, "Desk A", gone + silence);
        assert!(!rang(), "re-registering");
        let left = gone + silence + secs(1);
        browse.hear_at(false, 9, "Desk A", left);
        browse.hear_at(true, 9, "Desk A", left + silence + Duration::from_millis(1));
        assert!(rang(), "back after a silence");
    }

    /// One rejoin heard on the browse is one move: when this Mac comes back after longer than
    /// [`aterm_secrets::discover::REREGISTERING_WITHIN`], every member's `Add` comes at once (none
    /// is held since its `Rmv` as the Mac left), and the browse's bell keeps one ring for all of
    /// them, so the source tells them as one move, as the route source and the Commons beacon
    /// tell theirs, not one move a rest each for a quarter of an hour.
    /// NEGATIVE CONTROL: a nap that leaves the bell rung tells a move each time it is asked.
    #[test]
    #[cfg(unix)]
    fn one_rejoin_heard_on_the_browse_is_one_move() {
        let bell = Arc::new(aterm_secrets::wake::Bell::new());
        let browse = aterm_secrets::discover::Watch::idle_for(
            aterm_agent::node_service::ALAB_TCP_SERVICE,
            &bell,
            &[],
        );
        let start = Instant::now();
        let left = start + Duration::from_secs(30);
        let back = left + aterm_secrets::discover::REREGISTERING_WITHIN + Duration::from_secs(1);
        for (add, at) in [(true, start), (false, left), (true, back)] {
            for i in 0..34 {
                browse.hear_at(add, 4, &format!("member-{i:02}"), at);
            }
        }
        let moved = Arc::new(Moved::new());
        let mut watch = moved.watch();
        let (ended, end) = std::sync::mpsc::channel();
        let mut asked = 0;
        assert!(spawn_source(
            "test-browse",
            Arc::clone(&moved),
            (Woke::Browse, Duration::ZERO),
            move || {
                asked += 1;
                let rung = bell.nap(Duration::ZERO) == aterm_secrets::wake::Wake::Browse;
                if !rung || asked > 100 {
                    let _ = ended.send(());
                    return None;
                }
                Some(rung)
            }
        ));
        end.recv_timeout(HANG)
            .expect("the source ends once the bell is quiet");
        assert_eq!(watch.take(), Some(Woke::Browse));
        assert_eq!(moved.told(), 1, "one move for the whole rejoin");
        drop(browse);
    }

    /// A Commons peer heard again after a silence longer than [`SILENCE`], or one new, is a
    /// move once the baseline is over; one heard within it, or one gone silent, is not.
    /// NEGATIVE CONTROL: a look that takes every heard peer as new fails the second look; one
    /// that never forgets a peer misses its return after the silence.
    #[test]
    fn a_peer_heard_again_after_a_silence_is_a_move() {
        use aterm_types::network_state::Reachability::{LastSeen, Reachable};
        let at = Instant::now();
        let after = |secs| at + Duration::from_secs(secs);
        let mut heard = Heard::new(1);
        assert!(!heard.look_at(&[peer(1, Reachable)], at), "baseline");
        assert!(
            !heard.look_at(&[peer(1, Reachable)], after(2)),
            "still heard"
        );
        assert!(!heard.look_at(&[peer(1, LastSeen)], after(4)), "silent");
        let back = 2 + SILENCE.as_secs() + 1;
        assert!(
            !heard.look_at(&[peer(1, LastSeen)], after(back - 2)),
            "still silent"
        );
        assert!(heard.look_at(&[peer(1, Reachable)], after(back)), "back");
        assert!(
            heard.look_at(&[peer(1, Reachable), peer(2, Reachable)], after(back + 2)),
            "new"
        );
        assert!(
            !heard.look_at(&[peer(1, Reachable), peer(2, Reachable)], after(back + 4)),
            "both heard"
        );
    }

    /// A live LAN's peers drop out of Reachable and back about every half minute (an
    /// authenticated contact keeps one Reachable 15 s): such a blip is no move, however often it
    /// comes, so the source does not fire after each of its rests for as long as the LAN is up.
    /// NEGATIVE CONTROL: a look that compares with the last look only (the peer was LastSeen
    /// there) takes each blip's return as a move.
    #[test]
    fn a_peer_back_from_a_short_blip_is_no_move() {
        use aterm_types::network_state::Reachability::{LastSeen, Reachable};
        let mut heard = Heard::new(1);
        assert!(!heard.look(&[peer(1, Reachable)]), "baseline");
        for blip in 0..16 {
            assert!(!heard.look(&[peer(1, LastSeen)]), "silent, blip {blip}");
            assert!(!heard.look(&[peer(1, Reachable)]), "back, blip {blip}");
        }
    }

    /// A peer with one byte of machine id, as the Commons runtime observes it.
    pub(crate) fn peer(
        id: u8,
        reachability: aterm_types::network_state::Reachability,
    ) -> aterm_types::network_state::PeerObservation {
        aterm_types::network_state::PeerObservation {
            slot: u64::from(id),
            identity: aterm_types::network_state::MachineIdentity {
                machine_id: [id; 16],
                incarnation: [0; 16],
                label: None,
            },
            first_seen_ms: 1,
            last_seen_ms: 1,
            last_checked_ms: None,
            reachability,
            instances: None,
            samples: Vec::new(),
        }
    }
}
