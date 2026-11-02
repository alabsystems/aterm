// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THIS PROCESS'S "NETWORK MOVED" SIGNAL (docs/DESIGN-fluid-swarm-2026-10-28.md §3.5.2, F3c).
//! Nothing carries the signal between processes, so the window and the node job each watch
//! their own sources: the kernel's route socket ([`aterm_uds::network::RouteChanges`]), the one
//! `_alab._tcp` browse a process holds, on a Bonjour connection it owns, which tells each member
//! it did not hold and resolves what it holds to dial hints ([`hear_alab_tcp`]: the node job's,
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
    /// The process's `_alab._tcp` browse heard a member it did not hold ([`hear_alab_tcp`]).
    #[cfg_attr(
        not(any(target_os = "macos", test)),
        expect(dead_code, reason = "the browse source is macOS's")
    )]
    Browse,
    /// The Commons runtime heard a peer after every peer's silence ([`SILENCE`]), or a new one.
    Beacon,
    /// macOS's Local Network answer for aterm moved as the window heard it
    /// ([`local_network_moved`]): which LAN hosts aterm may reach changed, or may have.
    LocalNetwork,
    /// The network place settled ([`hear_place`], `aterm_netplace::moves`): the keeper read the
    /// new network, so it is usable now, told behind a route move's with only the flap rest
    /// (`aterm_netplace::moves::REST_MS`). Off macOS and Linux nothing tells it, tests included
    /// (they tell it on Unix).
    #[cfg_attr(
        not(any(target_os = "macos", target_os = "linux")),
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
            Self::Browse => "a member heard on the _alab._tcp browse",
            Self::Beacon => "a Commons beacon after two minutes of no peer, or from a new one",
            Self::LocalNetwork => "macOS's Local Network answer for aterm",
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
}

impl Told {
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
            told: Mutex::new(Told {
                count: 0,
                source: Woke::Timer,
                due: None,
                route: 0,
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
    /// ([`Told::route`]). macOS's Local Network answer is this Mac's own, no event on the LAN
    /// every Mac hears at once, so its watches hear it at once.
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
        told.count = told.count.wrapping_add(1);
        told.source = source;
        told.due = due;
        if source == Woke::Route {
            told.route = told.count;
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

    /// Whether [`Watch::take_at`] would take a move at `now`, taking none. Its one caller, the
    /// heartbeat clock, is Unix's.
    #[cfg(unix)]
    pub(crate) fn due_at(&self, now: Instant) -> bool {
        let told = self
            .moved
            .told
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        told.count != self.seen && told.due.is_none_or(|due| !self.spread || now >= due)
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
    let copies = book
        .iter()
        .find(|peer| peer.via == member::Via::Issuer)
        .and_then(|issuer| issuer.node.as_deref())
        .map(|issuer| member::read_issuer_book(&state, issuer))
        .unwrap_or_default();
    let others = book
        .iter()
        .chain(&copies)
        .filter(|peer| peer.node.is_none() || peer.node != own)
        .map(member::answer_key)
        .collect::<std::collections::BTreeSet<_>>()
        .len();
    let record = aterm_netplace::record::read(&aterm_netplace::record::dir(&state));
    herd_given(
        others,
        record.as_ref().map(|written| &written.record),
        aterm_netplace::unix_ms_now(),
    )
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
    #[cfg(all(not(test), any(target_os = "macos", target_os = "linux")))]
    if let Some(state) = aterm_types::dirs::state_dir() {
        hear_place(
            moved,
            aterm_netplace::record::dir(&state),
            aterm_uds::network::RouteChanges::open(),
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

/// How long the browse, waiting for System State's Local Network step or for sharing to be on
/// ([`keep_browse`]), waits before it looks again with no move told: the step's question wakes it
/// sooner ([`local_network_moved`]).
#[cfg(all(not(test), target_os = "macos"))]
const STEP_LOOK: Duration = Duration::from_secs(10);

/// How often a running browse looks at `[swarm] enabled` while it hears nothing
/// ([`aterm_net::bonjour::browse_while`]): one `stat` ([`aterm_secrets::swarm::Switch`]).
#[cfg(all(not(test), target_os = "macos"))]
const SHARING_LOOK: Duration = Duration::from_secs(5);

/// What the browse's keeper says in aterm.log once per change ([`keep_browse`]).
#[cfg(target_os = "macos")]
const BROWSE_SHARING_OFF: &str = "network moved: sharing is off ([swarm] enabled = false), so the \
    _alab._tcp browse is stopped until it is on";

/// What the browse's keeper says once it waited for System State's Local Network step
/// ([`keep_browse`]).
#[cfg(target_os = "macos")]
const BROWSE_WAITED: &str =
    "network moved: the _alab._tcp browse waited for System State's Local Network step";

/// What the browse's keeper says once a person turns sharing back on ([`keep_browse`]).
#[cfg(target_os = "macos")]
const BROWSE_SHARING_ON: &str = "network moved: sharing is on again, so the _alab._tcp browse \
    starts again";

/// Keep this process's `_alab._tcp` browse while `[swarm] enabled` leaves sharing on
/// (`sharing`, `docs/DESIGN-alab-swarm-2026-10-27.md` §9): it starts only with sharing on and
/// once this process `may_use` the LAN ([`browse_after_the_step`]), both read again after each
/// `wait`; it runs (`browse`, told to go on only while `sharing` stays on) until sharing goes
/// off, and starts again once a person turns it back on. Each stop and start is said once in
/// `said`. It returns once `browse` says the browse is not to be kept (a test's end).
#[cfg(target_os = "macos")]
fn keep_browse(
    mut sharing: impl FnMut() -> bool,
    mut may_use: impl FnMut() -> bool,
    mut wait: impl FnMut(),
    mut browse: impl FnMut(&mut dyn FnMut() -> bool) -> bool,
    mut said: impl FnMut(&str),
) {
    let mut off = false;
    loop {
        let mut for_the_step = false;
        browse_after_the_step(
            || {
                let on = sharing();
                if on == off {
                    off = !on;
                    said(if on {
                        BROWSE_SHARING_ON
                    } else {
                        BROWSE_SHARING_OFF
                    });
                }
                let goes = on && may_use();
                for_the_step |= on && !goes;
                goes
            },
            &mut wait,
            || {},
        );
        if for_the_step {
            said(BROWSE_WAITED);
        }
        if !browse(&mut sharing) {
            return;
        }
    }
}

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
            let mut switch =
                aterm_types::dirs::home_dir().map(|home| aterm_secrets::swarm::Switch::new(&home));
            keep_browse(
                || {
                    switch.as_mut().is_none_or(|switch| {
                        switch.enabled_saying(&|line| aterm_log::warn!("network moved: {line}"))
                            == Ok(true)
                    })
                },
                aterm_types::macos_setup::lan_first_use::may_use,
                || {
                    let _ = step.wait(STEP_LOOK);
                },
                |sharing| {
                    browse_until_off(&service, &watch, sharing);
                    true
                },
                |line| aterm_log::info!("{line}"),
            );
        });
    if let Err(error) = spawned {
        aterm_log::warn!("network moved: the browse source did not start: {error}");
    }
}

/// Browse `service` on `local.` while `sharing` stays on, each event reaching `watch`
/// ([`aterm_secrets::discover::Watch::hear`]), a fresh connection [`BROWSE_AGAIN`] after one
/// that failed; it returns once sharing is off (looked at each [`SHARING_LOOK`] the browse hears
/// nothing, and before each event).
#[cfg(all(not(test), target_os = "macos"))]
fn browse_until_off(
    service: &std::ffi::CStr,
    watch: &aterm_secrets::discover::Watch,
    sharing: &mut dyn FnMut() -> bool,
) {
    use std::ops::ControlFlow;
    let mut said = false;
    while sharing() {
        watch.began_browse();
        let result = aterm_net::bonjour::browse_while(
            service,
            c"local.",
            SHARING_LOOK,
            &mut *sharing,
            |event| {
                if let Ok(name) = event.name.to_str() {
                    watch.hear(event.added, event.interface, name);
                }
                ControlFlow::Continue(())
            },
        );
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
            || true,
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
        assert_eq!(one.wait(Duration::from_millis(1)), Woke::Timer);
    }

    /// Joining a LAN tells a route move, and the `_alab._tcp` browse or a Commons beacon tells
    /// another within this Mac's spread: the two are one wake, and a watch hears it as the route
    /// move, so a consumer that looks again only on a route move (the API reach probe) never
    /// drops it. A watch that heard the route move already is told the later move's own source.
    /// NEGATIVE CONTROL: keeping only the latest move's source wakes the watch with `Browse`.
    #[test]
    fn a_route_move_followed_by_a_browse_still_wakes_the_local_network_step() {
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
        assert_eq!(
            watch.take_at(t0 + secs(14)),
            Some(Woke::Route),
            "told as the route move among them"
        );
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
