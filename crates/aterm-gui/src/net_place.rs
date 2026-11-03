// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! This Mac's network place as the window reports it (`aterm_netplace`): `status`'s `net=…`
//! fields, `sys`'s `place` line and object, the network deck's summary and the menu bar's Network
//! row, every one in aterm-netplace's words, so none of them can say something else about where
//! this Mac is.
//!
//! The event loop reads no file for it: [`latest`] answers the place the last background read
//! made, and a read older than [`aterm_netplace::keeper::CACHE_FOR`] starts the next on a thread
//! of its own; paint, on the render thread, takes [`cached`], which starts nothing. [`start`]
//! starts this process's keeper candidate, the first read, and the follow lane that reads the
//! place every `FOLLOW_EVERY` and marks the away entry's change for the event loop
//! ([`App::follow_network_place`]: one Info fact in the exceptions digest while this Mac is away
//! from the ALab LAN, none once it is back; never an alert, never NEEDS YOU). A unit test reads
//! nothing of the person's: it sees only the place it held ([`hold`]), on its own thread.

#[cfg(all(unix, not(test)))]
use std::sync::Mutex;
#[cfg(all(unix, not(test)))]
use std::time::Instant;

use aterm_netplace::Place;
use aterm_netplace::words::{self, Words};
#[cfg(all(unix, not(test)))]
use aterm_types::MutexExt as _;

/// The last place read, when it was read, and whether a read is out.
#[cfg(all(unix, not(test)))]
struct Desk {
    place: Option<Place>,
    read_at: Option<Instant>,
    out: bool,
}

#[cfg(all(unix, not(test)))]
static DESK: Mutex<Desk> = Mutex::new(Desk {
    place: None,
    read_at: None,
    out: false,
});

#[cfg(test)]
thread_local! {
    /// The place a test holds for its own thread ([`hold`]).
    static HELD: std::cell::RefCell<Option<Place>> = const { std::cell::RefCell::new(None) };
}

/// A test's place for the reporters on its own thread.
#[cfg(test)]
pub(crate) fn hold(place: Option<Place>) {
    HELD.with(|held| *held.borrow_mut() = place);
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
#[cfg(all(test, unix))]
pub(crate) struct HeldPlace(Option<Place>);

#[cfg(all(test, unix))]
impl Drop for HeldPlace {
    fn drop(&mut self) {
        hold(self.0.take());
    }
}

#[cfg(all(test, unix))]
pub(crate) fn hold_scoped(place: Option<Place>) -> HeldPlace {
    HeldPlace(HELD.with(|held| held.replace(place)))
}

/// STARTS THE PLACE as aterm starts: this process's keeper candidate over aterm's state root
/// (`aterm_netplace::keeper::start`, once per process), the first read, and the follow lane
/// ([`follow`]). A unit test starts none of them: the state root is the person's.
pub(crate) fn start() {
    #[cfg(unix)]
    {
        if cfg!(test) {
            return;
        }
        let Some(state) = aterm_types::dirs::state_dir() else {
            return;
        };
        let _ = aterm_netplace::keeper::start(&state);
        let _ = latest();
        #[cfg(not(test))]
        follow(state);
    }
}

/// How often the follow lane reads the place ([`follow`]).
#[cfg(all(unix, not(test)))]
pub(crate) const FOLLOW_EVERY: std::time::Duration = std::time::Duration::from_secs(10);

/// Set when the away entry's fact changed since the event loop last stated it.
static AWAY_CHANGED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(true);

/// What the away entry states, as the follow lane compares it: where this Mac is away on, and
/// since when; `None` while it states nothing.
#[cfg_attr(
    all(not(unix), not(test)),
    expect(
        dead_code,
        reason = "the follow lane reads the place's Unix-only files"
    )
)]
fn away_key(place: Option<&Place>, words: &Words<'_>) -> Option<(String, Option<u64>)> {
    let place = place?;
    let entry = words::away_entry(place, words)?;
    let since = match place.alab {
        aterm_netplace::Alab::Away { since_ms } => since_ms,
        _ => None,
    };
    Some((entry.log, since))
}

/// THE FOLLOW LANE: a background thread that reads the place every `FOLLOW_EVERY` into the
/// window's last read ([`latest`]) and marks [`AWAY_CHANGED`] when the away entry's fact moved, so
/// the event loop restates the exceptions digest's entry at its next turn.
#[cfg(all(unix, not(test)))]
fn follow(state: std::path::PathBuf) {
    let started = crate::qos::spawn_lane_thread(
        "aterm-netplace-follow",
        crate::qos::Role::Background,
        move || {
            let mut stated: Option<Option<(String, Option<u64>)>> = None;
            loop {
                let place = aterm_netplace::read(&state);
                let key = away_key(Some(&place), &Words::now());
                {
                    let mut desk = DESK.lock_or_recover();
                    desk.place = Some(place);
                    desk.read_at = Some(Instant::now());
                }
                if stated.as_ref() != Some(&key) {
                    stated = Some(key);
                    AWAY_CHANGED.store(true, std::sync::atomic::Ordering::Release);
                }
                std::thread::sleep(FOLLOW_EVERY);
            }
        },
    );
    if let Err(e) = started {
        aterm_log::warn!("network place: the window's follow lane did not start: {e}");
    }
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
    /// The away entry restated when the follow lane marked its fact moved: the whole answer of
    /// `Family::NetworkPlace` from the place as last read.
    pub(crate) fn follow_network_place(&mut self) {
        if AWAY_CHANGED.swap(false, std::sync::atomic::Ordering::AcqRel) {
            self.state_network_place(latest().as_ref(), &Words::now());
        }
    }

    /// `Family::NetworkPlace`'s whole answer for `place` at `words`.
    pub(crate) fn state_network_place(&mut self, place: Option<&Place>, words: &Words<'_>) {
        let entries = away_entries(place, words);
        self.except_replace(aterm_messages::exceptions::Family::NetworkPlace, &entries);
    }
}

/// The place as last read; `None` until the first read lands, where no state root resolves, or
/// off Unix. A read older than [`aterm_netplace::keeper::CACHE_FOR`] starts the next one, on a
/// background thread, and this answers the last meanwhile.
pub(crate) fn latest() -> Option<Place> {
    #[cfg(test)]
    {
        ASKED.with(|asked| asked.set(asked.get() + 1));
        HELD.with(|held| held.borrow().clone())
    }
    #[cfg(all(unix, not(test)))]
    {
        let mut desk = DESK.lock_or_recover();
        let due = !desk.out
            && desk
                .read_at
                .is_none_or(|at| at.elapsed() >= aterm_netplace::keeper::CACHE_FOR);
        if due && let Some(state) = aterm_types::dirs::state_dir() {
            desk.out = true;
            let started = crate::qos::spawn_lane_thread(
                "aterm-netplace-read",
                crate::qos::Role::Background,
                move || {
                    let place = aterm_netplace::read(&state);
                    let mut desk = DESK.lock_or_recover();
                    desk.place = Some(place);
                    desk.read_at = Some(Instant::now());
                    desk.out = false;
                },
            );
            if started.is_err() {
                desk.out = false;
                desk.read_at = Some(Instant::now());
            }
        }
        desk.place.clone()
    }
    #[cfg(all(not(unix), not(test)))]
    {
        None
    }
}

/// The place as last read, starting no read: what paint takes, which runs on the render thread
/// and so never spawns a read of its own ([`latest`] does once its read is stale). The follow
/// lane's read every `FOLLOW_EVERY`, and every other caller's [`latest`], keep it current; `None`
/// until the first read lands, or off Unix. A unit test sees the place it held, and its call is
/// no read asked ([`reads_asked`]).
pub(crate) fn cached() -> Option<Place> {
    #[cfg(test)]
    {
        HELD.with(|held| held.borrow().clone())
    }
    #[cfg(all(unix, not(test)))]
    {
        DESK.lock_or_recover().place.clone()
    }
    #[cfg(all(not(unix), not(test)))]
    {
        None
    }
}

/// `status`'s network fields for `place` ([`words::status_fields`]); every value `-` with none.
pub(crate) fn status_fields_of(place: Option<&Place>) -> String {
    place.map_or_else(|| words::STATUS_UNKNOWN.to_owned(), words::status_fields)
}

/// `status`'s network fields now.
pub(crate) fn status_fields() -> String {
    status_fields_of(latest().as_ref())
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

/// The menu bar's Network row: the place's summary ([`words::summary`]). Whether a fleet read's
/// no-route is this Mac's Local Network privacy is the read's own verdict, judged by each dial's
/// address against this network ([`words::on_link`], the survey's note the fleet glance reads),
/// so the row blames nothing and excuses nothing by where this Mac is.
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
