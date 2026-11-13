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

    /// The exact generation and source due now, without acknowledging either. A
    /// caller may dispatch first, then acknowledge only this selected generation.
    #[cfg(any(unix, test))]
    pub(crate) fn pending_at(&self, now: Instant) -> Option<(u64, Woke)> {
        let told = self
            .moved
            .told
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        (told.count != self.seen && told.due.is_none_or(|due| !self.spread || now >= due))
            .then_some((told.count, told.heard_since(self.seen)))
    }

    /// Complete one selected generation after its dispatch was accepted. A move
    /// told during dispatch remains newer than `seen` and belongs to the next round.
    #[cfg(any(unix, test))]
    pub(crate) fn acknowledge(&mut self, generation: u64) {
        self.seen = generation;
    }

    /// The source of the latest move this watch was not told of and is due at `now`, without
    /// waiting. Consumers that need no dispatch acknowledgement take it immediately.
    #[cfg(any(unix, test))]
    pub(crate) fn take_at(&mut self, now: Instant) -> Option<Woke> {
        let (generation, woke) = self.pending_at(now)?;
        self.acknowledge(generation);
        Some(woke)
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
/// when no fleet, account, or converge consumer has requested a watch yet, and
/// while the launch's `[update] enabled` has no answer yet, since the checker
/// may start once it answers.
#[cfg(any(target_os = "macos", target_os = "linux"))]
pub(crate) fn observe_for_updater() {
    if aterm_update::automatic_at_launch() != Some(false) {
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
#[path = "network_moved_tests.rs"]
pub(crate) mod tests;
