// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! This Mac's network place as the window reports it (`aterm_netplace`): `status`'s `net=…`
//! fields, `sys`'s `place` line and object, the network deck's summary and the menu bar's Network
//! row, every one in aterm-netplace's words, so none of them can say something else about where
//! this Mac is.
//!
//! The event loop reads no file for it: [`latest`] answers the place the last background read
//! made, and a read older than [`aterm_netplace::keeper::CACHE_FOR`] requests the next from its
//! sole observation owner; paint takes [`cached`], which requests nothing. [`start`]
//! starts this process's keeper candidate and registers the window with the owner that reads the
//! place within `FOLLOW_EVERY` or at its earlier dwell deadline and wakes the event loop on
//! an away-key transition
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
    following: FollowState,
    notice: Option<Arc<Notice>>,
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

    /// One owner alone performs reads. Requests arriving during I/O remain latched.
    fn run(&self, mut read: impl FnMut() -> Arc<Place>) {
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
                    let external = held.read_at.map_or(std::time::Duration::ZERO, |at| {
                        FOLLOW_EVERY.saturating_sub(now.saturating_duration_since(at))
                    });
                    let wait = held
                        .following
                        .wait(aterm_netplace::unix_ms_now())
                        .min(external);
                    if held.requested || wait.is_zero() {
                        held.requested = false;
                        break now;
                    }
                    held = self
                        .wake
                        .wait_timeout(held, wait)
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .0;
                }
            };
            let place = read();
            let mut held = self.state.lock_or_recover();
            if held.closed {
                return;
            }
            let changed = held
                .following
                .observe(&place, aterm_netplace::unix_ms_now());
            held.place = Some(place);
            held.read_at = Some(observed_at);
            held.retry_at = None;
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
/// already serve on-demand reads; registration enables its capped follow schedule and flushes
/// any accepted pending change. Unit tests compile no startup path: the root is the person's.
#[cfg(all(unix, not(test)))]
pub(crate) fn start(proxy: aterm_window::event_loop::EventLoopProxy<crate::Wake>) {
    let Some(state) = aterm_types::dirs::state_dir() else {
        return;
    };
    let _ = aterm_netplace::keeper::start(&state);
    DESK.register(Notice::Window(proxy));
    let _ = latest();
}

/// The maximum wait before resampling external facts; dwell deadlines can wake sooner.
#[cfg(any(unix, test))]
pub(crate) const FOLLOW_EVERY: std::time::Duration = std::time::Duration::from_secs(10);

#[cfg(any(unix, test))]
#[derive(Default)]
struct FollowState {
    stated: Option<Option<words::AwayKey>>,
    next_change_ms: Option<u64>,
}

#[cfg(any(unix, test))]
impl FollowState {
    fn observe(&mut self, place: &Place, now_ms: u64) -> bool {
        let projection = words::away_key_with_deadline(place, now_ms);
        let changed = self.stated != Some(projection.key);
        self.stated = Some(projection.key);
        self.next_change_ms = projection.next_change_ms;
        changed
    }

    /// Called with a fresh wall sample after publication and notification effects.
    fn wait(&self, now_ms: u64) -> std::time::Duration {
        self.next_change_ms.map_or(FOLLOW_EVERY, |at| {
            std::time::Duration::from_millis(at.saturating_sub(now_ms)).min(FOLLOW_EVERY)
        })
    }
}

/// Start the sole observer outside the desk lock. It serves on-demand reads without a window;
/// a registered window also enables `FOLLOW_EVERY` and earlier dwell boundaries. Each actual
/// away-key transition wakes that window; internet-only changes generate no notification.
#[cfg(all(unix, not(test)))]
fn follow(state: std::path::PathBuf) {
    let started = crate::qos::spawn_lane_thread(
        "aterm-netplace-follow",
        crate::qos::Role::Background,
        move || DESK.run(|| Arc::new(aterm_netplace::read(&state))),
    );
    if let Err(e) = started {
        DESK.failed_start();
        aterm_log::warn!("network place: the window's follow lane did not start: {e}");
    }
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
/// lane's read every `FOLLOW_EVERY`, and every other caller's [`latest`], keep it current; `None`
/// until the first read lands, or off Unix. A unit test sees the place it held, and its call is
/// no read asked ([`reads_asked`]).
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

/// `sys`'s place for `place` at `words`: its line ([`words::line`]) and JSON object
/// ([`words::json_fields`]).
pub(crate) fn sys_view_of(place: &Place, words: &Words<'_>) -> aterm_vitals::PlaceView {
    aterm_vitals::PlaceView {
        line: words::line(place, words),
        fields: words::json_fields(place)
            .into_iter()
            .map(|(key, value)| (key, value_of(value)))
            .collect(),
    }
}

/// `sys`'s place now; `None` until a place was read.
pub(crate) fn sys_view() -> Option<aterm_vitals::PlaceView> {
    latest().map(|place| sys_view_of(&place, &Words::now()))
}

/// The menu bar's Network row: the place's summary ([`words::summary`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct MenuLine {
    /// `Network — <summary>`.
    pub label: String,
}

/// [`MenuLine`] for `place` at `words`.
pub(crate) fn menu_line_of(place: &Place, words: &Words<'_>) -> MenuLine {
    MenuLine {
        label: format!("Network \u{2014} {}", words::summary(place, words)),
    }
}

/// The menu bar's Network row now; `None` until a place was read.
pub(crate) fn menu_line() -> Option<MenuLine> {
    latest().map(|place| menu_line_of(&place, &Words::now()))
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
