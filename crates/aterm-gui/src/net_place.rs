// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! This Mac's network place as the window reports it (`aterm_netplace`): `status`'s `net=…`
//! fields, `sys`'s `place` line and object, the network deck's summary and the menu bar's first
//! line, every one in aterm-netplace's words, so none of them can say something else about where
//! this Mac is.
//!
//! The event loop reads no file for it: [`latest`] answers the place the last background read
//! made, and a read older than [`aterm_netplace::keeper::CACHE_FOR`] requests the next from its
//! sole observation owner; paint takes [`cached`], which requests nothing. [`start`]
//! starts this process's keeper candidate and registers the window with the owner, which reads
//! the place again when an input it is made from changes (a file of the place's folder, or the
//! routes it reads live: [`aterm_netplace::watch`], both armed before the owner's first read), at
//! the place's own next time-only change or the away entry's dwell, on the keeper's bounded look
//! for the gateway's ARP entry (which no event announces), and on no clock otherwise
//! (ATERM-KI-1105-20: it read every ten seconds), and wakes
//! the event loop on an away-key transition
//! ([`App::follow_network_place`]: one Info fact in the exceptions digest while this Mac is away
//! from the ALab LAN, none once it is back; never an alert, never NEEDS YOU). A unit test reads
//! nothing of the person's: it sees only the place it held ([`hold`]), on its own thread.

use std::sync::Arc;
#[cfg(unix)]
use std::sync::{Condvar, Mutex};
#[cfg(unix)]
use std::time::Instant;

use aterm_netplace::Place;
use aterm_netplace::words::{self, Words};
#[cfg(unix)]
use aterm_types::MutexExt as _;

/// The sole observer's accepted snapshot, pending request and notification state.
#[cfg(unix)]
struct Desk {
    place: Option<Arc<Place>>,
    read_at: Option<Instant>,
    retry_at: Option<Instant>,
    running: bool,
    requested: bool,
    closed: bool,
    changed: bool,
    /// An input the place is made from (a file, the routes) changed since the owner's last read
    /// began.
    input: bool,
    /// Whether those inputs are watched; where not, the owner reads again every
    /// [`aterm_netplace::keeper::UNWATCHED_EVERY`].
    watched: bool,
    /// A watch the owner started with was lost since (its route socket failed).
    lost: bool,
    /// How many input changes were told, for a test to take a watch's first looks.
    #[cfg(test)]
    told: usize,
    following: FollowState,
    notice: Option<Arc<Notice>>,
}

#[cfg(unix)]
impl Desk {
    /// How long until the owner reads again with no request and no input change, `None` for no
    /// clock at all: at once where it never read; else the earliest of the keeper's own bounded
    /// look at the place last read (the gateway's ARP entry while a move settles or the network
    /// keeps a weak id, which no event announces; every input where they are not watched,
    /// [`aterm_netplace::keeper::next_look`]) and the place's own next time-only change or the
    /// away key's dwell ([`FollowState::wait`]). `now` and `now_ms` are one moment.
    fn until_read(&self, now: Instant, now_ms: u64) -> Option<std::time::Duration> {
        let look = match (self.read_at, self.place.as_deref()) {
            (Some(at), Some(place)) => aterm_netplace::keeper::next_look(place, self.watched)
                .map(|look| look.saturating_sub(now.saturating_duration_since(at))),
            _ => Some(std::time::Duration::ZERO),
        };
        match (self.following.wait(now_ms), look) {
            (Some(due), Some(look)) => Some(due.min(look)),
            (due, look) => due.or(look),
        }
    }
}

#[cfg(unix)]
enum Notice {
    #[cfg(all(unix, not(test)))]
    Window(aterm_window::event_loop::EventLoopProxy<crate::Wake>),
    #[cfg(test)]
    Test(std::sync::mpsc::Sender<()>),
}

#[cfg(unix)]
impl Notice {
    fn send(&self) -> bool {
        match self {
            #[cfg(all(unix, not(test)))]
            Self::Window(proxy) => proxy.send_event(crate::Wake::NetworkPlaceChanged).is_ok(),
            #[cfg(test)]
            Self::Test(sender) => sender.send(()).is_ok(),
        }
    }
}

#[cfg(unix)]
struct ObservationDesk {
    state: Mutex<Desk>,
    wake: Condvar,
}

#[cfg(unix)]
impl ObservationDesk {
    const fn new() -> Self {
        Self {
            state: Mutex::new(Desk {
                place: None,
                read_at: None,
                retry_at: None,
                running: false,
                requested: false,
                closed: false,
                changed: true,
                input: false,
                watched: false,
                lost: false,
                #[cfg(test)]
                told: 0,
                following: FollowState {
                    stated: None,
                    next_change_ms: None,
                },
                notice: None,
            }),
            wake: Condvar::new(),
        }
    }

    fn register(&self, notice: Notice) {
        let notice = Arc::new(notice);
        let pending = {
            let mut held = self.state.lock_or_recover();
            if held.closed {
                return;
            }
            held.notice = Some(Arc::clone(&notice));
            self.wake.notify_one();
            held.changed
        };
        if pending {
            self.notify(&notice);
        }
    }

    fn notify(&self, notice: &Arc<Notice>) {
        if !notice.send() {
            let mut held = self.state.lock_or_recover();
            if held
                .notice
                .as_ref()
                .is_some_and(|current| Arc::ptr_eq(current, notice))
            {
                held.notice = None;
            }
        }
    }

    fn close(&self) {
        let mut held = self.state.lock_or_recover();
        held.closed = true;
        held.notice = None;
        self.wake.notify_one();
    }

    /// An input the place is made from changed (a watch heard a file written or the routes
    /// change): LATCHED for the owner, so a change during a read is read again as soon as that
    /// read lands.
    fn input_changed(&self) {
        let mut held = self.state.lock_or_recover();
        held.input = true;
        #[cfg(test)]
        {
            held.told += 1;
        }
        self.wake.notify_one();
    }

    /// A watch the owner relied on failed: from now on it reads on
    /// [`aterm_netplace::keeper::UNWATCHED_EVERY`] too.
    fn unwatched(&self) {
        let mut held = self.state.lock_or_recover();
        held.lost = true;
        held.watched = false;
        self.wake.notify_one();
    }

    fn take_change(&self) -> Option<Option<Arc<Place>>> {
        let mut held = self.state.lock_or_recover();
        if !std::mem::take(&mut held.changed) {
            return None;
        }
        Some(held.place.clone())
    }

    fn request(&self) -> (Option<Arc<Place>>, bool) {
        let mut held = self.state.lock_or_recover();
        if held.closed {
            return (held.place.clone(), false);
        }
        let now = Instant::now();
        if held
            .read_at
            .is_none_or(|at| now.saturating_duration_since(at) >= aterm_netplace::keeper::CACHE_FOR)
        {
            held.requested = true;
            self.wake.notify_one();
        }
        let start = !held.running && held.retry_at.is_none_or(|at| now >= at);
        if start {
            held.running = true;
        }
        (held.place.clone(), start)
    }

    fn failed_start(&self) {
        let mut held = self.state.lock_or_recover();
        held.running = false;
        held.retry_at = Some(Instant::now() + aterm_netplace::keeper::CACHE_FOR);
    }

    /// One owner alone performs reads, each answering the place and its own next time-only
    /// change ([`aterm_netplace::keeper::read_projected`]). Requests and input changes arriving
    /// during I/O remain latched: each is taken as the read that answers it begins, never after.
    /// `watched` says whether the place's inputs are watched (the caller armed those watches
    /// before this).
    fn run(&self, watched: bool, mut read: impl FnMut() -> (Arc<Place>, Option<u64>)) {
        struct Running<'a>(&'a ObservationDesk);
        impl Drop for Running<'_> {
            fn drop(&mut self) {
                let mut held = self.0.state.lock_or_recover();
                held.running = false;
                held.retry_at = Some(Instant::now() + aterm_netplace::keeper::CACHE_FOR);
                self.0.wake.notify_one();
            }
        }
        let _running = Running(self);
        {
            let mut held = self.state.lock_or_recover();
            held.watched = watched && !held.lost;
        }
        loop {
            let observed_at = {
                let mut held = self.state.lock_or_recover();
                loop {
                    if held.closed {
                        return;
                    }
                    if !held.requested && held.notice.is_none() {
                        held = self
                            .wake
                            .wait(held)
                            .unwrap_or_else(std::sync::PoisonError::into_inner);
                        continue;
                    }
                    let now = Instant::now();
                    let wait = held.until_read(now, aterm_netplace::unix_ms_now());
                    if held.requested || held.input || wait.is_some_and(|wait| wait.is_zero()) {
                        held.requested = false;
                        held.input = false;
                        break now;
                    }
                    held = match wait {
                        Some(wait) => {
                            self.wake
                                .wait_timeout(held, wait)
                                .unwrap_or_else(std::sync::PoisonError::into_inner)
                                .0
                        }
                        None => self
                            .wake
                            .wait(held)
                            .unwrap_or_else(std::sync::PoisonError::into_inner),
                    };
                }
            };
            let (place, place_due) = read();
            let mut held = self.state.lock_or_recover();
            if held.closed {
                return;
            }
            let changed = held
                .following
                .observe(&place, place_due, aterm_netplace::unix_ms_now());
            held.place = Some(place);
            held.read_at = Some(observed_at);
            held.retry_at = None;
            // Tests await cache publication itself, which need not change the away key.
            #[cfg(test)]
            self.wake.notify_all();
            let notice = if changed {
                held.changed = true;
                held.notice.clone()
            } else {
                None
            };
            drop(held);
            if let Some(notice) = notice {
                self.notify(&notice);
            }
        }
    }
}

#[cfg(all(unix, not(test)))]
static DESK: ObservationDesk = ObservationDesk::new();

/// No observer runs off Unix, but the first reconciliation must clear a restored old entry.
#[cfg(all(not(unix), not(test)))]
static UNKNOWN_PENDING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(true);

#[cfg(test)]
thread_local! {
    /// The place a test holds for its own thread ([`hold`]).
    static HELD: std::cell::RefCell<Option<Arc<Place>>> = const { std::cell::RefCell::new(None) };
}

/// A test's place for the reporters on its own thread.
#[cfg(test)]
pub(crate) fn hold(place: Option<Place>) {
    HELD.with(|held| *held.borrow_mut() = place.map(Arc::new));
}

#[cfg(test)]
thread_local! {
    /// How many times this thread asked for the place in the way that starts a read once the
    /// last is stale ([`latest`]); [`cached`] never counts.
    static ASKED: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// How many times this thread called [`latest`], each a read it may start: what a test of a
/// render-thread path holds at zero.
#[cfg(test)]
pub(crate) fn reads_asked() -> usize {
    ASKED.with(std::cell::Cell::get)
}

/// Restore a test thread's prior place, including when its fixture unwinds.
#[cfg(test)]
pub(crate) struct HeldPlace(Option<Arc<Place>>);

#[cfg(test)]
impl Drop for HeldPlace {
    fn drop(&mut self) {
        HELD.with(|held| *held.borrow_mut() = self.0.take());
    }
}

#[cfg(test)]
pub(crate) fn hold_scoped(place: Option<Place>) -> HeldPlace {
    HeldPlace(HELD.with(|held| held.replace(place.map(Arc::new))))
}

/// Register the window and start this process's keeper candidate. The observation owner may
/// already serve on-demand reads; registration enables its follow schedule (file changes and the
/// dwell deadline) and flushes any accepted pending change. Unit tests compile no startup path:
/// the root is the person's.
#[cfg(all(unix, not(test)))]
pub(crate) fn start(proxy: aterm_window::event_loop::EventLoopProxy<crate::Wake>) {
    let Some(state) = aterm_types::dirs::state_dir() else {
        return;
    };
    let _ = aterm_netplace::keeper::start(&state);
    DESK.register(Notice::Window(proxy));
    let _ = latest();
}

#[cfg(any(unix, test))]
#[derive(Default)]
struct FollowState {
    stated: Option<Option<words::AwayKey>>,
    next_change_ms: Option<u64>,
}

#[cfg(any(unix, test))]
impl FollowState {
    /// Follow `place`, read with `place_due` its own next time-only change: whether the away
    /// key changed. The next deadline is the earlier of `place_due` and the key's dwell.
    fn observe(&mut self, place: &Place, place_due: Option<u64>, now_ms: u64) -> bool {
        let projection = words::away_key_with_deadline(place, now_ms);
        let changed = self.stated != Some(projection.key);
        self.stated = Some(projection.key);
        self.next_change_ms = match (projection.next_change_ms, place_due) {
            (Some(dwell), Some(due)) => Some(dwell.min(due)),
            (dwell, due) => dwell.or(due),
        };
        changed
    }

    /// How long until the next deadline (the place's own time-only change or the away key's
    /// dwell), `None` with none: called with a fresh wall sample after publication and
    /// notification effects.
    fn wait(&self, now_ms: u64) -> Option<std::time::Duration> {
        self.next_change_ms
            .map(|at| std::time::Duration::from_millis(at.saturating_sub(now_ms)))
    }
}

/// Start the sole observer outside the desk lock. It serves on-demand reads without a window;
/// a registered window also enables reads on each change of the place's inputs and at its
/// deadlines ([`observe`]). Each actual away-key transition wakes that window; internet-only
/// changes generate no notification.
#[cfg(all(unix, not(test)))]
fn follow(state: std::path::PathBuf) {
    let started = crate::qos::spawn_lane_thread(
        "aterm-netplace-follow",
        crate::qos::Role::Background,
        move || {
            let files = watch_files(&DESK, &state);
            let _wall = watch_wall(
                &DESK,
                aterm_uds::wall_watch::WallWatch::start().map(std::sync::Arc::new),
            );
            #[cfg(any(target_os = "macos", target_os = "linux"))]
            let routes = watch_routes(&DESK, aterm_uds::network::RouteChanges::open());
            #[cfg(not(any(target_os = "macos", target_os = "linux")))]
            let routes = None::<()>;
            observe(&DESK, &state, files, routes, || {
                aterm_netplace::keeper::read_projected(&state)
            });
        },
    );
    if let Err(e) = started {
        DESK.failed_start();
        aterm_log::warn!("network place: the window's follow lane did not start: {e}");
    }
}

/// The place's folder under `state` watched for `desk` ([`aterm_netplace::watch::place_files`]):
/// each settled burst of writes is an input change.
#[cfg(unix)]
fn watch_files(
    desk: &'static ObservationDesk,
    state: &std::path::Path,
) -> Option<aterm_netplace::watch::Watch> {
    aterm_netplace::watch::place_files(
        &aterm_netplace::record::dir(state),
        aterm_netplace::watch::Reads::Reader,
        |_| desk.input_changed(),
    )
}

/// The wall clock's moves heard for `desk` through `source` ([`aterm_netplace::watch::wall`]):
/// a sleep's wake or a wall-clock step moves the place's wall-clock deadlines under the monotonic
/// wait, so each is an input change (ATERM-KI-1105-20).
#[cfg(unix)]
fn watch_wall(
    desk: &'static ObservationDesk,
    source: Option<Arc<aterm_uds::wall_watch::WallWatch>>,
) -> Option<aterm_netplace::watch::WallMoves> {
    aterm_netplace::watch::wall(source, || desk.input_changed())
}

/// The routes watched for `desk` through `changes` ([`aterm_netplace::watch::routes`]): each
/// change of network is an input change, and the socket failing puts `desk` on
/// [`aterm_netplace::keeper::UNWATCHED_EVERY`].
#[cfg(any(target_os = "macos", target_os = "linux"))]
fn watch_routes(
    desk: &'static ObservationDesk,
    changes: std::io::Result<aterm_uds::network::RouteChanges>,
) -> Option<aterm_netplace::watch::Routes> {
    aterm_netplace::watch::routes(changes, || desk.input_changed(), || desk.unwatched())
}

/// THE OBSERVATION OWNER over the state root `state`, on its thread until `desk` closes. The
/// caller arms both of the place's watched inputs before this, so before its first read: the
/// folder's files ([`watch_files`], pointed at the folder as each read resolves it, before
/// reading) and the routes it reads live (`watch_routes`), which change with no file written
/// whenever the keeper's record stays the same, and which no keeper writes while none keeps.
/// Either missing, or the route socket failing later, puts the owner on
/// [`aterm_netplace::keeper::UNWATCHED_EVERY`].
/// `read` is the read itself (the window's: [`aterm_netplace::keeper::read_projected`]).
#[cfg(unix)]
fn observe<R>(
    desk: &ObservationDesk,
    state: &std::path::Path,
    files: Option<aterm_netplace::watch::Watch>,
    routes: Option<R>,
    mut read: impl FnMut() -> (Place, Option<u64>),
) {
    desk.run(files.is_some() && routes.is_some(), || {
        if let Some(files) = &files {
            files.point(&aterm_netplace::record::dir(state));
        }
        let (place, due) = read();
        (Arc::new(place), due)
    });
    drop((files, routes));
}

/// Stop notification and further reads when the owning application exits. An in-flight
/// filesystem read finishes before the worker can retire; this does not join on the GUI.
#[cfg(all(unix, not(test)))]
pub(crate) fn stop() {
    DESK.close();
}

/// THE AWAY ENTRY (`Family::NetworkPlace`, a level family, Info): one while `place` reads away
/// from the ALab LAN ([`words::away_entry`]), none otherwise. Never a `Message::now`.
pub(crate) fn away_entries(
    place: Option<&Place>,
    words: &Words<'_>,
) -> Vec<aterm_messages::exceptions::Entry> {
    use aterm_messages::exceptions::{Entry, Family};
    use aterm_messages::need::{Reason, Resurface, Then, Why};
    let Some(away) = place.and_then(|place| words::away_entry(place, words)) else {
        return Vec::new();
    };
    let mut entry = Entry::new(
        Family::NetworkPlace,
        "",
        "away",
        away.what,
        Why::OnlyYou(Reason::new(away.why)),
        Then::step(away.step),
    )
    .severity(aterm_messages::Severity::Info)
    .resurface(Resurface::WAIT)
    .log_line(&away.log);
    for line in &away.more {
        entry = entry.more(line);
    }
    vec![entry]
}

impl crate::App {
    /// Consume the changed level and its accepted snapshot together, without starting a read.
    pub(crate) fn follow_network_place(&mut self) {
        #[cfg(all(unix, not(test)))]
        self.follow_network_place_on(&DESK, &Words::now());
        #[cfg(all(not(unix), not(test)))]
        if UNKNOWN_PENDING.swap(false, std::sync::atomic::Ordering::AcqRel) {
            self.state_network_place(None, &Words::now());
        }
    }

    #[cfg(unix)]
    fn follow_network_place_on(&mut self, desk: &ObservationDesk, words: &Words<'_>) {
        if let Some(place) = desk.take_change() {
            self.state_network_place(place.as_deref(), words);
        }
    }

    /// `Family::NetworkPlace`'s whole answer for `place` at `words`.
    pub(crate) fn state_network_place(&mut self, place: Option<&Place>, words: &Words<'_>) {
        let entries = away_entries(place, words);
        self.except_replace(aterm_messages::exceptions::Family::NetworkPlace, &entries);
    }
}

/// The place as last read; `None` until the first read lands, where no state root resolves, or
/// off Unix. A read older than [`aterm_netplace::keeper::CACHE_FOR`] requests the next one from
/// the background owner, and this answers the last meanwhile. Readers share an immutable snapshot;
/// cloning its handle copies no route payload and formatting happens after unlocking the desk.
pub(crate) fn latest() -> Option<Arc<Place>> {
    #[cfg(test)]
    {
        ASKED.with(|asked| asked.set(asked.get() + 1));
        HELD.with(|held| held.borrow().clone())
    }
    #[cfg(all(unix, not(test)))]
    {
        let (place, start) = DESK.request();
        if start {
            if let Some(state) = aterm_types::dirs::state_dir() {
                follow(state);
            } else {
                DESK.failed_start();
            }
        }
        place
    }
    #[cfg(all(not(unix), not(test)))]
    {
        None
    }
}

/// The place as last read, starting no read: what paint takes, which runs on the render thread
/// and so never requests a read ([`latest`] does once its read is stale). The follow
/// lane's read on each change of the place's inputs, and every other caller's [`latest`], keep
/// it current; `None` until the first read lands, or off Unix. A unit test sees the place it
/// held, and its call is no read asked ([`reads_asked`]).
pub(crate) fn cached() -> Option<Arc<Place>> {
    #[cfg(test)]
    {
        HELD.with(|held| held.borrow().clone())
    }
    #[cfg(all(unix, not(test)))]
    {
        DESK.state.lock_or_recover().place.clone()
    }
    #[cfg(all(not(unix), not(test)))]
    {
        None
    }
}

/// `status`'s network fields for `place` and the lab's `seats` ([`words::status_fields`]); every
/// value `-` with no place.
pub(crate) fn status_fields_of(place: Option<&Place>, seats: Option<&words::Seats>) -> String {
    place.map_or_else(
        || words::STATUS_UNKNOWN.to_owned(),
        |place| words::status_fields(place, seats),
    )
}

/// `status`'s network fields now, `seats` the fleet glance's last read of this Mac's copy
/// (`fleet_glance::FleetGlance::seats`).
pub(crate) fn status_fields(seats: Option<&words::Seats>) -> String {
    status_fields_of(latest().as_deref(), seats)
}

/// One place JSON value as `sys` prints it.
fn value_of(field: words::Field) -> aterm_vitals::PlaceValue {
    use aterm_vitals::PlaceValue;
    match field {
        words::Field::Text(text) => PlaceValue::Text(text),
        words::Field::Count(n) => PlaceValue::Count(n),
        words::Field::Flag(flag) => PlaceValue::Flag(flag),
        words::Field::Absent => PlaceValue::Absent("not_sampled"),
        words::Field::Object(fields) => PlaceValue::Object(
            fields
                .into_iter()
                .map(|(key, value)| (key, value_of(value)))
                .collect(),
        ),
    }
}

/// `sys`'s place for `place` at `words`: its line ([`words::line`]), JSON object
/// ([`words::json_fields`]) and kind ([`place_kind`]).
pub(crate) fn sys_view_of(place: &Place, words: &Words<'_>) -> aterm_vitals::PlaceView {
    aterm_vitals::PlaceView {
        line: words::line(place, words),
        fields: words::json_fields(place)
            .into_iter()
            .map(|(key, value)| (key, value_of(value)))
            .collect(),
        kind: place_kind(place),
    }
}

/// The kind of network `place` is on, as the Machine row draws it (A4 §8): netplace's own
/// [`aterm_netplace::Kind`] wherever it has one, offline where it is offline, and still checking
/// where it has neither.
pub(crate) fn place_kind(place: &Place) -> aterm_vitals::PlaceKind {
    use aterm_netplace::Kind;
    use aterm_vitals::PlaceKind;
    match (place.at, place.kind) {
        (aterm_netplace::Where::Offline, _) => PlaceKind::Offline,
        (_, Some(Kind::Lan)) => PlaceKind::Lan,
        (_, Some(Kind::Hotspot { .. })) => PlaceKind::Hotspot,
        (_, Some(Kind::Cellular)) => PlaceKind::Cellular,
        (_, Some(Kind::Vpn)) => PlaceKind::Vpn,
        (_, None) => PlaceKind::Checking,
    }
}

/// `sys`'s place now; `None` until a place was read.
pub(crate) fn sys_view() -> Option<aterm_vitals::PlaceView> {
    latest().map(|place| sys_view_of(&place, &Words::now()))
}

/// Where `place` is, as the menu-bar menu's first line ends: the place
/// ([`words::where_words`]), and the internet only where it was measured down
/// ([`words::internet_short`]): `ALab LAN`, `remote (phone hotspot) · internet down`. An
/// internet nothing measured (the path is up, or nothing says) is no finding and adds no word.
pub(crate) fn identity_words_of(place: &Place) -> String {
    let at = words::where_words(place);
    if place.internet.state == aterm_netplace::InternetState::Down {
        format!("{at} \u{b7} {}", words::internet_short(place))
    } else {
        at
    }
}

/// [`identity_words_of`] the place now; `None` until a place was read.
pub(crate) fn identity_words() -> Option<String> {
    latest().map(|place| identity_words_of(&place))
}

#[cfg(all(test, unix))]
thread_local! {
    /// The evidence directory a test notes into for its own thread ([`hold_evidence_dir`]).
    static EVIDENCE_DIR: std::cell::RefCell<Option<std::path::PathBuf>> =
        const { std::cell::RefCell::new(None) };
}

/// A test's evidence directory for the notes made on its own thread (Unix: the evidence file is
/// Unix's, `aterm_netplace::heard`).
#[cfg(all(test, unix))]
pub(crate) fn hold_evidence_dir(dir: Option<std::path::PathBuf>) {
    EVIDENCE_DIR.with(|held| *held.borrow_mut() = dir);
}

/// The name suffixes only a LAN or a private resolver answers (RFC 6762's `.local`, RFC 8375's
/// `.home.arpa`, and the private suffixes home routers and offices hand out), checked without
/// the case.
const LAN_SUFFIXES: [&str; 8] = [
    ".local",
    ".lan",
    ".home",
    ".home.arpa",
    ".internal",
    ".localdomain",
    ".intranet",
    ".private",
];

/// Whether an answer from `host` says the internet answers: not a loopback, private, link-local
/// or unnamed host, and no name a LAN gives out (a single label such as `m22` or `proxy`, or one
/// under [`LAN_SUFFIXES`] such as `proxy.lan`): a local proxy's answer says nothing past this Mac
/// or its LAN. A name is not resolved here (the check runs as the reply is booked, and sends
/// nothing), so a public-looking name that points at a private address still counts.
pub(crate) fn host_is_internet(host: &str) -> bool {
    let host = host.trim_start_matches('[').trim_end_matches(']');
    if host.is_empty() || host.eq_ignore_ascii_case("localhost") {
        return false;
    }
    match host.parse::<std::net::IpAddr>() {
        Ok(std::net::IpAddr::V4(v4)) => {
            !(v4.is_loopback()
                || v4.is_private()
                || v4.is_link_local()
                || v4.is_unspecified()
                || (v4.octets()[0] == 100 && (v4.octets()[1] & 0xc0) == 64))
        }
        Ok(std::net::IpAddr::V6(v6)) => {
            !(v6.is_loopback()
                || v6.is_unspecified()
                || (v6.segments()[0] & 0xfe00) == 0xfc00
                || (v6.segments()[0] & 0xffc0) == 0xfe80)
        }
        Err(_) => {
            let name = host.trim_end_matches('.').to_ascii_lowercase();
            name.contains('.') && !LAN_SUFFIXES.iter().any(|suffix| name.ends_with(suffix))
        }
    }
}

/// How old a turn's reply may be and still be noted: the turn-end loop books a session's last
/// reply, which after a relaunch or a wake can be hours old and from another network.
pub(crate) const TURN_REPLY_FRESH_MS: u64 = 60_000;

/// NOTES AN INTERNET ANSWER into the place's evidence: `endpoint` answered (`ok`) or did not,
/// as `source` measured it at `at_ms` (unix ms: when the reply came, not when it was booked;
/// `aterm_netplace::heard::note_internet`, which coalesces), so an old reply counts only for the
/// stay it came in. An answer from a host that is not the internet's ([`host_is_internet`]) and
/// a turn's reply older than [`TURN_REPLY_FRESH_MS`] are noted nowhere. The note takes the
/// evidence file's lock, so it is written on a thread of its own; a unit test notes on its own
/// thread into the directory it held ([`hold_evidence_dir`]), else nowhere.
pub(crate) fn note_internet(
    ok: bool,
    source: aterm_netplace::InternetSource,
    endpoint: &str,
    at_ms: u64,
) {
    if !host_is_internet(endpoint) {
        return;
    }
    if source == aterm_netplace::InternetSource::TurnReply
        && aterm_netplace::unix_ms_now().saturating_sub(at_ms) > TURN_REPLY_FRESH_MS
    {
        return;
    }
    #[cfg(all(test, unix))]
    {
        if let Some(dir) = EVIDENCE_DIR.with(|held| held.borrow().clone()) {
            aterm_netplace::heard::note_internet(&dir, ok, source, endpoint, at_ms);
        }
    }
    #[cfg(all(unix, not(test)))]
    {
        let Some(state) = aterm_types::dirs::state_dir() else {
            return;
        };
        let endpoint = endpoint.to_owned();
        let _ = crate::qos::spawn_lane_thread(
            "aterm-netplace-note",
            crate::qos::Role::Background,
            move || {
                aterm_netplace::heard::note_internet(
                    &aterm_netplace::record::dir(&state),
                    ok,
                    source,
                    &endpoint,
                    at_ms,
                );
            },
        );
    }
    #[cfg(not(unix))]
    {
        let _ = (ok, source, at_ms);
    }
}

#[cfg(test)]
#[path = "net_place_tests.rs"]
mod tests;
