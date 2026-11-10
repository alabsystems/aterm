// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THIS PROCESS'S "NETWORK MOVED" SIGNAL (docs/DESIGN-fluid-swarm-2026-10-28.md §3.5.2, F3c).
//! Nothing carries the signal between processes, so the window and the node job each watch
//! their own sources: the kernel's network-change socket ([`aterm_uds::network::RouteChanges`]), the one
//! `_alab._tcp` browse a process holds, through the root LAN broker, which tells a member heard
//! after every member's silence of [`aterm_secrets::discover::SILENCE`] and resolves what it
//! holds to dial hints ([`hear_alab_tcp`]: the node job's,
//! and a window's only while it serves the fleet port, §4.1 F17), and the Commons runtime's peers, the first heard after every
//! peer's silence of [`SILENCE`], or one heard for the first time ([`Heard`]). Each consumer (the window's converge lane, a fleet heartbeat, the sign-in
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
    /// The kernel told of a change to this Mac's addresses or links (macOS's and Linux's source;
    /// every platform's watch tells a route move it has not heard as one: [`Told::heard_since`]).
    Route,
    /// The process's `_alab._tcp` browse heard a member after every member's silence
    /// ([`aterm_secrets::discover::SILENCE`], [`hear_alab_tcp`]).
    #[cfg_attr(
        not(any(target_os = "macos", test)),
        expect(dead_code, reason = "the browse source is macOS's")
    )]
    Browse,
    /// The Commons runtime heard a peer after every peer's silence ([`SILENCE`]), or a new one.
    Beacon,
    /// The network place settled ([`hear_place`], `aterm_netplace::moves`): the keeper read the
    /// new network, so it is usable now, told behind a route move's with only the flap rest
    /// (`aterm_netplace::moves::REST_MS`). The live source runs on macOS and Linux;
    /// tests inject it on every platform.
    #[cfg_attr(
        not(any(target_os = "macos", target_os = "linux", test)),
        expect(
            dead_code,
            reason = "the place source rides the route socket, macOS's and Linux's"
        )
    )]
    Place,
}

impl Woke {
    /// Its words in an aterm.log line.
    pub(crate) const fn words(self) -> &'static str {
        match self {
            Self::Timer => "its timer",
            Self::Route => "a route change",
            Self::Browse => "a member heard on the _alab._tcp browse after two minutes of none",
            Self::Beacon => "a Commons beacon after two minutes of no peer, or from a new one",
            Self::Place => "the network place settling onto a network",
        }
    }
}

/// The longest a consumer that waits only for moves waits at once; it waits again after.
pub(crate) const LONG: Duration = Duration::from_secs(24 * 60 * 60);

/// One process's signal: how many moves were told, the source of the last, and when its watches
/// are due to hear it (`None`: at once), and which of them was the last route move
/// ([`Told::route`]).
#[derive(Debug)]
pub(crate) struct Moved {
    told: Mutex<Told>,
    changed: Condvar,
    /// How long after a move its watches hear it, where this Mac shares its moves with a whole
    /// herd ([`HERD`]): this Mac's spread ([`spread_of`]).
    spread: Duration,
    /// How many other Macs share this one's moves, read at each move ([`members_here`]): the
    /// spread is the share of [`Moved::spread`] that herd makes ([`herded`]).
    herd: fn() -> usize,
}

/// What a [`Moved`] was told.
#[derive(Debug)]
struct Told {
    /// How many moves were told.
    count: u64,
    /// The source of the last.
    source: Woke,
    /// When its watches are due to hear it (`None`: at once).
    due: Option<Instant>,
    /// Which move (its count) was the last route move: a watch that has not heard it is told a
    /// route move, whatever moved after it within the spread (a browse row, a beacon), since
    /// several moves told meanwhile are one wake and a route move is the one a consumer that
    /// looks again only on a route move (the API reach probe) must not miss.
    route: u64,
    /// A native wait generation, closed on the next move. Every waiter keeps its own Arc,
    /// so one consumer cannot drain another's notification.
    #[cfg(unix)]
    wake: Option<Arc<aterm_uds::stop::Stop>>,
}

impl Told {
    /// Arm while holding the same mutex as the event predicate.
    #[cfg(unix)]
    fn wake(&mut self) -> Arc<aterm_uds::stop::Stop> {
        Arc::clone(
            self.wake
                .get_or_insert_with(|| Arc::new(aterm_uds::stop::Stop::new())),
        )
    }

    /// What a watch that heard up to move `seen` is told now: the route move where one came
    /// since, else the last move's source.
    const fn heard_since(&self, seen: u64) -> Woke {
        if self.route > seen {
            Woke::Route
        } else {
            self.source
        }
    }
}

impl Moved {
    /// A signal no source has told yet, whose watches hear each move at once.
    #[cfg(test)]
    pub(crate) const fn new() -> Self {
        Self::spread_by(Duration::ZERO)
    }

    /// A signal no source has told yet, whose watches hear each move `spread` after it was told:
    /// a signal whose Mac shares its moves with a whole herd.
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
    /// told ([`herded`]).
    pub(crate) const fn herded_by(spread: Duration, herd: fn() -> usize) -> Self {
        Self {
            told: Mutex::new(Told {
                count: 0,
                source: Woke::Timer,
                due: None,
                route: 0,
                #[cfg(unix)]
                wake: None,
            }),
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
    /// one wake with it, at the later one's time, told as a route move where one of them was
    /// ([`Told::route`]).
    pub(crate) fn tell_at(&self, source: Woke, at: Instant) {
        let spread = if self.spread.is_zero() {
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
        told.count = told.count.wrapping_add(1);
        told.source = source;
        told.due = due;
        if source == Woke::Route {
            told.route = told.count;
        }
        #[cfg(unix)]
        if let Some(wake) = told.wake.take() {
            wake.set();
        }
        drop(told);
        self.changed.notify_all();
    }

    /// How many moves were told.
    #[cfg(all(test, unix))]
    pub(crate) fn told(&self) -> u64 {
        self.told
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .count
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
        let seen = self
            .told
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .count;
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
    /// Wait for this watch's next due move, either stop, or the original absolute deadline.
    /// A native wake only prompts another predicate check; it supplies no admission authority.
    /// `None` means a stop, and a deadline leaves a move whose spread is not due queued.
    ///
    /// # Errors
    /// A native bell or readiness wait failed. The caller retains its timed fallback.
    #[cfg(unix)]
    pub(crate) fn wait_until(
        &mut self,
        stops: &[&aterm_uds::stop::Stop],
        deadline: Option<Instant>,
    ) -> std::io::Result<Option<Woke>> {
        loop {
            if stops.iter().any(|stop| stop.is_set()) {
                return Ok(None);
            }
            let (wake, until) = {
                let mut told = self
                    .moved
                    .told
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner);
                let now = Instant::now();
                let unheard = told.count != self.seen;
                let due = told.due.filter(|_| self.spread && unheard);
                if unheard && due.is_none_or(|due| now >= due) {
                    let woke = told.heard_since(self.seen);
                    self.seen = told.count;
                    return Ok(Some(woke));
                }
                if deadline.is_some_and(|at| now >= at) {
                    return Ok(Some(Woke::Timer));
                }
                let until = match (deadline, due) {
                    (Some(a), Some(b)) => Some(a.min(b)),
                    (a, b) => a.or(b),
                };
                (told.wake(), until)
            };
            let bells = stops
                .iter()
                .map(|stop| stop.bell())
                .collect::<std::io::Result<Vec<_>>>()?;
            wake.wait_ready(&bells, until)?;
        }
    }

    /// Wait at most `most` for a move this watch was not told of, once it is due (this Mac's
    /// spread after it was told): the source of the latest one (several told meanwhile are one
    /// wake), a route move where one of them was ([`Told::heard_since`]), else [`Woke::Timer`],
    /// which leaves a move not yet due to a later wait.
    pub(crate) fn wait(&mut self, most: Duration) -> Woke {
        let deadline = Instant::now().checked_add(most);
        let mut told = self
            .moved
            .told
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        loop {
            let now = Instant::now();
            let unheard = told.count != self.seen;
            let until_due = told
                .due
                .filter(|_| self.spread)
                .map_or(Duration::ZERO, |due| due.saturating_duration_since(now));
            if unheard && until_due.is_zero() {
                let woke = told.heard_since(self.seen);
                self.seen = told.count;
                return woke;
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
        (told.count != self.seen && told.due.is_none_or(|due| !self.spread || now >= due)).then(
            || {
                let woke = told.heard_since(self.seen);
                self.seen = told.count;
                woke
            },
        )
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
        #[cfg(all(any(target_os = "macos", target_os = "linux"), not(test)))]
        aterm_update::set_network_change_probe(updater_probe(&moved));
        start_sources(&moved);
        moved
    })
}

/// Start this process's network observation for its automatic updater even
/// when no fleet, account, or converge consumer has requested a watch yet.
#[cfg(any(target_os = "macos", target_os = "linux"))]
pub(crate) fn observe_for_updater() {
    if aterm_update::automatic() {
        let _ = process();
    }
}

/// Use a separate consuming watch so an updater check cannot steal another
/// network consumer's notification. Keep the fleet's settling/spread policy.
#[cfg(any(target_os = "macos", target_os = "linux", test))]
fn updater_probe(moved: &Arc<Moved>) -> Box<dyn Fn() -> bool + Send + Sync> {
    let watch = Mutex::new(moved.watch());
    Box::new(move || {
        watch
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take_at(Instant::now())
            .is_some()
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

/// The herd a move told at `now_ms` waits out: the `book`'s other members, or none where the
/// network place's record says this Mac is returning alone from a remote stay
/// (`aterm_netplace::Record::lone_return`): only it moved, so it has no herd to stagger against.
/// A router or access point reboot, which moves every Mac from the ALab LAN through offline back
/// onto it, keeps the herd; so does a Mac with no record. Off Unix no fleet book is read, so
/// only the tests ask.
#[cfg(any(test, unix))]
fn herd_given(book: usize, record: Option<&aterm_netplace::Record>, now_ms: u64) -> usize {
    if record.is_some_and(|record| record.lone_return(now_ms)) {
        0
    } else {
        book
    }
}

/// How many other members share this Mac's moves: the peers its fleet book holds and its copy of
/// its issuer's book, each once ([`aterm_net::fleet_member::answer_key`]), this Mac's own node
/// aside; none in no fleet, and none for a Mac returning alone ([`herd_given`]). Read at each
/// move, so a join or a leave counts from the next one. The API reach probe's watch waits none
/// ([`Moved::watch_at_once`]).
#[cfg(all(not(test), unix))]
fn members_here() -> usize {
    use aterm_net::fleet_member as member;
    let Some(state) = aterm_types::dirs::state_dir() else {
        return 0;
    };
    let own = member::read_node_id(&state).ok().flatten();
    let book = member::read_peers(&state).unwrap_or_default();
    let others = others_in_book(own.as_deref(), &book, |issuer| {
        member::read_issuer_book(&state, issuer)
    });
    let record = aterm_netplace::record::read(&aterm_netplace::record::dir(&state));
    herd_given(
        others,
        record.as_ref().map(|written| &written.record),
        aterm_netplace::unix_ms_now(),
    )
}

/// The other members [`members_here`] counts for the Mac whose node is `own` and whose fleet book
/// is `book`: the book's peers and `copies_of(issuer)`, its copy of its issuer's book (read only
/// where the book names an issuer with a node), each once by its
/// [`aterm_net::fleet_member::answer_key`], `own` aside.
#[cfg(unix)]
fn others_in_book(
    own: Option<&str>,
    book: &[aterm_net::fleet_member::Peer],
    copies_of: impl FnOnce(&str) -> Vec<aterm_net::fleet_member::Peer>,
) -> usize {
    use aterm_net::fleet_member as member;
    let copies = book
        .iter()
        .find(|peer| peer.via == member::Via::Issuer)
        .and_then(|issuer| issuer.node.as_deref())
        .map(copies_of)
        .unwrap_or_default();
    book.iter()
        .chain(&copies)
        .filter(|peer| peer.node.is_none() || peer.node.as_deref() != own)
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

/// Whether aterm's own LAN traffic can go out now: on macOS a hello is answered
/// ([`aterm_uds::lan::broker::hello`]) by the root LAN broker or, while none is installed, by the
/// allowed direct path ([`aterm_uds::lan::direct`]), the only ways out with no alert; elsewhere
/// always, as the LAN is used in-process there. A LAN lane that waits for it waits for the
/// broker's socket, never for a person.
pub(crate) fn lan_answers() -> bool {
    lan_unavailable().is_none()
}

/// Why aterm's own LAN traffic cannot go out now, in the broker's own words (its error's
/// `Display`, [`aterm_uds::lan::LanError::Unavailable`] where no root broker serves and no
/// direct path does); `None` once a hello is answered, and always off macOS ([`lan_answers`]). A
/// process under the loopback guard ([`aterm_net::loopback::confined`], a debug build's isolated
/// fleet world) reaches the loopback alone, which needs no broker, so for it this is always
/// `None` too.
pub(crate) fn lan_unavailable() -> Option<String> {
    #[cfg(target_os = "macos")]
    {
        if aterm_net::loopback::confined() {
            return None;
        }
        aterm_uds::lan::broker::hello()
            .err()
            .map(|error| error.to_string())
    }
    #[cfg(not(target_os = "macos"))]
    {
        None
    }
}

/// Start this process's route source and its one `_alab._tcp` browse ([`hear_alab_tcp`]), so a
/// window hears members come back while the node job holds the fleet port.
fn start_sources(moved: &Arc<Moved>) {
    #[cfg(all(not(test), any(target_os = "macos", target_os = "linux")))]
    hear_routes(moved);
    #[cfg(all(not(test), any(target_os = "macos", target_os = "linux")))]
    if let Some(state) = aterm_types::dirs::state_dir() {
        hear_place(
            moved,
            aterm_netplace::record::dir(&state),
            aterm_uds::network::RouteChanges::open().ok(),
            PLACE_LOOKS,
        );
    }
    hear_alab_tcp_on(moved, &[]);
    let _ = moved;
}

/// This process's one `_alab._tcp` browse, once [`hear_alab_tcp`] started it.
#[cfg(unix)]
static ALAB_TCP: OnceLock<Arc<aterm_secrets::discover::Watch>> = OnceLock::new();

/// Start this process's one `_alab._tcp` browse ([`hear_browse`], read by an
/// [`aterm_secrets::discover::Watch`]), holding `own` (this process's own advert names) as its
/// own: a member heard once every other member was silent for
/// [`aterm_secrets::discover::SILENCE`] is a move ([`Woke::Browse`]), a peer's comings and goings
/// none, and what it holds resolves to dial hints ([`alab_tcp`]). Each process starts it with its
/// sources ([`start_sources`]), so the move detector and the heartbeat share one browse; a later
/// call adds its `own` names. A window names none, so this Mac's node job's advert is a member
/// on its browse; the node job withdraws it while this Mac has no route
/// ([`aterm_agent::node_service::advertises`]), so this Mac off every network for longer than
/// the silence and back is a move there too. None starts in a test build, outside macOS, or
/// under a private state root or CI (`ATERM_STATE_HOME`, `CI`, as the Commons runtime).
#[cfg(unix)]
pub(crate) fn hear_alab_tcp(own: &[&str]) {
    hear_alab_tcp_on(process(), own);
}

/// Whether this process starts its `_alab._tcp` browse at all, `set` saying which environment
/// variables are set: never in a scratch world ([`aterm_types::world::MARKER`], read as a plain
/// variable so a release build honours it too) or under `CI`, as the Commons runtime and the
/// converge runner's window lane ([`crate::converge_lane::runs`]).
#[cfg(any(test, target_os = "macos"))]
pub(crate) fn browse_runs(set: &dyn Fn(&str) -> bool) -> bool {
    !set(aterm_types::world::MARKER) && !set("CI")
}

/// [`hear_alab_tcp`], telling `moved` (the process's signal, which [`start_sources`] is starting
/// before [`process`] holds it) of each move.
fn hear_alab_tcp_on(moved: &Arc<Moved>, own: &[&str]) {
    #[cfg(unix)]
    if aterm_agent::node_service::DiscoveryPolicy::process_discovery().mesh {
        return;
    }
    #[cfg(all(not(test), target_os = "macos"))]
    if browse_runs(&crate::converge_lane::env_set) {
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

/// How long the browse, waiting for sharing to be on ([`keep_browse`]), waits before it looks
/// again with no move told.
#[cfg(all(not(test), target_os = "macos"))]
const OFF_LOOK: Duration = Duration::from_secs(10);

/// How often a running browse looks at `[swarm] enabled` while it hears nothing
/// ([`aterm_uds::lan::Events::next_within`]): one `stat` ([`aterm_secrets::swarm::Switch`]).
#[cfg(all(not(test), target_os = "macos"))]
const SHARING_LOOK: Duration = Duration::from_secs(5);

/// What the browse's keeper says in aterm.log once per change ([`keep_browse`]).
#[cfg(target_os = "macos")]
const BROWSE_SHARING_OFF: &str = "network moved: sharing is off ([swarm] enabled = false), so the \
    _alab._tcp browse is stopped until it is on";

/// What the browse's keeper says once a person turns sharing back on ([`keep_browse`]).
#[cfg(target_os = "macos")]
const BROWSE_SHARING_ON: &str = "network moved: sharing is on again, so the _alab._tcp browse \
    starts again";

/// Keep this process's `_alab._tcp` browse while `[swarm] enabled` leaves sharing on
/// (`sharing`, `docs/DESIGN-alab-swarm-2026-10-27.md` §9): it starts only with sharing on, read
/// again after each `wait`; it runs (`browse`, told to go on only while `sharing` stays on) until
/// sharing goes off, and starts again once a person turns it back on. Each stop and start is said
/// once in `said`. It returns once `browse` says the browse is not to be kept (a test's end).
#[cfg(target_os = "macos")]
fn keep_browse(
    mut sharing: impl FnMut() -> bool,
    mut wait: impl FnMut(),
    mut browse: impl FnMut(&mut dyn FnMut() -> bool) -> bool,
    mut said: impl FnMut(&str),
) {
    let mut off = false;
    loop {
        loop {
            let on = sharing();
            if on == off {
                off = !on;
                said(if on {
                    BROWSE_SHARING_ON
                } else {
                    BROWSE_SHARING_OFF
                });
            }
            if on {
                break;
            }
            wait();
        }
        if !browse(&mut sharing) {
            return;
        }
    }
}

/// The `_alab._tcp` browse `watch` reads: the root LAN broker's ([`aterm_uds::lan::browse`],
/// which macOS never asks about), whose request lives as long as this process holds its events,
/// read on a thread of its own. Each event reaches the watch
/// ([`aterm_secrets::discover::Watch::hear`]), which keeps the baseline, each instance's
/// interfaces and the bound, and rings its bell for a member heard after every member's silence
/// without blocking the reader. A connection that ended starts again after
/// [`BROWSE_AGAIN`], its first events a baseline again.
#[cfg(all(not(test), target_os = "macos"))]
fn hear_browse(watch: &Arc<aterm_secrets::discover::Watch>, moved: &Arc<Moved>) {
    let watch = Arc::clone(watch);
    let mut step = moved.watch();
    let spawned = std::thread::Builder::new()
        .name("aterm-moved-bonjour".to_owned())
        .spawn(move || {
            crate::qos::set_self(crate::qos::Role::Background);
            let mut switch =
                aterm_types::dirs::home_dir().map(|home| aterm_secrets::swarm::Switch::new(&home));
            keep_browse(
                || {
                    switch.as_mut().is_none_or(|switch| {
                        switch.enabled_saying(&|line| aterm_log::warn!("network moved: {line}"))
                            == Ok(true)
                    })
                },
                || {
                    let _ = step.wait(OFF_LOOK);
                },
                |sharing| {
                    browse_until_off(aterm_agent::node_service::ALAB_TCP_SERVICE, &watch, sharing);
                    true
                },
                |line| aterm_log::info!("{line}"),
            );
        });
    if let Err(error) = spawned {
        aterm_log::warn!("network moved: the browse source did not start: {error}");
    }
}

/// Browse `service` on `local.` through the root LAN broker while `sharing` stays on, each event
/// reaching `watch` ([`aterm_secrets::discover::Watch::hear`]), a fresh request [`BROWSE_AGAIN`]
/// after one that failed or found no broker (its words, the broker's, said once); it returns once
/// sharing is off (looked at each [`SHARING_LOOK`] the browse hears nothing, and before each
/// event).
#[cfg(all(not(test), target_os = "macos"))]
fn browse_until_off(
    service: &str,
    watch: &aterm_secrets::discover::Watch,
    sharing: &mut dyn FnMut() -> bool,
) {
    use aterm_uds::lan::Event;
    let mut said = false;
    while sharing() {
        watch.began_browse();
        let ended = match aterm_uds::lan::browse(service, "local.") {
            Ok(mut events) => loop {
                if !sharing() {
                    return;
                }
                match events.next_within(SHARING_LOOK) {
                    Ok(None) => {}
                    Ok(Some(Event::Added(instance))) => {
                        watch.hear(true, instance.interface, &instance.name);
                    }
                    Ok(Some(Event::Removed(instance))) => {
                        watch.hear(false, instance.interface, &instance.name);
                    }
                    Ok(Some(_)) => {}
                    Err(error) => break error,
                }
            },
            Err(error) => error,
        };
        if !said {
            said = true;
            aterm_log::warn!("network moved: the _alab._tcp browse ended: {ended}");
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
    let changes = match aterm_uds::network::RouteChanges::open() {
        Ok(changes) => changes,
        Err(why) => {
            aterm_log::warn!(
                "network moved: the kernel gave no route socket ({why}), so a change of network \
                 is heard only by the browse and the Commons beacon"
            );
            return;
        }
    };
    spawn_source(
        "aterm-moved-routes",
        Arc::clone(moved),
        (Woke::Route, REST),
        move || Some(changes.wait(LONG)),
    );
}

/// How often the place source reads the keeper's record: every second for a minute after a route
/// change (the keeper reads the new network within its settle), once a minute otherwise.
#[cfg(all(not(test), any(target_os = "macos", target_os = "linux")))]
const PLACE_LOOKS: PlaceLooks = PlaceLooks {
    after_a_change: Duration::from_secs(1),
    fast_for: Duration::from_secs(60),
    steady: Duration::from_secs(60),
};

/// The place source's pace (`PLACE_LOOKS`).
#[derive(Clone, Copy, Debug)]
#[cfg_attr(
    not(any(target_os = "macos", target_os = "linux")),
    expect(
        dead_code,
        reason = "the place source rides the route socket, macOS's and Linux's"
    )
)]
pub(crate) struct PlaceLooks {
    /// The look while a route change settles.
    pub(crate) after_a_change: Duration,
    /// How long after a route change it looks that often.
    pub(crate) fast_for: Duration,
    /// The look otherwise.
    pub(crate) steady: Duration,
}

/// THE PLACE SOURCE (`aterm_netplace::moves`): reads the network place's record in `dir` at
/// `looks`' pace, faster for a while after each change `routes` (this source's own route socket)
/// tells, and tells [`Woke::Place`] the moment the place settles onto another network than the
/// one it last told, with no rest: the route source tells a move at its first message (often the
/// old address going away) and rests [`REST`], and this is the move that says the new network is
/// usable. A settle back onto the network last told (a flapping link) is told at most once a
/// rest (`aterm_netplace::moves::REST_MS`), so each consumer's converge pass, probe lease,
/// heartbeat and claims re-read stays bounded.
/// Whether the herd's spread applies is [`herd_given`]'s: a return from away waits none, a router
/// reboot keeps it. Whether its thread started.
#[cfg_attr(
    not(any(target_os = "macos", target_os = "linux", test)),
    expect(
        dead_code,
        reason = "the place source rides the route socket, macOS's and Linux's"
    )
)]
#[cfg(unix)]
pub(crate) fn hear_place(
    moved: &Arc<Moved>,
    dir: std::path::PathBuf,
    routes: Option<aterm_uds::network::RouteChanges>,
    looks: PlaceLooks,
) -> bool {
    // The first look, on the caller's thread: where the place stands as the source starts is no
    // move, and any settle after this returns is one.
    let mut watch = aterm_netplace::moves::SettledWatch::new();
    let _ = watch.look(
        aterm_netplace::record::read(&dir)
            .as_ref()
            .map(|written| &written.record),
        aterm_netplace::unix_ms_now(),
    );
    let mut fast_until: Option<Instant> = None;
    spawn_source(
        "aterm-moved-place",
        Arc::clone(moved),
        (Woke::Place, Duration::ZERO),
        move || {
            let look = if fast_until.is_some_and(|until| Instant::now() < until) {
                looks.after_a_change
            } else {
                looks.steady
            };
            let changed = match &routes {
                Some(routes) => routes.wait(look),
                None => {
                    std::thread::sleep(look);
                    false
                }
            };
            if changed {
                fast_until = Instant::now().checked_add(looks.fast_for);
            }
            let record = aterm_netplace::record::read(&dir);
            Some(watch.look(
                record.as_ref().map(|written| &written.record),
                aterm_netplace::unix_ms_now(),
            ))
        },
    )
}

/// How long every peer the Commons runtime heard must go unheard for the next one heard to be a
/// move: this Mac back from away is a move, one peer's silence is none. It counts awake time
/// ([`Instant`] stops while macOS sleeps), so a wake on a changed network is the route source's
/// to tell. A live LAN's peers drop out of Reachable and back all the time (an authenticated
/// contact keeps one Reachable 15 s, and the runtime contacts its peers a few at a time), and
/// among some 40 Macs one is always back from longer than this: counted a peer at a time, the
/// source fired 110 times an hour where its rest allows 120 (0.147.0, 2026-11-01).
const SILENCE: Duration = Duration::from_secs(2 * 60);

/// What the Commons runtime has told of its peers: when each heard (reachable) within the last
/// [`SILENCE`] was last heard, every peer this watch has heard, and how many looks remain of the
/// baseline.
#[derive(Debug)]
pub(crate) struct Heard {
    heard: std::collections::BTreeMap<[u8; 16], Instant>,
    known: std::collections::BTreeSet<[u8; 16]>,
    baseline: usize,
}

impl Heard {
    /// A watch whose first `baseline` looks only learn who is heard.
    pub(crate) const fn new(baseline: usize) -> Self {
        Self {
            heard: std::collections::BTreeMap::new(),
            known: std::collections::BTreeSet::new(),
            baseline,
        }
    }

    /// The beacon source's watch: its baseline is [`SETTLE_LOOKS`] looks.
    pub(crate) const fn commons() -> Self {
        Self::new(SETTLE_LOOKS)
    }

    /// One look at the peers now ([`Heard::look_at`]).
    #[cfg(test)]
    pub(crate) fn look(&mut self, peers: &[aterm_types::network_state::PeerObservation]) -> bool {
        self.look_at(peers, Instant::now())
    }

    /// An unavailable sample is no look: it neither advances the baseline nor forgets peers.
    fn look_sample_at(
        &mut self,
        peers: Option<&[aterm_types::network_state::PeerObservation]>,
        now: Instant,
    ) -> bool {
        peers.is_some_and(|peers| self.look_at(peers, now))
    }

    /// One look at the peers at `now`: whether, once the baseline is over, one is heard where no
    /// peer was within [`SILENCE`], or one this watch never heard. It forgets when each peer
    /// unheard for longer was heard, so it holds only those heard lately.
    pub(crate) fn look_at(
        &mut self,
        peers: &[aterm_types::network_state::PeerObservation],
        now: Instant,
    ) -> bool {
        use aterm_types::network_state::Reachability;
        self.heard
            .retain(|_, at| now.saturating_duration_since(*at) <= SILENCE);
        let alone = self.heard.is_empty();
        let mut back = false;
        let mut new = false;
        for peer in peers
            .iter()
            .filter(|peer| peer.reachability == Reachability::Reachable)
        {
            back = true;
            new |= self.known.insert(peer.identity.machine_id);
            self.heard.insert(peer.identity.machine_id, now);
        }
        if self.baseline > 0 {
            self.baseline -= 1;
            return false;
        }
        new || (alone && back)
    }
}

/// How often the beacon source reads the Commons runtime's peers.
const BEACON_LOOK: Duration = Duration::from_secs(2);

/// The window's beacon reader: the peers of `read`'s network observation. It reads the network
/// alone ([`crate::NetworkRead::Network`]), since it draws no service and a services read keeps
/// their observer sampling.
pub(crate) fn beacon_peers(
    read: crate::NetworkSnapshotSource,
) -> impl Fn() -> Option<Vec<aterm_types::network_state::PeerObservation>> + Send + 'static {
    move || read(crate::NetworkRead::Network).map(|snapshot| snapshot.network.peers.clone())
}

/// Hear this process's Commons runtime (`read`: its peers as it observes them now, or no sample
/// while unavailable): a peer heard after every peer's silence, or a new one, is a move
/// ([`Heard`]). An unavailable
/// sample leaves the watch unchanged. Once per process; none in a test build.
pub fn hear_commons(
    read: impl Fn() -> Option<Vec<aterm_types::network_state::PeerObservation>> + Send + 'static,
) {
    static STARTED: OnceLock<()> = OnceLock::new();
    if cfg!(test) || STARTED.set(()).is_err() {
        return;
    }
    let mut heard = Heard::commons();
    spawn_source(
        "aterm-moved-commons",
        Arc::clone(process()),
        (Woke::Beacon, REST),
        move || {
            std::thread::sleep(BEACON_LOOK);
            Some(heard.look_sample_at(read().as_deref(), Instant::now()))
        },
    );
}

/// The beacon source's baseline in looks, half a minute of [`BEACON_LOOK`]s: who the runtime
/// hears then was there before this process looked, so it is no move.
const SETTLE_LOOKS: usize = 15;

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    #[test]
    fn updater_gets_its_own_settled_network_changes_without_stealing_other_consumers() {
        let moved = Arc::new(Moved::spread_by(Duration::from_secs(30)));
        let probe = updater_probe(&moved);
        let mut other = moved.watch();
        assert!(!probe());
        let future = Instant::now() + Duration::from_secs(60);
        moved.tell_at(Woke::Route, future);
        assert!(!probe(), "the updater respects the network settling delay");
        moved.tell_at(Woke::Place, Instant::now() - Duration::from_secs(60));
        assert!(probe());
        assert!(!probe(), "one check per settled move");
        assert_eq!(
            other.take(),
            Some(Woke::Route),
            "its pending route was not consumed"
        );
    }

    /// One minute: a wait for something that must happen.
    pub(crate) const HANG: Duration = Duration::from_secs(60);

    /// verify measure, its GUI smoke and the perf arena start aterm in a scratch world
    /// ([`aterm_types::world`]), often a release build that reads no development seam. Its
    /// marker and its lanes-off tables turn off, by the product's own gates, every launch lane
    /// that can reach the local network (a world's use of it would reach the person's real LAN
    /// and its members) or the login's launchd domain: this process's `_alab._tcp` browse, the
    /// converge runner's window lane, the browse's own `[swarm]` switch, fleet reads and the
    /// keeper job, besides the update and package lanes. Each gate is first shown open without the world, so none passes by being
    /// shut anyway.
    /// NEGATIVE CONTROL: a lanes-off text without its `[swarm]` table leaves the browse's switch
    /// and fleet reads on.
    #[test]
    fn a_scratch_world_starts_no_lane_that_reaches_the_local_network() {
        let none = |_: &str| false;
        let world = |name: &str| name == aterm_types::world::MARKER;
        assert!(browse_runs(&none), "the browse runs outside a world");
        assert!(!browse_runs(&world), "the world's marker skips the browse");
        assert_eq!(crate::converge_lane::runs(false, false, &none), cfg!(unix));
        assert!(
            !crate::converge_lane::runs(false, false, &world),
            "the world's marker skips the converge runner's window lane"
        );

        let open = crate::app_config::Config::parse("").expect("an empty config");
        assert!(open.swarm_enabled() && open.fleet_watch_enabled());
        assert_eq!(open.keeper_setting(), None);
        assert!(crate::app_config::update_checks_automatic(&open));
        assert!(open.packages_enabled());
        let empty = aterm_toml::from_str::<aterm_toml::Value>("").expect("an empty document");
        assert_eq!(aterm_secrets::swarm::enabled_in_value(&empty), Ok(true));
        let off = aterm_types::world::LANES_OFF;
        let config = crate::app_config::Config::parse(off).expect("the lanes-off tables parse");
        assert!(!config.swarm_enabled(), "swarm participation off: {off}");
        assert!(!config.fleet_watch_enabled(), "no fleet reads: {off}");
        assert_eq!(config.keeper_setting(), Some(false), "no keeper job: {off}");
        assert!(
            !crate::app_config::update_checks_automatic(&config),
            "{off}"
        );
        assert!(!config.packages_enabled(), "{off}");
        let doc = aterm_toml::from_str::<aterm_toml::Value>(off).expect("one document");
        assert_eq!(
            aterm_secrets::swarm::enabled_in_value(&doc),
            Ok(false),
            "the browse's own switch reads the swarm off: {off}"
        );
    }

    /// The automatic `_alab._tcp` browse runs only while `[swarm] enabled` leaves sharing on: off
    /// from the start it never browses; turned on it starts; turned off it stops, saying so once;
    /// turned on again it starts again. NEGATIVE CONTROL: a keeper that reads no switch browses
    /// while sharing is off.
    #[cfg(target_os = "macos")]
    #[test]
    fn the_browse_stops_and_starts_with_the_sharing_switch() {
        use std::cell::{Cell, RefCell};
        // The switch as each read finds it: off twice, on for a browse of three looks, off for
        // two reads, on again for one more browse.
        let reads = RefCell::new(
            [
                false, false, true, true, true, true, false, false, false, true, true, false,
            ]
            .into_iter(),
        );
        let sharing = || reads.borrow_mut().next().unwrap_or(false);
        let (browses, looks, waits) = (Cell::new(0), Cell::new(0), Cell::new(0));
        let said = RefCell::new(Vec::new());
        keep_browse(
            sharing,
            || waits.set(waits.get() + 1),
            |going| {
                browses.set(browses.get() + 1);
                while going() {
                    looks.set(looks.get() + 1);
                }
                browses.get() < 2
            },
            |line| said.borrow_mut().push(line.to_owned()),
        );
        assert_eq!(browses.get(), 2, "it browsed while on, twice");
        assert_eq!(
            looks.get(),
            4,
            "each browse ran only while the switch read on"
        );
        assert_eq!(waits.get(), 4, "it waited while the switch read off");
        assert_eq!(
            *said.borrow(),
            [
                BROWSE_SHARING_OFF,
                BROWSE_SHARING_ON,
                BROWSE_SHARING_OFF,
                BROWSE_SHARING_ON
            ],
            "each change said once"
        );
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
        assert_eq!(one.wait(Duration::ZERO), Woke::Timer);

        #[cfg(unix)]
        {
            use aterm_uds::stop::Stop;

            let moved = Arc::new(Moved::new());
            let (mut one, mut two) = (moved.watch_at_once(), moved.watch_at_once());
            let run_stop = Stop::new();
            let generation_stop = Stop::new();
            let stops = [&run_stop, &generation_stop];
            let deadline = Instant::now() + HANG;

            // A queued event wins without needing a native waiter to have been installed.
            moved.tell(Woke::Route);
            assert_eq!(
                one.wait_until(&stops, Some(deadline)).unwrap(),
                Some(Woke::Route)
            );
            assert_eq!(
                two.wait_until(&stops, Some(deadline)).unwrap(),
                Some(Woke::Route)
            );

            // Both native waiters borrow this same generation. The real broadcast must
            // leave its descriptor ready for each, even when neither was polling yet.
            let waiting = moved.told.lock().unwrap().wake();
            let bell = waiting.bell().unwrap();
            moved.tell(Woke::Place);
            for _ in 0..2 {
                assert_eq!(
                    aterm_uds::ready::any_now(&[bell]).unwrap(),
                    [0],
                    "a native notification is retained for every waiter"
                );
            }
            assert_eq!(
                one.wait_until(&stops, Some(deadline)).unwrap(),
                Some(Woke::Place)
            );
            assert_eq!(
                two.wait_until(&stops, Some(deadline)).unwrap(),
                Some(Woke::Place)
            );
            assert_eq!(
                one.wait_until(&stops, Some(Instant::now())).unwrap(),
                Some(Woke::Timer),
                "a consumed native generation must not spin"
            );

            // A new move does not renew the caller's original maintenance deadline.
            let due = Instant::now();
            moved.tell(Woke::Beacon);
            assert_eq!(
                one.wait_until(&stops, Some(due)).unwrap(),
                Some(Woke::Beacon)
            );
            assert_eq!(
                one.wait_until(&stops, Some(due)).unwrap(),
                Some(Woke::Timer)
            );

            // Either stop ends the same native wait without consuming a queued move.
            for run in [false, true] {
                let run_stop = Arc::new(Stop::new());
                let generation_stop = Arc::new(Stop::new());
                let mut watch = moved.watch_at_once();
                let cancelled = if run { &run_stop } else { &generation_stop };
                cancelled.set();
                moved.tell(Woke::Route);
                assert_eq!(
                    watch
                        .wait_until(&[&run_stop, &generation_stop], Some(deadline))
                        .unwrap(),
                    None
                );
                assert_eq!(watch.take(), Some(Woke::Route));

                let run_stop = Arc::new(Stop::new());
                let generation_stop = Arc::new(Stop::new());
                let (worker_run, worker_generation) =
                    (Arc::clone(&run_stop), Arc::clone(&generation_stop));
                let mut watch = moved.watch_at_once();
                let worker = std::thread::spawn(move || {
                    watch.wait_until(&[&worker_run, &worker_generation], Some(deadline))
                });
                if run {
                    run_stop.set();
                } else {
                    generation_stop.set();
                }
                assert_eq!(worker.join().unwrap().unwrap(), None);
            }
        }
    }

    /// Joining a LAN tells a route move, and the `_alab._tcp` browse or a Commons beacon tells
    /// another within this Mac's spread: the two are one wake, and a watch hears it as the route
    /// move, so a consumer that looks again only on a route move (the API reach probe) never
    /// drops it. A watch that heard the route move already is told the later move's own source.
    /// NEGATIVE CONTROL: keeping only the latest move's source wakes the watch with `Browse`.
    #[test]
    fn a_route_move_followed_by_a_browse_still_wakes_a_route_watch() {
        let moved = Arc::new(Moved::spread_by(Duration::from_millis(300)));
        let mut watch = moved.watch();
        let mut at_once = moved.watch_at_once();
        moved.tell(Woke::Route);
        moved.tell(Woke::Browse);
        assert_eq!(
            watch.wait(HANG),
            Woke::Route,
            "the route move is not hidden"
        );
        assert_eq!(at_once.take(), Some(Woke::Route));
        moved.tell(Woke::Beacon);
        assert_eq!(
            watch.wait(HANG),
            Woke::Beacon,
            "no route move since it heard one"
        );
        assert_eq!(at_once.take(), Some(Woke::Beacon));
    }

    /// A PER-MAC SPREAD AFTER A MOVE (docs/DESIGN-fluid-swarm-2026-10-28.md, F4's follow-up): on
    /// a signal spread by 3 s, a move is due 3 s after it was told, never before; a second move
    /// told before the first was heard is one wake, at the later one's time; a wait shorter than the spread reads
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
        assert_eq!(
            watch.take_at(t0 + secs(14)),
            Some(Woke::Route),
            "told as the route move among them"
        );
        assert_eq!(watch.take_at(t0 + secs(60)), None, "one wake for both");

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

    /// THE OTHERS A MEMBER'S BOOK COUNTS: its book's peers and its copy of its issuer's book,
    /// read only for the `Issuer` entry's node, each member once and this Mac's own node aside; a
    /// peer a person named counts by its place (host and port); an empty book counts none.
    /// NEGATIVE CONTROL: reading the copy for another entry with a node (the first one, or a
    /// `Joined` one) fails "the copy of the issuer's book", and keying a named peer by its host
    /// alone fails "each place once".
    #[cfg(unix)]
    #[test]
    fn a_member_counts_its_issuer_and_the_issuers_book_once_each_but_never_itself() {
        use aterm_net::fleet_member::{Peer, Via};
        let peer = |node: Option<&str>, host: &str, via: Via| Peer {
            node: node.map(Into::into),
            key: None,
            key_from: None,
            host: host.into(),
            addr: None,
            port: 7100,
            version: None,
            seen_ms: None,
            via,
            fleet: None,
            signed: None,
        };
        let book = [
            peer(Some("j"), "j.local", Via::Joined),
            peer(Some("i"), "i.local", Via::Issuer),
        ];
        let copies = |issuer: &str| {
            assert_eq!(issuer, "i", "the copy of the issuer's book");
            ["x", "y", "x", "m", "j"]
                .map(|node| peer(Some(node), node, Via::IssuerBook))
                .to_vec()
        };
        assert_eq!(others_in_book(Some("m"), &book, copies), 4, "j, i, x and y");
        let named = [
            peer(None, "h", Via::Manual),
            peer(None, "g", Via::Manual),
            Peer {
                port: 7200,
                ..peer(None, "h", Via::Manual)
            },
            peer(None, "h", Via::Manual),
        ];
        assert_eq!(
            others_in_book(None, &named, |_| panic!("no issuer")),
            3,
            "each place once: h:7100, g:7100 and h:7200"
        );
        assert_eq!(others_in_book(Some("m"), &[], |_| panic!("no book")), 0);
    }

    /// A RETURN FROM AWAY WAITS NO HERD, A REBOOT KEEPS IT (docs/DESIGN-network-place-2026-11-01.md,
    /// part B): a move told as this Mac comes back from a long stay on a phone hotspot (told
    /// before the keeper read home, or after) waits none of its fleet's spread, since only it
    /// moved; a router or access point reboot (the ALab LAN, offline, the same ALab LAN) and a
    /// Mac with no record keep the herd of 35.
    /// NEGATIVE CONTROL: a `herd_given` that keeps the book whatever the record (the original
    /// `members_here`) fails "a lone return waits no herd".
    #[test]
    fn a_return_from_away_waits_no_herd_and_a_reboot_keeps_it() {
        use aterm_netplace::place::{Known, Record, step};
        let t0 = 1_800_000_000_000_u64;
        let min = 60_000;
        let home = aterm_netplace::sample::home(t0, 3);
        let hotspot = aterm_netplace::sample::hotspot(t0, t0);
        let facts = |place: &aterm_netplace::Place| aterm_netplace::Facts {
            route: place.route.clone(),
            tunnel: None,
            path: place.path,
        };
        let offline = aterm_netplace::Facts::default();
        let walk = |record: Record, steps: &[(&aterm_netplace::Facts, u64)]| {
            steps.iter().fold(record, |record, (facts, now)| {
                step(&record, facts, &Default::default(), None, *now, Some(t0)).0
            })
        };
        let learned = Record {
            remembered: vec![Known {
                network: home.network.clone().expect("strong"),
                learned_ms: t0,
                confirmed_ms: t0,
            }],
            ..Record::default()
        };
        let (at_home, on_hotspot) = (facts(&home), facts(&hotspot));
        let away = walk(learned.clone(), &[(&at_home, t0), (&on_hotspot, t0 + min)]);
        assert_eq!(
            herd_given(35, Some(&away), t0 + 60 * min),
            0,
            "a lone return waits no herd"
        );
        let arrived = walk(away, &[(&at_home, t0 + 60 * min)]);
        assert_eq!(
            herd_given(35, Some(&arrived), t0 + 60 * min + 5_000),
            0,
            "told once the keeper read home"
        );
        let rebooted = walk(
            learned,
            &[
                (&at_home, t0),
                (&offline, t0 + 30 * min),
                (&at_home, t0 + 31 * min),
            ],
        );
        assert_eq!(
            herd_given(35, Some(&rebooted), t0 + 31 * min),
            35,
            "a reboot keeps the herd"
        );
        assert_eq!(herd_given(35, None, t0), 35, "no record keeps the herd");
    }

    /// THE SETTLED PLACE IS A MOVE TOLD WITH NO REST: the place source over a record directory
    /// tells each watch [`Woke::Place`] once the record settles onto a network (from offline),
    /// and again at once when it settles onto another one.
    /// NEGATIVE CONTROL: a source that reads no record (its closure answering `Some(false)`)
    /// tells nothing, and the first wait hangs past the one-minute detector: "the arrival".
    #[cfg(unix)]
    #[test]
    fn a_settled_place_is_told_as_a_move_with_no_rest() {
        use aterm_netplace::place::{Record, step};
        use aterm_netplace::record::{Written, write};
        let root = aterm_tempfile::tempdir().expect("a scratch directory");
        let dir = aterm_netplace::record::dir(root.path());
        let t0 = 1_800_000_000_000_u64;
        let home = aterm_netplace::sample::home(t0, 3);
        let other = {
            let mut route = home.route.clone().expect("a route");
            route.gateway_hw = Some(vec![9, 9, 9, 9, 9, 9]);
            route
        };
        let facts = |route: Option<aterm_netplace::RouteFacts>| aterm_netplace::Facts {
            route,
            tunnel: None,
            path: home.path,
        };
        let mut record = Record::default();
        let mut put = |facts: &aterm_netplace::Facts, now: u64| {
            record = step(&record, facts, &Default::default(), None, now, Some(t0)).0;
            write(
                &dir,
                &Written {
                    record: record.clone(),
                    ..Written::default()
                },
            )
            .expect("the record");
        };
        put(&facts(None), t0);
        let moved = Arc::new(Moved::new());
        let mut watch = moved.watch();
        let looks = PlaceLooks {
            after_a_change: Duration::from_millis(5),
            fast_for: Duration::from_millis(5),
            steady: Duration::from_millis(5),
        };
        assert!(hear_place(&moved, dir.clone(), None, looks));
        put(&facts(home.route.clone()), t0 + 60_000);
        assert_eq!(watch.wait(HANG), Woke::Place, "the arrival");
        put(&facts(Some(other)), t0 + 120_000);
        assert_eq!(watch.wait(HANG), Woke::Place, "another network, at once");
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

    /// A peer's comings and goings on the browse are no move of this Mac's network: over an hour
    /// in which, each half minute, a member starts under a name never heard (each process names
    /// its advert afresh), one leaves, and one comes back after longer than
    /// [`aterm_secrets::discover::SILENCE`], the source, asked each half minute as its rest
    /// allows, tells none. This Mac back after every member's silence is one move. The silence
    /// runs at its real length, on injected times.
    /// NEGATIVE CONTROL: the rule that took each member not held as a move (0.155.0, at its rest
    /// floor in the owner's log) tells a move at every half minute of the hour; the closing
    /// rejoin tells its one move under either rule.
    #[test]
    #[cfg(unix)]
    fn an_hour_of_members_coming_and_going_on_the_browse_tells_no_move() {
        let secs = Duration::from_secs;
        let silence = aterm_secrets::discover::SILENCE;
        let bell = Arc::new(aterm_secrets::wake::Bell::new());
        let browse = aterm_secrets::discover::Watch::idle_for(
            aterm_agent::node_service::ALAB_TCP_SERVICE,
            &bell,
            &["alab-0000a0a0"],
        );
        let moved = Arc::new(Moved::new());
        let (look, looks) = std::sync::mpsc::channel::<()>();
        let (answer, answers) = std::sync::mpsc::channel::<bool>();
        let (ended, end) = std::sync::mpsc::channel::<()>();
        assert!(spawn_source(
            "test-browse-churn",
            Arc::clone(&moved),
            (Woke::Browse, Duration::ZERO),
            move || {
                if looks.recv().is_err() {
                    let _ = ended.send(());
                    return None;
                }
                let rung = bell.nap(Duration::ZERO) == aterm_secrets::wake::Wake::Browse;
                answer.send(rung).ok()?;
                Some(rung)
            }
        ));
        let ask = || {
            look.send(()).expect("the source is running");
            answers.recv_timeout(HANG).expect("the source looks")
        };
        let name = |i: u32| format!("alab-{i:08x}");
        let start = Instant::now();
        browse.began_browse();
        browse.hear_at(true, 4, &name(0), start);
        browse.hear_at(true, 4, "alab-0000a0a0", start);
        browse.child_began(secs(3600));
        let mut at = start;
        let mut told = 0;
        for i in 1..=120 {
            at += secs(30);
            browse.hear_at(true, 4, &name(i), at);
            if i > 4 {
                browse.hear_at(false, 4, &name(i - 4), at);
            }
            if i > 9 {
                browse.hear_at(true, 4, &name(i - 9), at);
            }
            told += usize::from(ask());
        }
        assert_eq!(told, 0, "moves told over {:?} of churn", at - start);
        let members = browse.heard();
        assert!(members.len() > 100, "{members:?}");
        let left = at + secs(1);
        for member in &members {
            browse.hear_at(false, 4, member, left);
        }
        assert!(!ask(), "every member left");
        for member in &members {
            browse.hear_at(true, 4, member, left + silence + secs(1));
        }
        assert!(ask(), "back after every member's silence");
        assert!(!ask(), "told once");
        drop(look);
        end.recv_timeout(HANG)
            .expect("the source ends once nothing asks");
        assert_eq!(
            moved.told(),
            1,
            "one move for the rejoin, none for the hour"
        );
        drop(browse);
    }

    /// One rejoin heard on the browse is one move: when this Mac comes back after longer than
    /// [`aterm_secrets::discover::SILENCE`], every member's `Add` comes at once (none is held
    /// since its `Rmv` as the Mac left), and the browse's bell keeps one ring for all of them, so
    /// the source tells them as one move, as the route source and the Commons beacon tell theirs,
    /// not one move a rest each for a quarter of an hour.
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
        let back = left + aterm_secrets::discover::SILENCE + Duration::from_secs(1);
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

    /// A Commons peer heard after every peer's silence longer than [`SILENCE`], or one new, is
    /// a move once the baseline is over; one heard within it, or one gone silent, is not.
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

    /// The beacon source's baseline at its real count, not the one-look baseline of the tests
    /// around it: the source looks once a [`BEACON_LOOK`] after it starts, and every look in
    /// the first half minute only learns who is heard, one look each however many peers it
    /// hears. A runtime just started has confirmed no peer yet (each is LastSeen or Unknown
    /// until an authenticated contact), so the first look hears none; from the second it hears
    /// two. A peer first heard on the half minute's last look was there before this process
    /// looked, so it is no move, nor on the next look (heard within [`SILENCE`], as are the
    /// peers heard throughout); a peer first heard on the first look after the half minute is
    /// one.
    /// NEGATIVE CONTROL: `SETTLE_LOOKS = 14` fails "first heard on the baseline's last look";
    /// `SETTLE_LOOKS = 16` fails "new on the first look after the baseline"; a baseline used up
    /// once per peer heard rather than once per look fails "first heard on the baseline's last
    /// look"; one that a look hearing no peer leaves whole fails "new on the first look after
    /// the baseline".
    #[test]
    fn the_commons_baseline_is_fifteen_looks() {
        use aterm_types::network_state::Reachability::{LastSeen, Reachable, Unknown};
        const HALF_MINUTE: Duration = Duration::from_secs(30);
        let last = u32::try_from(HALF_MINUTE.as_millis() / BEACON_LOOK.as_millis())
            .expect("a half minute is a few looks");
        assert_eq!(BEACON_LOOK * last, HALF_MINUTE, "whole looks");
        let start = Instant::now();
        let at = |look: u32| start + BEACON_LOOK * look;
        let unconfirmed = [peer(1, LastSeen), peer(4, Unknown)];
        let two = [peer(1, Reachable), peer(4, Reachable)];
        let (mut heard, mut fresh) = (Heard::commons(), Heard::commons());
        for watch in [&mut heard, &mut fresh] {
            assert!(
                !watch.look_at(&unconfirmed, at(1)),
                "baseline look 1 hears no peer"
            );
            for look in 2..last {
                assert!(!watch.look_at(&two, at(look)), "baseline look {look}");
            }
        }
        let three = [peer(1, Reachable), peer(4, Reachable), peer(2, Reachable)];
        assert!(
            !heard.look_at(&three, at(last)),
            "first heard on the baseline's last look"
        );
        assert!(
            !heard.look_at(&three, at(last + 1)),
            "heard throughout, and since the baseline's last look"
        );
        assert!(
            heard.look_at(
                &[
                    peer(1, Reachable),
                    peer(4, Reachable),
                    peer(2, Reachable),
                    peer(3, Reachable)
                ],
                at(last + 2)
            ),
            "new after the baseline"
        );
        assert!(
            !fresh.look_at(&two, at(last)),
            "heard throughout the baseline"
        );
        assert!(
            fresh.look_at(
                &[peer(1, Reachable), peer(4, Reachable), peer(3, Reachable)],
                at(last + 1)
            ),
            "new on the first look after the baseline"
        );
    }

    /// Missing samples do not count as empty observations, even after the silence has elapsed.
    /// NEGATIVE CONTROL: treating an unavailable sample as an empty look consumes the baseline
    /// and forgets the peer before an actual observation arrives.
    #[test]
    fn an_unavailable_commons_sample_leaves_the_watch_unchanged() {
        use aterm_types::network_state::Reachability::Reachable;
        let at = Instant::now();
        let later = at + SILENCE + Duration::from_secs(1);
        let mut heard = Heard::new(2);
        assert!(!heard.look_sample_at(Some(&[peer(1, Reachable)]), at));
        assert_eq!(heard.baseline, 1);
        let before = heard.heard.clone();

        assert!(!heard.look_sample_at(None, later));
        assert_eq!(heard.baseline, 1, "no baseline look was observed");
        assert_eq!(heard.heard, before, "no peer was forgotten or heard again");

        assert!(
            !heard.look_sample_at(Some(&[peer(1, Reachable)]), later),
            "the next actual look finishes the baseline"
        );
        assert_eq!(heard.baseline, 0);
        assert_eq!(heard.heard.get(&[1; 16]), Some(&later));
        assert!(
            heard.look_sample_at(Some(&[peer(1, Reachable), peer(2, Reachable)]), later),
            "a new peer in a subsequent actual sample is a move"
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

    /// One peer back from any silence is no move while another is heard: among some 40 Macs one
    /// always is, and the source fired after each of its rests.
    /// NEGATIVE CONTROL: a look that times each peer's silence alone takes this return as a move.
    #[test]
    fn a_peer_back_while_another_is_heard_is_no_move() {
        use aterm_types::network_state::Reachability::{LastSeen, Reachable};
        let at = Instant::now();
        let mut heard = Heard::new(1);
        let both = [peer(1, Reachable), peer(2, Reachable)];
        assert!(!heard.look_at(&both, at), "baseline");
        let back = SILENCE.as_secs() * 2;
        for secs in (2..back).step_by(2) {
            let look = at + Duration::from_secs(secs);
            let silent = [peer(1, Reachable), peer(2, LastSeen)];
            assert!(!heard.look_at(&silent, look), "peer 2 silent at {secs} s");
        }
        assert!(
            !heard.look_at(&both, at + Duration::from_secs(back)),
            "peer 1 was heard all along"
        );
    }

    /// The beacon reads the network's peers and asks for no services read, which would keep the
    /// services' observer sampling for a beacon that draws none of them.
    /// NEGATIVE CONTROL: a beacon reading with services records `WithServices`.
    #[test]
    fn the_beacon_reads_the_network_alone() {
        use aterm_types::network_state::{NetworkStateSnapshot, Reachability::Reachable};
        let asked = Arc::new(Mutex::new(Vec::new()));
        let mut snapshot = NetworkStateSnapshot::default();
        snapshot.network.peers = vec![peer(7, Reachable)];
        let snapshot = Arc::new(snapshot);
        let reads = Arc::clone(&asked);
        let peers = beacon_peers(Arc::new(move |read| {
            reads.lock().unwrap().push(read);
            Some(Arc::clone(&snapshot))
        }));
        assert_eq!(peers(), Some(vec![peer(7, Reachable)]));
        assert_eq!(*asked.lock().unwrap(), [crate::NetworkRead::Network]);
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
            last_repair: None,
            last_authenticated_endpoint: None,
            reachability,
            instances: None,
            samples: Vec::new(),
        }
    }
}
