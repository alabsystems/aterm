// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Fabric bridge runtime (`aterm-link serve`).

macro_rules! stderr_line {
    ($($arg:tt)*) => {
        ::aterm_uds::stdout::stderr_line(::std::format_args!($($arg)*))
            .unwrap_or_else(|error| panic!("failed printing to stderr: {error}"))
    };
}

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::io;
use std::sync::Arc;
use std::time::{Duration, Instant};

use astream_broker::refill::{RefillStep, SharedRefillMeter};
use astream_broker::Record as BrokerRecord;
use astream_wire::WakeClass;
use aterm_hash::fnv1a_64;
use aterm_types::control_verbs::ScreenGen;
use aterm_types::fabric_grammar::{Bye, DeadReason, ANSWER_KINDS};

use crate::body::{via_ok, Body};
use crate::ctl::{Ctl, Reply, REQUEST_LINE_MAX};
use crate::fx::{BusCloser, Fx};
use crate::mailbox::{End, EventMark, Item, Mailbox, Pushed, ReplayPressure, Source};
use crate::pipeline::{HeldCommit, Link, Publish};
use crate::presence::{Fields, Marked, Mode, Slot};
use crate::render::trust_of;
use crate::state::{
    asks, Asked, BridgeLock, Deadline, InFlight, LogWitness, PublishIdentity, PublishPin,
    StandingHalt, StateDir, TopicCursor, ASKED_KEEP, DEADLINES_KEEP, INC_MAX,
};
use crate::subject::{self, Reject};
use crate::transport::{self, Backoff, Conn};
use crate::unverified_hold::OutboxLane;
use crate::wait_tally::Recorder;

#[path = "bridge_mirror.rs"]
mod mirror_run;

/// Hang detector for a required broker answer, including a locally scheduled broker.
pub(crate) const ACK_DEADLINE: Duration = Duration::from_secs(60);

/// The bound on each broker answer of the bridge's links and of `notify`'s.
pub(crate) fn ack_deadline() -> Duration {
    ack_remaining(Duration::ZERO)
}

pub(crate) fn ack_remaining(elapsed: Duration) -> Duration {
    static AT: std::sync::OnceLock<Duration> = std::sync::OnceLock::new();
    AT.get_or_init(|| ack_limit(aterm_types::dev_seam!("ATERM_LINK_ACK_DEADLINE_MS").as_deref()))
        .saturating_sub(elapsed)
}

/// A timeout fixture may shorten an answer that must never arrive. Successful broker
/// work keeps the production minute; an empty, malformed or zero override keeps it too.
fn ack_limit(milliseconds: Option<&std::ffi::OsStr>) -> Duration {
    milliseconds
        .and_then(|ms| crate::parse_uint::<u64>(ms.to_str()?))
        .filter(|ms| *ms > 0)
        .map_or(ACK_DEADLINE, Duration::from_millis)
}

#[cfg(test)]
#[path = "bridge_ack_deadline_tests.rs"]
mod ack_deadline_tests;

/// How long a subscription's reader reads nothing before it takes the link for dead
/// ([`spawn_reader`]'s live read deadline, [`End::Silent`]): astream's
/// [`astream_broker::IDLE_MARK_DEADLINE`], three of the broker's idle marks.
const LIVE_DEADLINE: Duration = astream_broker::IDLE_MARK_DEADLINE;

/// [`LIVE_DEADLINE`]'s seam, in milliseconds, in a seam-enabled build: the fabric_world
/// harness sets it to the hang detector on every instance it starts (its `LIVE_WAIT`), since
/// its broker runs on the test's loaded machine.
const LIVE_DEADLINE_SEAM: &str = "ATERM_LINK_LIVE_DEADLINE_MS";

fn live_deadline() -> Duration {
    static AT: std::sync::OnceLock<Duration> = std::sync::OnceLock::new();
    *AT.get_or_init(|| {
        aterm_types::dev_seam!(LIVE_DEADLINE_SEAM)
            .and_then(|ms| crate::parse_uint::<u64>(ms.to_str()?))
            .map_or(LIVE_DEADLINE, Duration::from_millis)
    })
}

const LINK_REFRESH: Duration = Duration::from_secs(2);

const LINK_MOVE_GAP: Duration = Duration::from_millis(250);

/// How long a failed redial [`Bridge::dial_failed`] keeps quiet (its reason was said
/// already) before it says the outage again, with the redials it stood for.
const LINK_DOWN_RETELL: Duration = Duration::from_secs(30);

/// How far below the head a session's refill starts when it has no `seen` watermark (it never
/// ran `inbox seen`): `Bridge::refill` and the attach's shared refill (`refill_together`) both
/// read its lane from there, so their scans cross whatever the log holds in that span.
pub const REFILL_FLOOR_SPAN: u64 = 4096;

// The attach's shared refill read is weighed by astream's `SharedRefillMeter` at its defaults
// (`Bridge::refill_node`). Its `REFILL_ROW_COST`, what one record the read carries and offers to
// no session costs in records the broker scans, was measured here: on an Apple M5 Max at the
// `opt-test` profile, against the broker in the test's own process (2026-10-27, two runs of
// five repetitions, at 1-minute loads of 9 and 16), a lane's `Fetch` scanned 7.7-10.7 ns a
// record over 200,000 `ev` records, and the shared read carried 100,000 mail records of lanes
// it read for no one at 1.48-2.13 us a record, 158-255 records' scan each. A cost off by a
// factor of two moves the hand-on by a page or two (256 records carried each). Its slack, how
// far what the read carried may pass what it saved before each session reads on alone, is one
// page's scan: about half a millisecond at the rate above, or some 340 records carried for no
// session. astream defines that slack from the page (`FETCH_SCAN_MAX`); the build holds it
// there, so a pin bump that parts the two stops this crate compiling.
const _: () = assert!(
    astream_broker::refill::DEFAULT_REFILL_SPLIT_SLACK
        == astream_broker::broker::FETCH_SCAN_MAX as u64
);

const ROSTER_REFRESH: Duration = Duration::from_millis(2_000);

const FAST_TIMERS_SEAM: &str = aterm_types::domain::ENV_DEV_FAST_TIMERS;

const FAST_ROSTER_REFRESH: Duration = Duration::from_millis(200);

/// Returns `true` when [`FAST_TIMERS_SEAM`] is set in a seam-enabled build.
pub(crate) fn fast_timers() -> bool {
    static FAST: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *FAST.get_or_init(|| aterm_types::dev_seam!(FAST_TIMERS_SEAM).is_some())
}

fn roster_refresh() -> Duration {
    if fast_timers() {
        FAST_ROSTER_REFRESH
    } else {
        ROSTER_REFRESH
    }
}

const UNSERVED_PARK: Duration = Duration::from_secs(60);

/// The first wait, on the wall clock (ms), after a try at a halt request or record owed failed
/// with the link up (`Bridge::halt_owed_now`); each failure after doubles it.
const HALT_RETRY_MIN_MS: u64 = 60 * 1000;

/// The longest such wait: an hour.
const HALT_RETRY_MAX_MS: u64 = 60 * 60 * 1000;

pub(crate) const GHOST_SWEEP: Duration = Duration::from_secs(60);

const PRESENCE_VIEW_REOPEN: Duration = astream_broker::IDLE_MARK_DEADLINE;

const CONFIRM_SCAN_MAX: u64 = astream_broker::broker::TAIL_CHUNK as u64;

const CONFIRM_ROWS: u32 = 32;

const SAY_REPLAY_PAGES: usize = 4;

const SAY_REPLAY_PAGE: u32 = 256;

struct SayReplay {
    pages_left: usize,
    refill_at: Instant,
    after: Option<String>,
    page: u32,
}

impl SayReplay {
    fn new(now: Instant) -> Self {
        Self {
            pages_left: SAY_REPLAY_PAGES,
            refill_at: now + roster_refresh(),
            after: None,
            page: SAY_REPLAY_PAGE,
        }
    }

    fn refill(&mut self, now: Instant) {
        if now >= self.refill_at {
            self.pages_left = SAY_REPLAY_PAGES;
            self.refill_at = now + roster_refresh();
        }
    }

    fn take(&mut self, owed: &BTreeSet<String>) -> Option<String> {
        if self.pages_left == 0 {
            return None;
        }
        let sid = self
            .after
            .as_ref()
            .and_then(|after| {
                owed.range((
                    std::ops::Bound::Excluded(after.clone()),
                    std::ops::Bound::Unbounded,
                ))
                .next()
            })
            .or_else(|| owed.iter().next())?
            .clone();
        self.pages_left -= 1;
        self.after = Some(sid.clone());
        Some(sid)
    }
}

fn may_take_replay_page(mailbox: &Mailbox, roster: bool, taken: usize) -> bool {
    if !roster && taken > 0 {
        return false;
    }
    match mailbox.replay_pressure() {
        ReplayPressure::Clear => true,
        ReplayPressure::Pending => roster && taken == 0,
        ReplayPressure::Urgent => false,
    }
}

const SAY_CURSOR_CHECKPOINT_SHARDS: u64 = 256;

const UNHANDLED_KEEP: usize = 512;

const HELLO_REFILL: &str = "refill=in,say";

fn unhandled_floor(queue: &VecDeque<(u64, u64)>) -> Option<u64> {
    queue.iter().map(|&(_, off)| off).min()
}

fn note_unhandled(queue: &mut VecDeque<(u64, u64)>, id: u64, off: u64) {
    if queue.iter().any(|&(i, o)| i >= id && o <= off) {
        return;
    }
    queue.push_back((id, off));
}

fn prune_handled(queue: &mut VecDeque<(u64, u64)>, seen: u64) {
    queue.retain(|&(id, _)| id > seen);
}

fn floors_of(by_topic: &BTreeMap<String, VecDeque<(u64, u64)>>) -> BTreeMap<String, u64> {
    by_topic
        .iter()
        .filter_map(|(t, q)| unhandled_floor(q).map(|f| (t.clone(), f)))
        .collect()
}

const GHOST_AFTER: Duration = Duration::from_secs(300);

/// How long a subject a departed session held stays charged to this node, once its
/// presence row reads `exited` (or is gone) and nothing is owed on it, before the
/// sweep gives it back ([`Bridge::reclaim_subjects`]). The presence rule is
/// `last_only`, so no retention window asks for longer.
pub(crate) const RECLAIM_AFTER: Duration = GHOST_AFTER;

const RECLAIM_AFTER_SEAM: &str = "ATERM_LINK_RECLAIM_AFTER_MS";

fn reclaim_after() -> Duration {
    static AT: std::sync::OnceLock<Duration> = std::sync::OnceLock::new();
    *AT.get_or_init(|| {
        aterm_types::dev_seam!(RECLAIM_AFTER_SEAM)
            .and_then(|ms| crate::parse_uint::<u64>(ms.to_str()?))
            .map_or(RECLAIM_AFTER, Duration::from_millis)
    })
}

/// How often the sweep runs while it owes one: [`GHOST_SWEEP`], or sooner when the
/// reclaim's own wait is shorter.
fn ghost_sweep() -> Duration {
    GHOST_SWEEP.min(reclaim_after())
}

const SELF_ACK_KEEP: usize = 4096;
/// The most offsets [`Bridge::claims`] holds: far past what one attach's replay and retries
/// publish or settle.
const CLAIMS_KEEP: usize = 4096;

const VERDICT_KINDS: [WakeClass; 2] = [WakeClass::Undeliverable, WakeClass::Expired];

const WAITING_KINDS: [&str; 2] = ["ask", "task"];

const NEW_LANES_PER_NODE: usize = 64;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum NoLane {
    Undrained,
    Capped,
    /// Another Mac's lane, with the mesh on, while the node service's hand-off cannot route to
    /// it ([`NoHandoff`]).
    Handoff(NoHandoff),
}

/// Why no other Mac is routed to, with the mesh on (§11 C3): the node service's hand-off, which
/// names the members, is missing, stale or unreadable. Routing to another Mac fails closed: a
/// post to one is retired `unroutable`, and an answer or receipt to one is recorded unsent, each
/// with this word on the bus (`why=` on an `undeliverable` record, `reason=` on an `unacked` or
/// `unnotified` one) and the reason in full on stderr.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum NoHandoff {
    /// No hand-off: the node service has not written one (it is not running, or its mail pushes
    /// have not started).
    Missing,
    /// The hand-off names another node as this Mac's: written before this Mac's node key changed,
    /// and not rewritten since.
    Stale,
    /// The hand-off cannot be read, or says what this build cannot parse.
    Unreadable,
}

impl NoHandoff {
    /// The word the bus records it with.
    const fn word(self) -> &'static str {
        match self {
            Self::Missing => "hand-off-missing",
            Self::Stale => "hand-off-stale",
            Self::Unreadable => "hand-off-unreadable",
        }
    }
}

const DEADLINE_RETRY: Duration = Duration::from_millis(250);

fn deadline_wake(
    now: Instant,
    now_ms: u64,
    earliest_at: Option<u64>,
    roster_due: Instant,
    retry_overdue: bool,
) -> Option<Instant> {
    let until = Duration::from_millis(earliest_at?.saturating_sub(now_ms));
    let until = if retry_overdue && until.is_zero() {
        DEADLINE_RETRY
    } else {
        until
    };
    Some(now + until.min(roster_due.saturating_duration_since(now)))
}

fn seen_watermark(off: Option<u64>, floor: Option<Option<u64>>) -> Option<u64> {
    match floor {
        Some(floor) => floor.and_then(|f| f.checked_sub(1)),
        None => off,
    }
}

/// Whether `item` came off the bus, not aterm's push lane, so it is no reply a held inbox
/// commit could ride and sends the commit before it is handled ([`Bridge::hold_inbox_commit`]).
/// aterm closing the lane is aterm's, and the wind-down lands the commit itself.
fn off_the_bus(item: &Item) -> bool {
    !matches!(item, Item::Event(_) | Item::Closed(Source::Aterm))
}

fn idle_wait(
    now: Instant,
    roster_due: Instant,
    deadline_due: Option<Instant>,
    ghost_due: Option<Instant>,
) -> Duration {
    let next = [Some(roster_due), deadline_due, ghost_due]
        .into_iter()
        .flatten()
        .min()
        .expect("the roster always has a deadline");
    next.saturating_duration_since(now)
}

fn wait_for_retry_or_aterm_close(
    fx: &Fx,
    mailbox: &Mailbox,
    backoff: Duration,
    mut on_event: impl FnMut(&str),
) -> bool {
    let deadline = fx.now() + backoff;
    loop {
        let remaining = deadline.saturating_duration_since(fx.now());
        match fx.take(mailbox, remaining) {
            Some(Item::Closed(Source::Aterm)) => return true,
            None => return false,
            Some(item) => {
                if let Item::Event(line) = &item {
                    on_event(line);
                    #[cfg(test)]
                    if fx.mutant(crate::sim::world::Mutant::EventEndsTheBackoff) {
                        return false;
                    }
                }
                if fx.now() >= deadline {
                    return false;
                }
            }
        }
    }
}

const DEADLINE_SWEEP_MAX: usize = 16;

const EXPIRED_KEEP: usize = 4096;

const REASON_TOKEN_MAX: usize = 32;

/// The `roster=<seq>` token of a `sessions bridge` or `outbox` reply's header: `None` from an
/// aterm that sends none.
fn roster_token(header: &str) -> Option<u64> {
    header
        .split(' ')
        .find_map(|t| t.strip_prefix("roster="))
        .and_then(crate::parse_uint)
}

fn parse_bridge_roster<'a>(
    rows: impl Iterator<Item = &'a str>,
) -> io::Result<(BTreeMap<u64, String>, BTreeMap<String, String>)> {
    let mut locals = BTreeMap::new();
    let mut epochs = BTreeMap::new();
    for row in rows {
        let mut cols = row.split_whitespace();
        let (Some(local), Some(sid), Some(nonce)) = (cols.next(), cols.next(), cols.next()) else {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "malformed bridge roster row",
            ));
        };
        if !cols.all(|extra| extra.split_once('=').is_some_and(|(k, _)| !k.is_empty())) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "malformed bridge roster row",
            ));
        }
        if local.is_empty() || !local.bytes().all(|b| b.is_ascii_digit()) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "invalid bridge roster local id",
            ));
        }
        let local = local.parse::<u64>().map_err(|_| {
            io::Error::new(io::ErrorKind::InvalidData, "invalid bridge roster local id")
        })?;
        let nonce = nonce.strip_prefix("nonce=").ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidData, "missing bridge roster nonce")
        })?;
        if sid == "-"
            || !aterm_codec::hex::is_lower(nonce, 32)
            || locals.insert(local, sid.to_string()).is_some()
            || epochs.insert(sid.to_string(), nonce.to_string()).is_some()
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "invalid bridge roster identity",
            ));
        }
    }
    Ok((locals, epochs))
}

/// Maximum body byte size carried by a single `inbox get @<off>` response (256 KiB).
pub const FETCH_BODY_MAX: usize = 256 * 1024;

/// How the bridge attached to the `aterm` endpoint and what capabilities it holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Attachment {
    /// Connected via inherited verb and push file descriptors (`Scope::Bridge`).
    Inherited,
    /// Connected via explicit `--sock` and owner token in read/presence observer mode.
    Observer,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
enum EndpointHold {
    #[default]
    Free,
    Fleet,
    Local,
    Both,
    Unsaid,
}

impl EndpointHold {
    fn of(held: bool, origin: Option<&str>) -> Self {
        if !held {
            return Self::Free;
        }
        let Some(list) = origin.filter(|list| origin_list_ok(list)) else {
            return Self::Unsaid;
        };
        let fleet = list.split(',').any(|word| word == "fleet");
        let other = list.split(',').any(|word| word != "fleet");
        match (fleet, other) {
            (true, false) => Self::Fleet,
            (true, true) => Self::Both,
            (false, _) => Self::Local,
        }
    }

    const fn fleet(self) -> bool {
        matches!(self, Self::Fleet | Self::Both | Self::Unsaid)
    }
}

fn origin_list_ok(list: &str) -> bool {
    const ORDER: [&str; 4] = ["person", "remote", "fleet", "local"];
    let mut next = 0;
    for word in list.split(',') {
        let Some(at) = ORDER[next..].iter().position(|known| *known == word) else {
            return false;
        };
        next += at + 1;
    }
    true
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct StatusSample {
    hold: EndpointHold,
    detail: String,
    agent: Option<String>,
    agent_deferred: bool,
    /// The screen's generation the read named (`gen=`), `None` for `-` (the terminal busy).
    screen: Option<ScreenGen>,
    /// The session's meaning fields, `role= attention= user_title=` as `meta` prints them, that
    /// a `sessions status` row carried ([`meta_words`]); `None` for a row that left them out
    /// (a writer held the session's meta) and for every `@<sid> status` read.
    meta: Option<String>,
}

impl StatusSample {
    fn apply_presence(&self, slot: &mut Slot) {
        slot.fields.set_detail(Some(&self.detail));
        if !self.agent_deferred {
            slot.saw_agent(self.agent.as_deref());
        }
        slot.saw_screen(self.screen);
        if let Some(meta) = &self.meta {
            slot.fields.read_meta(meta);
            slot.sampled = true;
        }
    }
}

/// The three `meta` words a presence slot folds ([`Fields::read_meta`]) that aterm says beside a
/// `sessions status` row and an `events` watch's `sub` ack (critic item E05).
const META_WORDS: [&str; 3] = ["role", "attention", "user_title"];

/// The [`META_WORDS`] among `tokens`, joined as a `meta` header [`Fields::read_meta`] reads:
/// `Some(None)` when none is said, `None` when one is said twice or some but not all are (no
/// reading of a part), and every other token ignored.
fn meta_words<'a>(tokens: impl Iterator<Item = &'a str>) -> Option<Option<String>> {
    let mut said: [Option<&str>; META_WORDS.len()] = [None; META_WORDS.len()];
    for tok in tokens {
        let Some(at) = meta_word(tok) else {
            continue;
        };
        if said[at].replace(tok).is_some() {
            return None;
        }
    }
    match said {
        [None, None, None] => Some(None),
        [Some(role), Some(attention), Some(user_title)] => {
            Some(Some(format!("{role} {attention} {user_title}")))
        }
        _ => None,
    }
}

/// Which of the [`META_WORDS`] `tok` says (`<word>=…`), by its index.
fn meta_word(tok: &str) -> Option<usize> {
    META_WORDS.into_iter().position(|key| {
        tok.strip_prefix(key)
            .is_some_and(|rest| rest.starts_with('='))
    })
}

enum BatchSample {
    Ready(StatusSample),
    Stale,
}

enum StatusBatch {
    Rows(BTreeMap<u64, BatchSample>),
    Unavailable,
}

fn parse_batch_status<'a>(
    local: u64,
    tokens: impl Iterator<Item = &'a str>,
) -> Option<StatusSample> {
    let (mut sid, mut revision, mut hold, mut detail, mut agent) = (None, None, None, None, None);
    let mut origin = None;
    let mut agent_deferred = false;
    let mut screen: Option<Option<ScreenGen>> = None;
    let mut meta: Vec<&str> = Vec::new();
    for tok in tokens {
        if meta_word(tok).is_some() {
            // The meaning fields (critic item E05), each once and all three or none
            // ([`meta_words`]).
            meta.push(tok);
        } else if let Some(v) = tok.strip_prefix("sid=") {
            if v.is_empty()
                || !v.bytes().all(|b| b.is_ascii_digit())
                || sid.replace(v.parse::<u64>().ok()?).is_some()
            {
                return None;
            }
        } else if let Some(v) = tok.strip_prefix("revision=") {
            if v.is_empty()
                || !v.bytes().all(|b| b.is_ascii_digit())
                || revision.replace(v.parse::<u64>().ok()?).is_some()
            {
                return None;
            }
        } else if let Some(v) = tok.strip_prefix("hold=") {
            let value = match v {
                "0" => Some(false),
                "1" => Some(true),
                _ => None,
            }?;
            if hold.replace(value).is_some() {
                return None;
            }
        } else if let Some(v) = tok.strip_prefix("hold_origin=") {
            // Closed, and contradiction-checked below: a hold says whose it
            // is, and no hold has an origin. `fleet,local` is both slots;
            // `person` the window's Take Over, `remote` another Mac's drive
            // (`origin_list_ok`).
            if !(v == "-" || origin_list_ok(v)) || origin.replace(v).is_some() {
                return None;
            }
        } else if let Some(v) = tok.strip_prefix("agent=") {
            if v.is_empty() || agent.replace(v.to_string()).is_some() {
                return None;
            }
        } else if tok == "agent_deferred=1" {
            if agent_deferred {
                return None;
            }
            agent_deferred = true;
        } else if let Some(v) = tok.strip_prefix("gen=") {
            // The screen's generation, `-` while the terminal was busy: said once, and
            // nothing else.
            let named = match v {
                "-" => None,
                v => Some(ScreenGen::parse(v)?),
            };
            if screen.replace(named).is_some() {
                return None;
            }
        } else {
            let v = tok.strip_prefix("detail=")?;
            if detail.replace(v.to_string()).is_some() {
                return None;
            }
        }
    }
    let _revision = revision?;
    let agent = match (agent, agent_deferred) {
        (Some(word), false) => Some(word),
        (None, true) => None,
        _ => return None,
    };
    let hold = match (hold?, origin) {
        (held, None) => EndpointHold::of(held, None),
        (false, Some("-")) => EndpointHold::Free,
        (true, Some(origin)) if origin_list_ok(origin) => EndpointHold::of(true, Some(origin)),
        _ => return None,
    };
    let meta = meta_words(meta.into_iter())?;
    (sid == Some(local)).then_some(StatusSample {
        hold,
        detail: detail?,
        agent,
        agent_deferred,
        screen: screen?,
        meta,
    })
}

fn parse_batch_rows<'a>(
    lines: impl Iterator<Item = &'a str>,
    locals: &BTreeMap<u64, String>,
    epochs: &BTreeMap<String, String>,
) -> BTreeMap<u64, BatchSample> {
    let mut rows = BTreeMap::new();
    let mut seen = BTreeSet::new();
    for line in lines {
        let mut words = line.split_whitespace();
        let Some(local) = words
            .next()
            .filter(|w| !w.is_empty() && w.bytes().all(|b| b.is_ascii_digit()))
            .and_then(|w| w.parse::<u64>().ok())
        else {
            continue;
        };
        let Some(expected_sid) = locals.get(&local) else {
            continue;
        };
        if !seen.insert(local) {
            rows.insert(local, BatchSample::Stale);
            continue;
        }
        let (Some(sid), Some(nonce)) = (words.next(), words.next()) else {
            continue;
        };
        if sid != expected_sid || epochs.get(expected_sid).map(String::as_str) != Some(nonce) {
            rows.insert(local, BatchSample::Stale);
            continue;
        }
        if let Some(sample) = parse_batch_status(local, words) {
            rows.insert(local, BatchSample::Ready(sample));
        }
    }
    rows
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Fault {
    None,
    LoseAtermBeforeDeliver,
    FailStatusWhileMarked,
    FailPostPublishWhileMarked,
    DropSessionExitedWhileMarked,
    DropPostEventWhileMarked,
    SkipRosterOutboxWhileMarked,
    OversizeDeliverLine,
    SweepGhostsAtOnceWhileMarked,
    ExpireDeadlinesAtOnceWhileMarked,
    DropMetaEventWhileMarked,
    ParkRetireWhileMarked,
}

impl Fault {
    fn from_env(state: &StateDir) -> Self {
        let Some(fault) = aterm_types::dev_seam!("ATERM_LINK_FAULT") else {
            return Fault::None;
        };
        if state.root().join("fault-fired").exists() {
            return Fault::None;
        }
        match fault.to_str() {
            Some("lose-aterm-before-deliver") => Fault::LoseAtermBeforeDeliver,
            Some("fail-status-while-marked") => Fault::FailStatusWhileMarked,
            Some("fail-post-publish-while-marked") => Fault::FailPostPublishWhileMarked,
            Some("drop-session-exited-while-marked") => Fault::DropSessionExitedWhileMarked,
            Some("drop-post-event-while-marked") => Fault::DropPostEventWhileMarked,
            Some("skip-roster-outbox-while-marked") => Fault::SkipRosterOutboxWhileMarked,
            Some("oversize-deliver-line") => Fault::OversizeDeliverLine,
            Some("sweep-ghosts-at-once-while-marked") => Fault::SweepGhostsAtOnceWhileMarked,
            Some("expire-deadlines-at-once-while-marked") => {
                Fault::ExpireDeadlinesAtOnceWhileMarked
            }
            Some("drop-meta-event-while-marked") => Fault::DropMetaEventWhileMarked,
            Some("park-retire-while-marked") => Fault::ParkRetireWhileMarked,
            other => {
                stderr_line!(
                    "aterm-link: ATERM_LINK_FAULT={:?} names no fault this bridge has; \
                     nothing is armed",
                    other.unwrap_or("<not UTF-8>")
                );
                Fault::None
            }
        }
    }

    /// Mark the fault spent, for [`Fault::from_env`] of a later bridge of this state dir and for
    /// the test that armed it: a dev seam's marker, whole by its rename and flushed by nothing,
    /// as `notify`'s is, since no power loss falls inside a test.
    fn fired(self, state: &StateDir) {
        let _ = aterm_uds::atomic_file::write_flushed(
            &state.root().join("fault-fired"),
            b"1\n",
            0o600,
            None,
        );
        stderr_line!("aterm-link: ATERM_LINK_FAULT={self:?} fired");
    }
}

/// Minted capability line parsed from `--cap-file` via [`astream_cap::capfile`].
pub use astream_cap::capfile::Line as Cap;

/// Reads and parses a `--cap-file` into capability lines through astream's owner-only ring reader
/// ([`astream_cap::capfile::read_private_ring`]). A cap is authority, so a file others can read, a
/// symlink and a FIFO are each refused at once, every refusal starting with `path` and saying what
/// to do. The file is what is judged, never its folder: a person keeps a cap where they choose.
///
/// # Errors
/// A refused or absent file, or a ring that does not parse (`InvalidData`, naming `path:line`).
pub fn read_cap_file(path: &str) -> io::Result<Vec<Cap>> {
    astream_cap::capfile::read_private_ring(path).map_err(|e| {
        if e.kind() == io::ErrorKind::InvalidInput {
            io::Error::new(
                e.kind(),
                format!(
                    "{e}: move it aside, then `aterm fabric on` mints a node's ring and `aterm \
                     fabric mint-for <h-name> --out <cap>` a person's"
                ),
            )
        } else {
            e
        }
    })
}

/// Whether a capability read failed without establishing an absent or malformed ring.
/// A rule refusal or unavailable I/O never authorizes replacing the file.
pub(crate) fn cap_refused(error: &io::Error) -> bool {
    !matches!(
        error.kind(),
        io::ErrorKind::NotFound | io::ErrorKind::InvalidData
    )
}

/// Write a test's cap file as every aterm that mints one does: owner-only.
#[cfg(test)]
pub(crate) fn write_owner_only(path: &std::path::Path, ring: impl AsRef<[u8]>) {
    use std::os::unix::fs::PermissionsExt as _;
    std::fs::write(path, ring).expect("write a cap file");
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).expect("chmod a cap");
}

/// Reads all `--cap-file` paths in order into a single capability ring.
pub fn read_ring(cap_files: &[String]) -> io::Result<Vec<Cap>> {
    let mut ring = Vec::new();
    for path in cap_files {
        ring.extend(read_cap_file(path)?);
    }
    Ok(ring)
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct LinkReport {
    up: bool,
    reason: &'static str,
    rtt_ms: Option<u64>,
    reported_at: Option<Instant>,
}

impl LinkReport {
    const fn new() -> Self {
        Self {
            up: false,
            reason: "",
            rtt_ms: None,
            reported_at: None,
        }
    }

    fn ack_wants_report(&self, rtt_ms: u64, now: Instant) -> bool {
        if !self.up {
            return true;
        }
        let since = self
            .reported_at
            .map_or(Duration::MAX, |t| now.saturating_duration_since(t));
        if since >= LINK_REFRESH {
            return true;
        }
        let moved = self
            .rtt_ms
            .is_none_or(|last| rtt_ms > last.saturating_mul(2) || last > rtt_ms.saturating_mul(2));
        moved && since >= LINK_MOVE_GAP
    }

    fn down_wants_report(&self, reason: &str) -> bool {
        self.up || self.reason != reason || self.reported_at.is_none()
    }
}

/// The failed redials [`Bridge::dial_failed`] has kept quiet since it last said the link down.
#[derive(Debug, Clone, Copy, Default)]
struct QuietDials {
    said_at: Option<Instant>,
    kept: u64,
}

fn link_reason(e: &io::Error, other: &'static str) -> &'static str {
    use astream_broker::proto::{ERR_PROTOCOL, ERR_RECORD_REFUSED, ERR_TOO_MANY_CONNS};
    use io::ErrorKind as K;
    if astream_broker::protocol_mismatch(e).is_some() {
        return "protocol";
    }
    if let Some(refusal) = astream_broker::broker_refusal(e) {
        if refusal.is_expired() {
            return "expired";
        }
        if refusal.is_poisoned() {
            return "poisoned";
        }
        return match refusal.code {
            ERR_TOO_MANY_CONNS => "busy",
            ERR_RECORD_REFUSED => "record",
            ERR_PROTOCOL => "protocol",
            _ => other,
        };
    }
    match e.kind() {
        K::NotFound => "no-socket",
        K::ConnectionRefused => "refused",
        K::PermissionDenied => "denied",
        K::TimedOut | K::WouldBlock => "no-ack",
        K::UnexpectedEof
        | K::BrokenPipe
        | K::ConnectionReset
        | K::ConnectionAborted
        | K::NotConnected => "closed",
        K::Other => other,
        _ => "error",
    }
}

fn sent_past_an_owed_ack(e: &io::Error) -> bool {
    let Some(owed) = astream_broker::acks_owed(e) else {
        return false;
    };
    stderr_line!(
        "aterm-link: BUG: a request was sent past {} unread pipelined ack(s) and astream \
         refused it, sending nothing: {e}; the connection is dropped and redialled",
        owed.owed
    );
    true
}

const fn face_name(source: Source) -> &'static str {
    match source {
        Source::Fleet => "fleet",
        Source::Halt => "halt",
        Source::Inbox => "inbox",
        Source::Say => "say",
        Source::Aterm => "aterm",
    }
}

const fn cut_reason(source: Source) -> &'static str {
    match source {
        Source::Fleet => "cut-fleet",
        Source::Halt => "cut-halt",
        Source::Inbox => "cut-inbox",
        Source::Say => "cut-say",
        Source::Aterm => "closed",
    }
}

fn refused_for_good(e: &io::Error) -> bool {
    astream_broker::broker_refusal(e).is_some_and(|r| r.is_permanent() && !r.is_expired())
}

/// Whether aterm's answer to a landed post's `outbox sent`, or a landed receipt's `deliver
/// <sid> receipt=<rid>`, says it left aterm's outbox: `OK` retired it (a receipt's `OK` also
/// answers one aterm no longer owes), and `ERR no such post` or `ERR no such session` say aterm
/// holds it no more. Every other answer leaves it queued at its pin (aterm-gui-fabric's
/// `retire_post` and `deliver_receipt` answer `ERR busy update-committing`, the outbox
/// untouched, while a seamless update's commit fences retirements), so its in-flight entry
/// stands for the drain's retry ([`Bridge::land_group`]).
fn left_the_outbox(reply: &Reply) -> bool {
    reply.ok() || matches!(reply.header(), "ERR no such post" | "ERR no such session")
}

/// Whether `e` is a broker's refusal of a write behind a GAP FENCE (`gap-fenced=<seq>`): refused
/// for a write sent before it, not for its own sake.
fn gap_fenced(e: &io::Error) -> bool {
    astream_broker::broker_refusal(e)
        .is_some_and(|r| astream_broker::sticky_fence::parse_gap_fenced_seq(&r.msg).is_some())
}

fn refusal_is_ack(e: &io::Error) -> bool {
    astream_broker::broker_refusal(e).is_some_and(|r| {
        r.code != astream_broker::proto::ERR_TOO_MANY_CONNS && !r.is_expired() && !r.is_poisoned()
    })
}

/// WHAT ONE READ OF ONE OFFSET FOUND ([`Bridge::read_at`]).
#[derive(Debug)]
enum ReadAt {
    /// The read's page: at most the one record asked for, the first of its filter at or above
    /// the offset, a record retention holds below the filter's oldest included.
    Rows(Vec<BrokerRecord>),
    /// CUT, never absent: the offset lies below the filter's oldest (astream's
    /// `retained-from=<floor>`, `floor` above the offset), and the told read of a broker that
    /// tells fetches (`Client::supports_told_fetch`) found no record of the filter there.
    /// Whatever record of it was there, retention gave up.
    Cut(u64),
    /// UNTOLD: the offset lies below the filter's oldest `floor` and the broker does not tell
    /// fetches (one built before told fetches), so its refusal says nothing of what retention
    /// still holds there: a record of the filter at the offset may be held, unread.
    Untold(u64),
    /// UNASKED: the offset lies below the filter's oldest, and the told read could not be
    /// made on its connection of its own (its dial, its `Hello`, its log not the link's, its
    /// read): no answer about the record there. Not judged as the link's: the link stands,
    /// and each caller says what an unanswered read is to it ([`Bridge::read_at`]).
    Unasked(io::Error),
}

/// The plain read of one offset `off`, judged: its rows; or, where astream refused it below
/// its filter's oldest (`retained-from=<oldest>` with `<oldest>` above `off`), that oldest,
/// `Err(floor)`: no answer about the record at `off` yet, since retention can still hold it
/// there ([`Bridge::read_at`] reads it again, told). Every other refusal, the same words under
/// another code, and an `<oldest>` at `off` stay the errors they are.
fn plain_read_at(
    page: io::Result<astream_broker::Page>,
    off: u64,
) -> io::Result<Result<Vec<BrokerRecord>, u64>> {
    match page {
        Ok((rows, _)) => Ok(Ok(rows)),
        Err(e) => match transport::retained_past(&e, off) {
            Some(floor) => Ok(Err(floor)),
            None => Err(e),
        },
    }
}

/// The told read of one offset `off` of `filter` (`Client::fetch_told`), on `reader`, a
/// connection that is not the link's, since asking for gap notices turns every fetch on it into
/// a told one: its page, of at most the one record, which below the filter's oldest starts at
/// `off` only with a record retention holds there, and is empty where retention holds none of
/// the filter from `off`; `None` where its broker does not tell fetches
/// (`Client::supports_told_fetch`, known once the connection said `Hello`).
pub(crate) fn told_read_at(
    reader: &mut transport::Conn,
    off: u64,
    filter: &str,
) -> io::Result<Option<Vec<BrokerRecord>>> {
    if !reader.supports_told_fetch() {
        return Ok(None);
    }
    match reader.fetch_told(off, filter, 1) {
        Ok(((rows, _), _)) => Ok(Some(rows)),
        Err(e) if transport::retained_past(&e, off).is_some() => Ok(Some(Vec::new())),
        Err(e) => Err(e),
    }
}

#[derive(Debug, PartialEq, Eq)]
enum Unfetched {
    Why(&'static str),
    Retained(u64),
}

impl From<&'static str> for Unfetched {
    fn from(why: &'static str) -> Self {
        Unfetched::Why(why)
    }
}

impl Unfetched {
    fn tail(&self) -> String {
        match self {
            Unfetched::Why(why) => format!("err={why}"),
            Unfetched::Retained(floor) => format!("err=retained floor={floor}"),
        }
    }
}

fn release_tokens() -> String {
    format!(
        "proto={} aterm={}",
        astream_broker::proto::PROTO_VERSION,
        crate::pct::encode(&aterm_stamp::build_id())
    )
}

const LINK_LOG_LANE: u32 = 2;

const LINK_CONTINUES_LANE: u32 = 3;

const LINK_SET_ASIDE_LANE: u32 = 4;

const LINK_QUARANTINE_LANE: u32 = aterm_types::control_verbs::LINK_QUARANTINE_LANE;

const LINK_MIRROR_WAIT_LANE: u32 = 6;

/// The lane that added `link gap from=<a> to=<b> [records=<n>]` (astream R4's addendum A9).
const LINK_GAP_LANE: u32 = aterm_types::control_verbs::LINK_GAP_LANE;

/// The lane that added `link gap … sid=<sid> [topic=<t>]` (B12): a give-up the bridge's own read
/// of one session's lane met ([`Bridge::tell_given_up`]).
const LINK_GAP_SCOPED_LANE: u32 = aterm_types::control_verbs::LINK_GAP_SCOPED_LANE;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Replaced {
    head: u64,
    was_high: u64,
    was_log: Option<astream_broker::LogIdentity>,
    log: Option<astream_broker::LogIdentity>,
    continues: Option<u64>,
    deadlines: usize,
    inflight: usize,
}

impl Replaced {
    fn why(&self) -> String {
        match (self.was_log, self.log, self.continues) {
            (Some(was), Some(now), Some(f)) if was != now => format!(
                "its broker names it {now}, the next generation of the {was} this node read, \
                 which continues it below offset {f}"
            ),
            (Some(was), Some(now), None) if was != now => {
                format!("its broker names it {now}, not the {was} this node read")
            }
            _ => format!(
                "its head is {}, below the {} this node saw there",
                self.head, self.was_high
            ),
        }
    }
}

const SEQ_BLOCK: u64 = 64;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct SeqBlock {
    inc: u64,
    next: u64,
    last: u64,
}

/// The most publishes the bridge keeps unacked on its one pipelined connection: a send
/// at the window settles acks first.
const PIPELINE_WINDOW: usize = 64;

/// How long a round parks while an ack is owed before it reads the acks
/// itself: a park that took nothing in this long settles them (blocking).
/// Also how long after a round first held the inbox commit for the loop's
/// next write (critic item B08, `Bridge::hold_inbox_commit`) the loop parks
/// before it sends that commit alone.
/// Public for the fabric bench, which counts a release later than this plus
/// a barrier after its ack as late.
pub const IDLE_ACK_GRACE: Duration = Duration::from_millis(5);

const INBOX_COMMIT_EVERY: usize = 256;

#[derive(Debug)]
enum Then {
    Ev {
        payload: String,
        sid: Option<String>,
        record: Option<u64>,
    },
    Presence {
        sid: String,
        mark: Option<Marked>,
        exited: bool,
    },
    HaltAck {
        off: u64,
    },
    Mirror {
        slot: usize,
    },
    Post(Box<SentPost>),
    Commit {
        upto: u64,
        retry: bool,
    },
    /// The broker's SETTLE of its log's device, sent once the mirror pin's barriers returned
    /// (critic item F07, [`Bridge::make_the_pin_fence`]): its answer makes the fence.
    Settle,
}

fn commit_refusal_is_an_answer(then: &Then, answer: &io::Result<(u64, bool)>) -> bool {
    matches!(then, Then::Commit { retry: false, .. })
        && answer
            .as_ref()
            .err()
            .is_some_and(|e| refusal_is_ack(e) && !refused_for_good(e))
}

impl Then {
    const fn commits_through(&self, off: u64) -> bool {
        matches!(self, Then::Commit { upto, .. } if *upto >= off)
    }

    const fn is_commit(&self) -> bool {
        matches!(self, Then::Commit { .. })
    }

    const fn is_settle(&self) -> bool {
        matches!(self, Then::Settle)
    }

    const fn waited_when_idle(&self) -> bool {
        matches!(self, Then::Post(_) | Then::Mirror { .. })
    }

    fn posted_by(&self, sid: &str) -> bool {
        self.post().is_some_and(|post| post.sid == sid)
    }

    fn post(&self) -> Option<&QueuedPost> {
        match self {
            Then::Post(sent) => Some(&sent.post),
            _ => None,
        }
    }

    /// Whether this is a post of `sid`'s in `identity`'s words (one pinned before identities,
    /// whatever its words).
    fn posted_in(&self, sid: &str, identity: &PublishIdentity) -> bool {
        matches!(self, Then::Post(sent) if sent.post.sid == sid
            && sent.pin.identity.as_ref().is_none_or(|words| words == identity))
    }

    fn own_lane_post(&self) -> bool {
        matches!(self, Then::Post(sent) if sent.journaled)
    }

    /// Whether this is a post of `sid`'s no in-flight entry stands for.
    fn unjournaled_post_of(&self, sid: &str) -> bool {
        matches!(self, Then::Post(sent) if !sent.journaled && sent.post.sid == sid)
    }

    fn waiting_post(&self) -> bool {
        matches!(self, Then::Post(sent) if WAITING_KINDS.contains(&sent.post.kind.as_str()))
    }
}

#[derive(Debug)]
struct SentPost {
    post: QueuedPost,
    subject: String,
    pin: PublishPin,
    journaled: bool,
}

/// A POST OR AN OWED RECEIPT THE BROKER LANDED at `off`, its bookkeeping owed (critic items E07
/// and F15, [`Bridge::land_group`]): its offset in the self-lane log where its record is this
/// node's own ([`Landing::remembers`]), a post's key's and ask's records, aterm's retirement of
/// it from its outbox, its reservation's end and, where `entry` names one, its in-flight
/// entry's.
#[derive(Debug)]
struct Landing {
    of: Outboxed,
    subject: String,
    pin: PublishPin,
    off: u64,
    /// The broker's dedup answered the publish: an earlier attempt landed at `off`.
    deduped: bool,
    /// The identity of the record at `off`, where it is known.
    actual: Option<PublishIdentity>,
    /// The in-flight entry at the pin's sequence this landing ends, by whether its record is
    /// this node's own ([`Bridge::settle_landed`]'s `ours`): `None` for a record with no entry
    /// (one onto another node's lane), or one whose entry is not this landing's to end.
    entry: Option<bool>,
}

/// WHAT A [`Landing`] RETIRES FROM ATERM'S OUTBOX, and how ([`Bridge::landing_line`]).
#[derive(Debug)]
enum Outboxed {
    /// A queued post, by its `outbox sent`; its reservation is the post journal's.
    Post(QueuedPost),
    /// An owed receipt, by its `deliver <sid> receipt=<rid> off=<n>` (critic item F15); its
    /// reservation is its own file ([`StateDir::receipt_pin`]).
    Receipt(OwedReceipt),
}

impl Outboxed {
    /// The session whose outbox holds it.
    fn sid(&self) -> &str {
        match self {
            Self::Post(post) => &post.sid,
            Self::Receipt(r) => &r.sid,
        }
    }
}

impl Landing {
    /// Whether its offset goes into the self-lane log ([`Bridge::land_group`]'s first step):
    /// only where the record at `off` lies on one of this node's own lanes (`on_own_lane`,
    /// [`Bridge::on_own_lane`]), a post's as an owed receipt's (critic item F17, "THE SELF-LANE
    /// LOG TAKES THIS NODE'S OWN RECORDS ONLY" at [`Bridge::land_group`]). The record's face is
    /// the identity read at `off` where one is known (a keyed post's dedup answers its key's
    /// winner, whose face can be another lane's), else the attempt's, as
    /// [`Bridge::publish_journaled_entry`] judges an entry's record its own. A post or a receipt
    /// onto another node's lane or a person's is never this node's own. One whose entry an
    /// earlier settle ended had its offset remembered then, which the window holds already.
    fn remembers(&self, on_own_lane: impl Fn(&str) -> bool) -> bool {
        let face = self
            .actual
            .as_ref()
            .or(self.pin.identity.as_ref())
            .map_or(self.subject.as_str(), |identity| identity.subject.as_str());
        on_own_lane(face)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PostNext {
    Moves,
    Holds,
    Ends,
}

const SUBJECT_BUDGET: &str = DeadReason::SubjectBudget.word();

fn past_the_budget(e: &io::Error) -> bool {
    astream_broker::broker_refusal(e).is_some_and(astream_broker::BrokerRefusal::is_subject_bound)
}

fn broker_unreachable() -> io::Error {
    io::Error::new(io::ErrorKind::NotConnected, "the broker is unreachable")
}

fn same_log(judged: Option<astream_broker::LogIdentity>, c: &Conn) -> io::Result<()> {
    match (judged, c.seen_log_identity()) {
        (Some(judged), Some(now)) if judged != now => Err(io::Error::new(
            io::ErrorKind::ConnectionReset,
            format!("the broker's log changed during the attach ({judged} -> {now})"),
        )),
        _ => Ok(()),
    }
}

const SAY_RESERVE_DIVISOR: u64 = 4;

const LANE_RESERVE_DIVISOR: u64 = 8;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Opens {
    Topic,
    Lane,
    Mirror,
}

impl Opens {
    const fn reserve(self, max: u64) -> u64 {
        match self {
            Opens::Topic | Opens::Mirror => max / SAY_RESERVE_DIVISOR,
            Opens::Lane => max / LANE_RESERVE_DIVISOR,
        }
    }

    const fn kept_for(self) -> &'static str {
        match self {
            Opens::Topic => "its sessions' faces and directed lanes",
            Opens::Lane => "its sessions' faces",
            Opens::Mirror => "its sessions' presence rows and directed lanes",
        }
    }
}

/// THE NODE'S SUBJECT BUDGET AS ONE DRAIN OR ONE MIRROR PASS READ IT (critic item G04)
/// ([`Bridge::budget_scoped`]). A scope reads the budget at most once, at the first subject it
/// judges that the bridge does not know indexed ([`Bridge::budget_allows`]), and judges each
/// later one against that reading less every subject the bridge may have opened since
/// ([`Bridge::count_opening`]). Before G04 every such subject cost a `subject_budget` round
/// trip, behind `conn_now`'s read of every owed ack, so an attach's first drain over K fresh
/// lanes made K of them.
///
/// The reading less the count never says more is left than the broker holds: the budget is this
/// producer's (`subject_budget(producer_id)`), a subject is charged to the producer whose
/// record first indexed it, and every publish and will the bridge sends is counted where it is
/// written ([`Bridge::send_pipelined`], [`Bridge::publish_at`], [`Bridge::publish_answer`],
/// [`Bridge::bring_presence_up`]'s will, [`Bridge::mirror_send`]'s bursts): one for each to a
/// subject it does not know indexed, landed or not. A retirement, or a record onto a subject
/// already indexed, leaves more than the count says. So where the count lets a subject open,
/// the per-subject read would too, and where it says the budget is short, the per-subject read
/// decides (a fresh reading, then the subject's own `last`), as it decided every subject before.
/// A reading never outlives its scope: what another connection under this producer spends (a
/// handoff's successor, `aterm link`'s own writes) and what a retirement frees are read by the
/// next scope's first read.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum BudgetScope {
    /// No drain or mirror pass is running: each judgement reads the budget.
    Out,
    /// A scope runs and has read nothing yet.
    Unread,
    /// The scope read `budget`, and the bridge has sent `opened` publishes and wills to subjects
    /// it did not know indexed since.
    Read {
        budget: astream_broker::SubjectBudget,
        opened: u64,
    },
}

#[derive(Debug)]
struct PublicationReadFailure(io::Error);

impl std::fmt::Display for PublicationReadFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "dedup verification read failed: {}", self.0)
    }
}

impl std::error::Error for PublicationReadFailure {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.0)
    }
}

fn rtt_ms_of(rtt: Duration) -> u64 {
    u64::try_from(rtt.as_micros().div_ceil(1_000)).unwrap_or(u64::MAX)
}

/// Configuration parameters passed to `aterm-link serve`.
#[derive(Clone)]
pub struct Config {
    pub fleet: String,
    /// Broker Unix domain socket path.
    pub broker: String,
    pub cap_files: Vec<String>,
    pub state_dir: String,
    /// Principals whose `task` and `control` records are delivered without demotion to `note`.
    pub accept_from: Vec<String>,
    /// Explicit endpoint socket path (`--sock <path>`) for hand-started observer mode.
    pub sock: Option<String>,
    /// Endpoint instance token for observer mode.
    pub token: Option<String>,
    /// Session presence publication detail mode (`meta` or `minimal`).
    pub presence: Mode,
    /// Whether `inbox seen` verdicts on `ask`/`task` rows publish `kind=ack` receipts.
    pub receipts: bool,
    /// Session-log bus mirroring mode (`auto`, `off`, or `turns`).
    pub log: crate::mirror::LogMode,
    /// With the mesh on (§11 C3): where this Mac's node key names the bridge's node, and where
    /// the node service hands it the members it routes to. The fleet is then
    /// [`crate::federate::FLEET`], whatever `fleet` says. `None` with the mesh off.
    pub mesh: Option<crate::federate::MeshPaths>,
}

impl std::fmt::Debug for Config {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let Config {
            fleet,
            broker,
            cap_files,
            state_dir,
            accept_from,
            sock,
            token,
            presence,
            receipts,
            log,
            mesh,
        } = self;
        f.debug_struct("Config")
            .field("fleet", fleet)
            .field("broker", broker)
            .field("cap_files", cap_files)
            .field("state_dir", state_dir)
            .field("accept_from", accept_from)
            .field("sock", sock)
            .field("token", &token.as_ref().map(|_| "<redacted>"))
            .field("presence", presence)
            .field("receipts", receipts)
            .field("log", log)
            .field("mesh", mesh)
            .finish()
    }
}

/// Owned verb and push file descriptors inherited by the bridge process.
pub struct InheritedLanes {
    /// Control verb request/response lane (fd 3).
    pub verb: std::os::fd::OwnedFd,
    /// Pushed endpoint event lane (fd 4).
    pub push: std::os::fd::OwnedFd,
}

/// Live state of an `aterm-link serve` bridge instance.
pub struct Bridge {
    cfg: Config,
    node: String,
    producer_id: u64,
    /// The node service's hand-off as last read, by the file's identity (§11 C3).
    handoff: HandoffSeen,
    /// Why the last route [`Bridge::resolve_to`] refused was refused, when the hand-off refused
    /// it ([`NoHandoff`]).
    unrouted: Option<NoHandoff>,
    /// The nodes this Mac held before a key rotation, by the hand-off this attach read (§11
    /// C3b): mail its sessions wrote under one is this Mac's own, and the node service moves the
    /// mail this log held unread for one to this node's inbox (`federate::Lane::Inbox`).
    old_nodes: BTreeSet<String>,
    /// The reads of those nodes' mail lanes the node service minted this bridge
    /// (`federate::OLD_READS_FILE`), each the read of one of them (`federate::old_read`), at most
    /// `federate::RING_OLDS_MAX`, and no other grant: where the record each moved one came from
    /// lies ([`Bridge::wire_re`]).
    old_reads: Vec<Cap>,
    /// The nodes of [`Bridge::old_nodes`] that [`Bridge::old_reads`] reads: the ones the node
    /// service chose (`federate::ringed`), whose unread mail it moves here, so this bridge follows
    /// its choice rather than making its own.
    read_olds: BTreeSet<String>,
    /// With the mesh on, the held topics this bridge last named on its node's topics face
    /// ([`Bridge::say_held_topics`]); `None` until it names them on this attach.
    topics_said: Option<BTreeSet<String>>,
    /// Whether the last try to name them was refused, so a lasting refusal is said once.
    topics_refused: bool,
    inc: u64,
    witnessed_dead: Option<(u64, String)>,
    mark: Option<String>,
    state: StateDir,
    caps: Vec<Cap>,
    ring_kept: Option<String>,
    attachment: Attachment,
    fault: Fault,
    waits: Recorder,
    taken_at: Option<Instant>,
    post_event: Option<Instant>,
    ctl: Ctl,
    push: Option<Ctl>,
    conn: Option<Link<Then>>,
    ack_batch: u32,
    in_round: bool,
    inbox_pending: Option<u64>,
    inbox_pending_records: usize,
    /// Since when a round has held the inbox commit for the loop's next write to carry
    /// ([`Bridge::hold_inbox_commit`]); `None` while none is held.
    commit_held: Option<Instant>,
    group_record: Option<u64>,
    /// Within an inbox run ([`Bridge::on_inbox_run`]): [`Bridge::deliver_record`] queues the
    /// run's `deliver` lines, which [`Bridge::send_rows`] pipelines.
    queue_rows: bool,
    queued_rows: Vec<QueuedRow>,
    /// How many lines each of [`Bridge::send_rows`]'s batches carried.
    #[cfg(test)]
    rows_sent: Vec<usize>,
    /// Within a pass that reads acks in a run ([`Bridge::grouped`]): a landed post's bookkeeping
    /// waits in [`Bridge::landings`] for the pass's group (critic item E07).
    grouping: bool,
    /// The landed posts a pass has read and not yet settled, in the order their acks were read
    /// ([`Bridge::flush_landings`]).
    landings: Vec<Landing>,
    locals: BTreeMap<u64, String>,
    per_sid: PerSession,
    roster_due: Instant,
    ghost_due: Instant,
    ghosts: BTreeMap<String, Instant>,
    exits_owed: BTreeSet<String>,
    /// The `ev` records a GAP FENCE refused (`gap-fenced=`), by session (`None`: the node's
    /// own face) and payload: refused only because a write the bridge sent before them was,
    /// they are sent again once the fence is lifted ([`Bridge::resend_fenced_evs`]), so on a
    /// broker that gap-fences an `ev` is lost only where its own batch is refused, as on one
    /// that does not. So are those the broker's RATE LIMIT refused, or the bridge held back
    /// behind it ([`crate::rate_hold`]), once the hold is over: nothing of them was appended.
    evs_fenced: Vec<(Option<String>, String)>,
    /// THE `undeliverable` NOTICES THE BROKER'S RATE LIMIT HELD BACK (critic item E06's review,
    /// [`Bridge::notify_sender_undeliverable`]), by the offset of the record each answers: its
    /// subject and body. Nothing of one was appended, so it is sent again at its own sequence
    /// (`ACK_SEQ_BASE | off`) once the hold is over ([`Bridge::resend_held_notices`]), and a
    /// resend that crossed a landing is astream's dedup's to answer. Until it lands the inbox
    /// commit stays below the oldest ([`Bridge::inbox_commit_floor`]): a bridge that goes first
    /// leaves the record to the next, which refuses it again and tells its sender then.
    notices_held: BTreeMap<u64, (String, Vec<u8>)>,
    /// The subjects a departed session held that this node may give back, each with the
    /// offset of its newest record and since when it has been one ([`Bridge::reclaim_subjects`]).
    reclaim: BTreeMap<String, (u64, Instant)>,
    /// A session left the roster, or the bridge attached, since the last sweep: the next one
    /// looks for subjects to give back.
    reclaim_owed: bool,
    /// This node holds a lane to another node's session. That session's leaving is no event
    /// here, so every sweep looks for it while one is held: set by a record onto such a lane
    /// ([`Bridge::note_remote_lane`]) and by each sweep's read of the lanes.
    reclaim_remote: bool,
    /// The log nothing is given back on: its retention cut refuses every retire (`cut: `).
    /// Said once.
    reclaim_cut: Option<astream_broker::LogIdentity>,
    /// This connection's ring does not let the node read the lanes it writes (a permanent
    /// refusal): nothing is given back until the next attach, which may bring another ring.
    /// Said once an attach.
    reclaim_unread: bool,
    say_drained_to: u64,
    say_checkpoint_tick: u64,
    say_replay: SayReplay,
    say_bus_head: u64,
    halts: BTreeMap<String, (u64, bool, String)>,
    kept_halts: BTreeMap<String, String>,
    /// With the mesh on (§11 C4): every halt this node applied, by its id, and each id lifted
    /// here ([`crate::halt::Halts`], kept in [`crate::halt::STATE_FILE`]). The old per-person
    /// maps above stay empty then: the move carries what they held in.
    mesh_halts: crate::halt::Halts,
    /// With the mesh on: the nodes the node service's hand-off last named gone, whose halts
    /// lift and whose records apply no more. Kept as last read while the hand-off is missing,
    /// so a node service restarting lifts nothing and raises nothing.
    halt_gone: BTreeSet<String>,
    /// A halt record refused, and why, said once until another reason comes.
    halt_refusal_told: Option<&'static str>,
    /// With the mesh on: when this node last said each of its halt records, by id, its own
    /// records the log holds counted at their stamps ([`crate::halt::Halts::due`]).
    halt_said_at: BTreeMap<String, u64>,
    /// The halts said again, or asked for ([`crate::halt::Word::Ask`]), after a lift for every
    /// Mac that this run answered with the halt-off, and when, each answered again at most every
    /// [`crate::halt::RESAY_MS`] ([`crate::halt::Outcome::Answer`]).
    halt_answered: BTreeMap<String, u64>,
    /// With the mesh on: each person's request, by its offset, that could not be said while the
    /// link stayed up (the node key names another node, the state dir refused a sequence), tried
    /// again on the back-off below and by every attach.
    halt_unsaid: BTreeMap<u64, (String, crate::halt::Ask)>,
    /// When the next try at what is owed (a request unsaid, a record due again) may run, on the
    /// wall clock, and the wait after a try that failed, doubling to [`HALT_RETRY_MAX_MS`].
    halt_retry: (u64, u64),
    bus_high: u64,
    bus_log: Option<astream_broker::LogIdentity>,
    indexed_subjects: BTreeSet<String>,
    /// The subject budget the running drain or mirror pass read ([`BudgetScope`], critic item G04).
    budget_scope: BudgetScope,
    #[cfg(test)]
    budget_queries: u64,
    #[cfg(test)]
    parked: Option<Duration>,
    seen_nodes: BTreeSet<String>,
    presence_view: Arc<crate::presence_view::PresenceView>,
    presence_view_retry_at: Option<Instant>,
    log_witness: LogWitness,
    log_replaced: Option<Replaced>,
    rewound_log: Option<astream_broker::LogIdentity>,
    no_yield: bool,
    refused: Option<io::Error>,
    halt_applied: bool,
    halt_unconfirmed: bool,
    halt_ack_pending: Option<u64>,
    halt_reason: String,
    /// The furthest offset this run's inbox commits have landed through on this log, or
    /// `None` before one has (and again once the log's offsets are forgotten): a fresh log's
    /// first record sits at offset 0, so a bare 0 could not tell that record committed from
    /// nothing committed ([`Bridge::committed_through`]).
    committed: Option<u64>,
    commit_refused: bool,
    /// This link's broker refused to settle the mirror pin's fence (critic item F07,
    /// [`Bridge::pin_settle_refused`]): the bridge flushes the pin itself until the next attach.
    settle_refused: bool,
    endpoint_lane: u32,
    mirror: Option<Box<mirror_run::MirrorRun>>,
    broker_retains: Option<bool>,
    endpoint_build: Option<String>,
    commit_floor: Option<u64>,
    #[cfg(test)]
    ignore_lineage: bool,
    #[cfg(test)]
    attach_past_an_earlier_boots_run: bool,
    #[cfg(test)]
    route_on_a_pin_that_reads: bool,
    #[cfg(test)]
    deliver_replay_at_once: bool,
    /// A read back takes any copy of its publication identity, claimed or not (the rule
    /// [`Bridge::claims`] keeps switched off, a test's negative control).
    #[cfg(test)]
    read_back_past_claims: bool,
    /// Records the attach's shared refill read carried and offered to no session ([`Bridge::refill_node`]):
    /// what it cost over a read lane by lane, for a test to bound.
    #[cfg(test)]
    refill_carried: u64,
    /// The attach's shared refill read never hands its sessions to their own lanes for what it
    /// carries (the rule [`Bridge::refill_node`] keeps switched off, a test's negative control).
    #[cfg(test)]
    refill_never_hands_on: bool,
    /// The broker answers each page of the attach's shared refill read with no records, ending
    /// where it began ([`Bridge::refill_node`]): a fault a test injects, as no broker of the
    /// rigs or the seeded worlds answers a page that makes no progress short of the head.
    #[cfg(test)]
    refill_page_stalls: bool,
    /// A session below the node inbox's oldest joins the attach's shared refill read there without
    /// reading its own lane below it (the rule [`Bridge::refill_together`] keeps switched off, a
    /// test's negative control).
    #[cfg(test)]
    refill_joins_below_the_oldest: bool,
    /// A move's drop of the mirror's pinned batch that the disk refused is said and the attach
    /// goes on past it (the rule `Bridge::mirror_forget_log` keeps switched off, a test's
    /// negative control).
    #[cfg(test)]
    attach_past_an_unwritten_drop: bool,
    /// A test's cut, landing between a deduped answer and its read back, as astream's writer
    /// runs retention between two requests of a connection: called with the answered offset
    /// ([`Bridge::publish_reserved`]).
    #[cfg(test)]
    cut_after_the_answer: Option<Box<dyn FnMut(u64) + Send>>,
    #[cfg(test)]
    moved_at_start: u64,
    /// The next reclaim's read of the lanes this node writes is refused with this code.
    #[cfg(test)]
    reclaim_read_refused: Option<u8>,
    /// A record written onto a subject just ahead of its retire, as a successor or a sender would.
    #[cfg(test)]
    written_before_retire: Option<(String, Vec<u8>)>,
    /// The forged-self window ([`forged_self`]): the newest [`SELF_ACK_KEEP`] offsets this node
    /// knows hold its own records on its own lanes.
    self_acked: BTreeSet<u64>,
    /// The window's floor, the lowest offset it accuses, kept apart from the offsets it holds
    /// (critic item F17's review, [`self_window`]). `None` while it waits for the log's head:
    /// the `acked` file was missing, could not be read, or held neither an offset nor a floor,
    /// so the window accuses nothing until the attach that first reads the head floors it
    /// there (critic item F20, [`Bridge::floor_at_the_head`]).
    self_floor: Option<u64>,
    acked_entries: usize,
    acked_appendable: bool,
    /// Whether the `acked` file names the window's floor (`AckedLog::floor`): until it does,
    /// the next write names it ([`Bridge::remember_self_acks`]).
    acked_floored: bool,
    unsettled: BTreeMap<u64, u64>,
    awaiting_drain: BTreeMap<u64, (u64, bool)>,
    /// OFFSETS THIS BRIDGE KNOWS HOLD ONE SEQUENCE'S RECORD, on the log it names (aterm-link B05
    /// review): each copy the attach's replay or a retry published, or read back and settled, by
    /// sequence ([`Bridge::claim`]). A read back for another sequence skips them
    /// ([`Bridge::landed_since`]): the replay of an entry the disk refused to clear publishes a
    /// sender's earlier post again, above the head of the sender's next post of the same words,
    /// and taken for that post it would settle it where it never landed. A retry takes its own
    /// sequence's claim as where it landed ([`Bridge::landed_at_its_pin`]). A first attempt is
    /// never claimed: a post's pin is made only once every earlier post of its sender in the
    /// same words was answered ([`Bridge::settle_same_words`]), so their first attempts land below
    /// its head. At most [`CLAIMS_KEEP`], the lowest offsets going first.
    claims: Option<(astream_broker::LogIdentity, BTreeMap<u64, u64>)>,
    unsent: Option<u64>,
    held_senders: BTreeSet<String>,
    /// THE BROKER'S RATE LIMIT, WAITED OUT (critic item E06, [`crate::rate_hold`]): a
    /// rate-limited refusal holds every publish of this producer for the wait it names (capped),
    /// and the refused post's sender for the whole of it. Nothing held goes on the wire; the
    /// loop parks no later than a hold's end, and the drain then sends again what it held.
    rate: crate::rate_hold::RateHold,
    /// THE LANES AN UNVERIFIABLE RESERVATION HOLDS (critic item F21, [`crate::unverified_hold`]): a
    /// post or an owed receipt whose dedup answer its read back could not vouch for holds its
    /// own sender's posts, or its receipts, on a back-off with a ceiling, and every other
    /// sender's drain goes on; the loop parks no later than a hold's end, and the drain then
    /// tries the reservation again.
    unverified: crate::unverified_hold::Unverified,
    /// The attach's replay runs ([`Bridge::settle_in_flight`]): the gap fences a refused
    /// entry set stay up until it ends ([`Bridge::lift_gap_fences`]).
    fences_held: bool,
    collided: Option<(u64, u64)>,
    seq_block: Option<SeqBlock>,
    lanes_opened: BTreeSet<String>,
    deadlines: BTreeMap<u64, Deadline>,
    /// The deadline table's changes no write of the asks journal has taken yet, by the ask's
    /// offset, with its asker: a deadline noted, or one settled, expired or dropped, that the
    /// disk does not hold as `deadlines` does (item F4). Empty once a write takes
    /// ([`Bridge::persist_asks`]): the state dir notes every change in the journal's index
    /// whether its write takes or not, and a write after a refused one rewrites the file from
    /// that index. It stays small while the disk refuses: a post held over its own unwritten
    /// deadline holds its sender's queue, and an answer withheld over its own holds the inbox,
    /// so each entry is a deadline the table held, or one more per sender.
    deadlines_unwritten: BTreeMap<u64, String>,
    asked: BTreeMap<u64, String>,
    expired: BTreeMap<u64, String>,
    /// The asks a record settled while no deadline of theirs stood, by offset, with their asker:
    /// what a held post's retry settles instead of arming
    /// ([`Bridge::settle_a_retry_answered_ahead`]).
    settled_unarmed: BTreeMap<u64, String>,
    deadline_due: Instant,
    closers: Vec<(Source, Arc<BusCloser>)>,
    link: LinkReport,
    quiet_dials: QuietDials,
    attaching: bool,
    attach_rtt: Option<Duration>,
    bye: Option<Bye>,
    /// The `roster=<seq>` of the `sessions bridge` reply `locals` and `per_sid.epochs` were last
    /// read from, while aterm sends one and no inherited bye is latched. A drain whose `outbox` reply names
    /// the same count is looking at that roster, and reads it no more ([`Bridge::drain_whole`]).
    roster_seen: Option<u64>,
    /// The drain under way has looked at the roster since its `outbox` reply, so a post it
    /// resolves to an unknown sid has nothing newer to find ([`Bridge::resolve_to`]).
    roster_looked: bool,
    withdrawn: BTreeSet<String>,
    withdrawal_told: bool,
    /// Until when a quit's withdrawal waits out the broker's rate limit, from its first row
    /// ([`crate::rate_hold::WITHDRAW_WAIT_MAX`], critic item E06's review).
    withdraw_waits_until: Option<Instant>,
    set_aside: u64,
    quarantine_said: Option<u64>,
    quarantine_unread: bool,
    pin_unreconciled: Option<String>,
    recovery_owed: bool,
    /// The last bridge of the state dir ran in an earlier boot, as judged when this bridge
    /// started ([`StateDir::last_ran_in_an_earlier_boot`]): the run record named an earlier
    /// boot, or, with no record, the incarnation lease did, and a lease that reads named an
    /// earlier boot too. A lease of this boot is an attach whose sessions may still run, and one
    /// that names no boot (an older build's off the boot volume) cannot rule such an attach out:
    /// either holds this false. The start's first roster, where it reads, sweeps that boot's
    /// sessions' files and post reservations ([`Bridge::sweep_sessions_of_an_earlier_boot`]); no
    /// later roster does.
    earlier_boot: bool,
    mailbox: Arc<Mailbox>,
    fx: Fx,
    _state_lock: BridgeLock,
}

#[derive(Default)]
struct PerSession {
    epochs: BTreeMap<String, String>,
    pending_admit: BTreeSet<String>,
    topics: BTreeMap<String, BTreeMap<String, TopicCursor>>,
    unhandled: BTreeMap<String, BTreeMap<String, VecDeque<(u64, u64)>>>,
    topic_floors_dirty: BTreeSet<String>,
    handled_to: BTreeMap<String, u64>,
    topics_sampled: BTreeSet<String>,
    watched: BTreeMap<String, u64>,
    presence: BTreeMap<String, Slot>,
    lane_owed: BTreeMap<String, Option<u64>>,
    /// The highest floor told for each (sid, scope) ([`Bridge::tell_given_up`]): the scope is
    /// the topic, or empty for the session's mail lane.
    given_up_told: BTreeMap<(String, String), u64>,
}

impl PerSession {
    fn prune_to(&mut self, live: &BTreeSet<String>) {
        let Self {
            epochs,
            pending_admit,
            topics,
            unhandled,
            topic_floors_dirty,
            handled_to,
            topics_sampled,
            watched,
            presence,
            lane_owed,
            given_up_told,
        } = self;
        given_up_told.retain(|(sid, _), _| live.contains(sid));
        epochs.retain(|sid, _| live.contains(sid));
        pending_admit.retain(|sid| live.contains(sid));
        topics.retain(|sid, _| live.contains(sid));
        unhandled.retain(|sid, _| live.contains(sid));
        topic_floors_dirty.retain(|sid| live.contains(sid));
        handled_to.retain(|sid, _| live.contains(sid));
        topics_sampled.retain(|sid| live.contains(sid));
        watched.retain(|sid, _| live.contains(sid));
        presence.retain(|sid, _| live.contains(sid));
        lane_owed.retain(|sid, _| live.contains(sid));
    }
}

fn advance_frontier_cursors(
    topics: &mut BTreeMap<String, BTreeMap<String, TopicCursor>>,
    held: &BTreeMap<String, Option<u64>>,
    frontier: u64,
    next: u64,
    checkpoint_slot: u64,
) -> Vec<String> {
    let mut checkpoint = Vec::new();
    for (sid, by_topic) in topics {
        if held.contains_key(sid) {
            continue;
        }
        let mut changed = false;
        for cursor in by_topic.values_mut() {
            if cursor.next >= frontier && cursor.next < next {
                cursor.next = next;
                changed = true;
            }
        }
        if changed && fnv1a_64(sid.as_bytes()) % SAY_CURSOR_CHECKPOINT_SHARDS == checkpoint_slot {
            checkpoint.push(sid.clone());
        }
    }
    checkpoint
}

/// `(node, sid)` a lane `writer` writes leads to: `/f/<fleet>/in/<node>/<sid>/<writer>/<kind>`.
fn lane_addr(fleet: &str, writer: &str, lane: &str) -> Option<(String, String)> {
    let segs: Vec<&str> = lane.split('/').collect();
    (segs.len() == 8
        && segs[1] == "f"
        && segs[2] == fleet
        && segs[3] == "in"
        && segs[6] == writer
        && segs[4].starts_with("n-")
        && segs[5].starts_with("s-")
        && subject::is_principal(segs[4])
        && subject::is_principal(segs[5]))
    .then(|| (segs[4].to_string(), segs[5].to_string()))
}

/// One page [`Bridge::lane_page`] read: the page, what the read found given up below where it
/// read, the told page's uncounted gap notice, apart, and each record's origin in the page's
/// order where its connection asked ([`Bridge::connect`]; empty where it did not).
type LanePage = (
    astream_broker::Page,
    Option<astream_broker::Gap>,
    Option<astream_broker::Gap>,
    Vec<crate::mailbox::Origin>,
);

/// The origins a refill page read, as its refill takes them: none under [`REFILL_UNSTAMPED`],
/// the negative control of a moved copy a refill delivers.
fn refill_origins(origins: Vec<crate::mailbox::Origin>) -> Vec<crate::mailbox::Origin> {
    #[cfg(test)]
    if REFILL_UNSTAMPED.with(std::cell::Cell::get) {
        return Vec::new();
    }
    origins
}

/// `rows`, each with its origin from `origins`, the origins its page's connection read
/// (astream's `Client::page_origins`): one per record where the connection asked, none where it
/// did not.
fn with_origins(
    rows: Vec<BrokerRecord>,
    origins: Vec<crate::mailbox::Origin>,
) -> impl Iterator<Item = (BrokerRecord, crate::mailbox::Origin)> {
    let origins = if origins.len() == rows.len() {
        origins
    } else {
        vec![None; rows.len()]
    };
    rows.into_iter().zip(origins)
}

fn say_record(fleet: &str, subject: &str, raw: &[u8]) -> Option<(subject::SayAddr, Body)> {
    let say = subject::parse_say(fleet, subject)?;
    let (body, _raw_tail) = Body::decode(raw);
    body.via
        .as_deref()
        .is_none_or(via_ok)
        .then_some((say, body))
}

impl Bridge {
    /// Constructs an observer-mode [`Bridge`] from `cfg.sock` and `cfg.token`.
    pub fn new(cfg: Config) -> io::Result<Self> {
        Self::build(cfg, None)
    }

    /// Constructs a full [`Bridge`] from `cfg` and already-admitted [`InheritedLanes`].
    pub fn with_inherited_lanes(cfg: Config, lanes: InheritedLanes) -> io::Result<Self> {
        Self::build(cfg, Some(lanes))
    }

    fn build(cfg: Config, lanes: Option<InheritedLanes>) -> io::Result<Self> {
        let observer = cfg.sock.is_some() && cfg.token.is_some();
        if observer == lanes.is_some() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "provide owned inherited lanes, or an observer socket and token",
            ));
        }
        let fx = Fx::os();
        let state = StateDir::open_on(&cfg.state_dir, fx.disk())?;
        let caps = read_ring(&cfg.cap_files)?;
        let (ctl, push, attachment) = match lanes {
            Some(lanes) => (
                Ctl::adopt(lanes.verb)?,
                Some(Ctl::adopt(lanes.push)?),
                Attachment::Inherited,
            ),
            None => (
                Ctl::connect(
                    cfg.sock.as_deref().unwrap_or_default(),
                    cfg.token.as_deref().unwrap_or_default(),
                )?,
                None,
                Attachment::Observer,
            ),
        };
        Self::assemble(cfg, fx, state, caps, ctl, push, attachment)
    }

    /// Constructs a simulated [`Bridge`] backed by `fx` and `ctl` (`crate::sim`).
    #[cfg(test)]
    pub(crate) fn simulated(cfg: Config, fx: Fx, ctl: Ctl) -> io::Result<Self> {
        let state = StateDir::open_on(&cfg.state_dir, fx.disk())?;
        state.adopt_without_the_attach(fx.mutant(crate::sim::world::Mutant::AdoptWithoutTheAttach));
        state.stop_leaves_the_heads_unflushed(
            fx.mutant(crate::sim::world::Mutant::StopLeavesTheHeadsUnflushed),
        );
        let caps = read_ring(&cfg.cap_files)?;
        Self::assemble(cfg, fx, state, caps, ctl, None, Attachment::Inherited)
    }

    fn assemble(
        mut cfg: Config,
        fx: Fx,
        state: StateDir,
        caps: Vec<Cap>,
        mut ctl: Ctl,
        push: Option<Ctl>,
        attachment: Attachment,
    ) -> io::Result<Self> {
        let probe = fx.lane(|| ctl.request("outbox"))?;
        let attachment = match (attachment, probe.ok()) {
            (Attachment::Inherited, true) => Attachment::Inherited,
            (_, _) => {
                stderr_line!(
                    "aterm-link: OBSERVER MODE: aterm refused `outbox` ({}); \
                     carries no mail, applies no fleet halt",
                    probe.header()
                );
                Attachment::Observer
            }
        };
        let state_lock = hold_state_dir(&state, &mut ctl, push.as_ref(), attachment)?;
        let node = match &cfg.mesh {
            Some(mesh) => {
                crate::federate::FLEET.clone_into(&mut cfg.fleet);
                mesh_node(&state, &caps, mesh)?
            }
            None => node_for(&state, &caps)?,
        };
        let producer_id = astream_cap::producer_id_of(&node);
        let mesh_halts = if cfg.mesh.is_some() {
            load_mesh_halts(&state)
        } else {
            crate::halt::Halts::default()
        };
        // The pid file is whole by its rename, and flushed by nothing: every reader takes it
        // as advisory (`StateDir::holder` names it in the state lock's refusal, which the lock
        // itself decides; the lab and the tests read it in a state dir made for their run, and
        // no power loss falls inside one), and no process it names outlives a power loss, so a
        // file that rolled back or lost its bytes there is no worse than a durable one naming
        // the dead bridge. A durable write's two full flushes sat on every restart's and
        // handoff's way to an attached bridge: the temporary's before the rename, inside
        // kill -> new pid, and the directory's after it, inside new pid -> node row acked.
        let _ = aterm_uds::atomic_file::write_flushed(
            &state.root().join("pid"),
            format!("{}\n", std::process::id()).as_bytes(),
            0o600,
            None,
        );
        let fault = Fault::from_env(&state);
        let waits = Recorder::from_env(state.root());
        let mailbox = Arc::new(Mailbox::default());
        mailbox.stamp(waits.on());
        state.time_ops(waits.on());
        let log_witness = state.log_witness().unwrap_or_default();
        let acked = state.self_acked_log();
        let (acked_entries, acked_appendable, acked_floored) =
            (acked.entries, acked.appendable, acked.floor.is_some());
        let (self_acked, self_floor) = self_window(acked);
        let mirror = mirror_run::MirrorRun::new(cfg.log, &state, fx.now(), || fx.alias_key());
        let (asked, deadlines) = state.asks_tables();
        let now = fx.now();
        Ok(Self {
            node,
            producer_id,
            handoff: HandoffSeen::default(),
            unrouted: None,
            old_nodes: BTreeSet::new(),
            old_reads: Vec::new(),
            read_olds: BTreeSet::new(),
            topics_said: None,
            topics_refused: false,
            inc: 0,
            witnessed_dead: None,
            mark: None,
            acked_entries,
            acked_appendable,
            acked_floored,
            self_acked,
            self_floor,
            unsettled: BTreeMap::new(),
            awaiting_drain: BTreeMap::new(),
            claims: None,
            unsent: None,
            held_senders: BTreeSet::new(),
            rate: crate::rate_hold::RateHold::default(),
            unverified: crate::unverified_hold::Unverified::default(),
            fences_held: false,
            collided: None,
            seq_block: None,
            lanes_opened: BTreeSet::new(),
            deadlines: deadlines.into_iter().map(|d| (d.off, d)).collect(),
            deadlines_unwritten: BTreeMap::new(),
            asked: asked.into_iter().map(|a| (a.off, a.to)).collect(),
            expired: BTreeMap::new(),
            settled_unarmed: BTreeMap::new(),
            deadline_due: now + roster_refresh(),
            state,
            caps,
            ring_kept: None,
            attachment,
            fault,
            waits,
            taken_at: None,
            post_event: None,
            ctl,
            push,
            conn: None,
            ack_batch: 0,
            in_round: false,
            inbox_pending: None,
            inbox_pending_records: 0,
            commit_held: None,
            group_record: None,
            queue_rows: false,
            queued_rows: Vec::new(),
            #[cfg(test)]
            rows_sent: Vec::new(),
            grouping: false,
            landings: Vec::new(),
            locals: BTreeMap::new(),
            per_sid: PerSession::default(),
            roster_due: now + roster_refresh(),
            ghost_due: now + ghost_sweep(),
            ghosts: BTreeMap::new(),
            exits_owed: BTreeSet::new(),
            evs_fenced: Vec::new(),
            notices_held: BTreeMap::new(),
            reclaim: BTreeMap::new(),
            reclaim_owed: false,
            reclaim_remote: false,
            reclaim_cut: None,
            reclaim_unread: false,
            say_drained_to: 0,
            say_checkpoint_tick: 0,
            say_replay: SayReplay::new(now),
            say_bus_head: 0,
            halts: BTreeMap::new(),
            kept_halts: BTreeMap::new(),
            mesh_halts,
            halt_gone: BTreeSet::new(),
            halt_refusal_told: None,
            halt_said_at: BTreeMap::new(),
            halt_answered: BTreeMap::new(),
            halt_unsaid: BTreeMap::new(),
            halt_retry: (0, 0),
            bus_high: log_witness.high,
            bus_log: log_witness.id,
            indexed_subjects: BTreeSet::new(),
            budget_scope: BudgetScope::Out,
            #[cfg(test)]
            budget_queries: 0,
            #[cfg(test)]
            parked: None,
            seen_nodes: BTreeSet::new(),
            presence_view: Arc::default(),
            presence_view_retry_at: None,
            log_witness,
            log_replaced: None,
            rewound_log: None,
            no_yield: false,
            refused: None,
            halt_applied: false,
            halt_unconfirmed: false,
            halt_ack_pending: None,
            halt_reason: String::new(),
            committed: None,
            commit_refused: false,
            settle_refused: false,
            endpoint_lane: 0,
            mirror,
            broker_retains: None,
            endpoint_build: None,
            commit_floor: None,
            #[cfg(test)]
            ignore_lineage: false,
            #[cfg(test)]
            attach_past_an_earlier_boots_run: false,
            #[cfg(test)]
            route_on_a_pin_that_reads: false,
            #[cfg(test)]
            deliver_replay_at_once: false,
            #[cfg(test)]
            read_back_past_claims: false,
            #[cfg(test)]
            refill_carried: 0,
            #[cfg(test)]
            refill_never_hands_on: false,
            #[cfg(test)]
            refill_page_stalls: false,
            #[cfg(test)]
            refill_joins_below_the_oldest: false,
            #[cfg(test)]
            attach_past_an_unwritten_drop: false,
            #[cfg(test)]
            cut_after_the_answer: None,
            #[cfg(test)]
            moved_at_start: 0,
            #[cfg(test)]
            reclaim_read_refused: None,
            #[cfg(test)]
            written_before_retire: None,
            closers: Vec::new(),
            link: LinkReport::new(),
            quiet_dials: QuietDials::default(),
            attaching: false,
            attach_rtt: None,
            bye: None,
            roster_seen: None,
            roster_looked: false,
            withdrawn: BTreeSet::new(),
            withdrawal_told: false,
            withdraw_waits_until: None,
            set_aside: 0,
            quarantine_said: None,
            quarantine_unread: false,
            pin_unreconciled: None,
            recovery_owed: false,
            earlier_boot: false,
            mailbox,
            cfg,
            fx,
            _state_lock: state_lock,
        })
    }

    /// One request on the verb lane, every `deliver` line an inbox run queued sent ahead of it
    /// ([`Bridge::send_rows`]), so aterm reads the lane in the order the bridge decided it.
    fn ctl_request(&mut self, line: &str) -> io::Result<Reply> {
        self.send_rows();
        let started = self.waits.starts();
        let ctl = &mut self.ctl;
        let reply = self.fx.lane(|| ctl.request(line));
        if started.is_some() {
            self.waits.note(crate::wait_tally::lane_row(line), started);
        }
        note_ctl_loss(&self.ctl, &self.mailbox, reply.as_ref().err());
        reply
    }

    fn aterm_gone(&self) -> bool {
        self.ctl.lost() || self.mailbox.aterm_closed()
    }

    /// A connection to this bus under the ring. With the mesh on it asks for each record's
    /// origin (astream's `Client::origin_stamps`), so every read on it, a tail
    /// (`Subscription::last_origin`) or a page (`Client::page_origins`), names where each record
    /// came from: a moved copy is known by the read that brings it ([`Bridge::moved_from`]),
    /// and an answer's `re=` is read in one fetch ([`Bridge::held_record`]). A broker that
    /// predates origins stamps none.
    fn connect(&self) -> io::Result<(Conn, BusCloser)> {
        let (mut c, closer) = self.fx.dial(&self.cfg.broker, ack_deadline())?;
        transport::attach_ring(&mut c, &self.caps)?;
        if self.cfg.mesh.is_some() {
            c.origin_stamps()?;
        }
        Ok((c, closer))
    }

    fn reload_caps(&mut self) {
        match ring_on_disk(&self.cfg.cap_files, &self.caps, &self.node) {
            Ok(ring) => {
                if ring != self.caps {
                    stderr_line!(
                        "aterm-link: the cap ring on disk changed ({} grant(s)); attaching with it",
                        ring.len()
                    );
                    self.caps = ring;
                }
                self.ring_kept = None;
            }
            Err(why) => {
                if self.ring_kept.as_deref() != Some(why.as_str()) {
                    stderr_line!(
                        "aterm-link: kept the cap ring this bridge holds; the one in {} is not \
                         taken: {why}",
                        self.cfg.cap_files.join(" ")
                    );
                    self.ring_kept = Some(why);
                }
            }
        }
    }

    fn connect_on_log(&self) -> io::Result<(Conn, BusCloser)> {
        let (c, closer) = self.connect()?;
        same_log(self.bus_log, &c)?;
        Ok((c, closer))
    }

    /// A connection of its own for a read TOLD WHAT RETENTION GAVE UP
    /// ([`transport::fetch_told_kept`]), on the log the link reads: asking for gap notices
    /// turns every fetch on a connection into a told one, which the link's own reads, each
    /// reading on at the floor, are not. It has said `Hello` (the ring's attach says it, and
    /// with no ring it is said here), so its log is held to the link's and its
    /// `Client::supports_told_fetch` says whether its broker tells fetches.
    fn told_reader(&self) -> io::Result<(Conn, BusCloser)> {
        let (mut c, closer) = self.connect()?;
        if self.caps.is_empty() {
            c.log_identity()?;
        }
        same_log(
            self.conn.as_ref().and_then(|conn| conn.seen_log_identity()),
            &c,
        )?;
        Ok((c, closer))
    }

    /// READ ONE OFFSET `off` OF `filter`: a `Fetch` from it of at most one record, on the
    /// link. Where astream refuses that read below the filter's oldest (`retained-from=`,
    /// [`plain_read_at`]), the same read again TOLD what retention gave up
    /// (`Client::fetch_told`, on a connection of its own: [`Bridge::told_reader`]): the
    /// oldest is where the filter is complete, and retention can still hold a record of it
    /// below there (astream's `retention::oldest`: a filter whose subjects have different
    /// floors, a class made whole from the base by a rules change, the floors folded past
    /// their bound), which the told read delivers first. So on a broker that tells fetches
    /// the refusal reads as the record found, or as [`ReadAt::Cut`] where retention holds
    /// none of the filter at `off`; never as no record there (finding F3 of the B05 landing:
    /// the dedup read back of [`Bridge::publish_reserved`] took a held record for none and
    /// sent its post again, and [`Bridge::fetch_record`] answered `retained` for a record
    /// the broker still holds). On one that does not (`Client::supports_told_fetch`, false
    /// on a broker built before told fetches, which refuses the told read as it refused the
    /// plain one) it reads as [`ReadAt::Untold`], and where the told read cannot be made, as
    /// [`ReadAt::Unasked`].
    ///
    /// THE TOLD READ IS NOT THE LINK'S: its connection's dial, its `Hello`, its log and its
    /// read are its own, and what befalls them is handed back, the link untouched (at the
    /// broker's connection cap, or out of descriptors, a second dial fails while the link
    /// stands). Each caller judges it: the dedup read back as its own failed read, the link
    /// dropped, so the post's fate stays unknown; the read-only ones as no answer yet.
    ///
    /// # Errors
    /// No link (`NotConnected`), and the plain read's other answers, judged as the link's
    /// reads are, the link dropped.
    fn read_at(&mut self, off: u64, filter: &str) -> io::Result<ReadAt> {
        let fx = self.fx.clone();
        let Some(conn) = self.conn_now() else {
            return Err(broker_unreachable());
        };
        let started = fx.now();
        let page = plain_read_at(conn.fetch(off, filter, 1), off);
        let floor = match self.observe(started, page, Some("read"))? {
            Ok(rows) => return Ok(ReadAt::Rows(rows)),
            Err(floor) => floor,
        };
        #[cfg(test)]
        if self
            .fx
            .mutant(crate::sim::world::Mutant::ReadAtWithoutTheHeld)
        {
            return Ok(ReadAt::Cut(floor));
        }
        let started = fx.now();
        let told = self
            .told_reader()
            .and_then(|(mut reader, _closer)| told_read_at(&mut reader, off, filter));
        self.waits.request(crate::wait_tally::Row::Request, started);
        Ok(match told {
            Err(e) => ReadAt::Unasked(e),
            Ok(None) => ReadAt::Untold(floor),
            // Below the oldest the told page starts at `off` only with a record retention
            // holds there: one further up says it holds none of the filter at `off`.
            Ok(Some(rows)) if rows.first().is_some_and(|(at, _, _)| *at == off) => {
                ReadAt::Rows(rows)
            }
            Ok(Some(_)) => ReadAt::Cut(floor),
        })
    }

    /// ONE PAGE OF A SESSION'S LANE `filter` FROM `cursor`, READ ON PAST WHAT RETENTION GAVE UP
    /// AND NEVER PAST WHAT IT STILL HOLDS: the link's read, which reads on at the floor
    /// (`transport::fetch_kept`); and where that read met the filter's oldest above `cursor`,
    /// the page read again from `cursor` TOLD what retention gave up (`Client::fetch_told`, on
    /// `told`, a connection of its own the first such page dials, [`Bridge::told_reader`], and
    /// the caller's later pages read on). A lane is one subject per sender and kind, each with
    /// its own floor, so its oldest is the greatest of them and retention can still hold mail
    /// of it below there (astream's `retention::oldest`), which the told page delivers first;
    /// read on at the floor, that mail is skipped and told as given up (the F3 review: a
    /// refill and a deadline sweep did so).
    ///
    /// Handed back with what the read found given up below where it read, joined: the range
    /// below a floor its told read was refused at (the lane holds none there), and the told
    /// page's gap notice where it counts records given up (`records` of the lane's in
    /// `from..to` the page did not deliver). An uncounted notice (astream's ledger counts
    /// nothing a rules change left uncounted, which is when mail is held below the oldest)
    /// says only that a record of its range the page did not deliver may be gone, beside the
    /// mail of the range the page delivers, so it is not joined to what went: aterm marks no
    /// lane range its ring holds mail of either (aterm-gui-fabric's `note_given_up`), and a
    /// page that ran out below the oldest with none leaves its next to decide. It is handed
    /// back apart, the third of the answer, for a caller with no other notice to tell: a topic
    /// replay ([`Bridge::catch_up_topic_page`], item F4), where no face tells what went.
    /// Against a broker that does not tell fetches the told read is refused as the plain one
    /// was and reads on at the floor too, the range below it told as given up, as before told
    /// fetches.
    ///
    /// # Errors
    /// No link (`NotConnected`), and the link's read's, judged as its reads are, the link
    /// dropped. The told reader's dial, `Hello`, log and read are its own connection's, not
    /// judged as the link's: each caller stops its read as at any failed one.
    fn lane_page(
        &mut self,
        told: &mut Option<(Conn, BusCloser)>,
        cursor: u64,
        filter: &str,
        max: u32,
    ) -> io::Result<LanePage> {
        if told.is_none() {
            let fx = self.fx.clone();
            let Some(conn) = self.conn_now() else {
                return Err(broker_unreachable());
            };
            let started = fx.now();
            let answer = transport::fetch_kept(conn, cursor, filter, max);
            let origins = conn.page_origins().to_vec();
            let (page, floor) = self.observe(started, answer, Some("read"))?;
            if floor.is_none() {
                return Ok((page, None, None, origins));
            }
            #[cfg(test)]
            if let Some(floor) = floor.filter(|_| {
                self.fx
                    .mutant(crate::sim::world::Mutant::LaneReadWithoutTheHeld)
            }) {
                let below = astream_broker::Gap {
                    from: cursor,
                    to: floor,
                    records: None,
                };
                return Ok((page, Some(below), None, origins));
            }
            *told = Some(self.told_reader()?);
        }
        let Some((reader, _)) = told.as_mut() else {
            // Dialled just above.
            return Err(broker_unreachable());
        };
        let started = self.fx.now();
        let (mut at, mut tries, mut gone) = (cursor, 0usize, None::<astream_broker::Gap>);
        let page = loop {
            match reader.fetch_told(at, filter, max) {
                Ok(told_page) => break Ok(told_page),
                Err(e) => match transport::retained_past(&e, at) {
                    Some(floor) if tries < transport::KEPT_TRIES => {
                        // None of the lane is held in `at..floor`: retention gave all of it up.
                        let below = astream_broker::Gap {
                            from: at,
                            to: floor,
                            records: None,
                        };
                        gone = Some(gone.map_or(below, |g| g.and(below)));
                        tries = tries.saturating_add(1);
                        at = floor;
                    }
                    _ => break Err(e),
                },
            }
        };
        self.waits.request(crate::wait_tally::Row::Request, started);
        let (page, notice) = page?;
        let origins = reader.page_origins().to_vec();
        let (notice, uncounted) = match notice {
            Some(n) if n.records.is_none() => (None, Some(n)),
            n => (n.filter(|n| n.records.is_some_and(|r| r > 0)), None),
        };
        let gone = match (gone, notice) {
            (Some(g), Some(n)) => Some(g.and(n)),
            (g, n) => g.or(n),
        };
        Ok((page, gone, uncounted, origins))
    }

    fn note_broker_retention(&mut self) {
        let fleet = self.cfg.fleet.clone();
        let node = self.node.clone();
        let rules = self.conn.as_ref().and_then(|link| link.retention_rules());
        self.broker_retains =
            Some(rules.is_some_and(|r| crate::broker_rules::retains_ev(&r, &fleet, &node)));
    }

    fn read_log_identity(&mut self) -> io::Result<Option<astream_broker::LogIdentity>> {
        let no_ring = self.caps.is_empty();
        let fx = self.fx.clone();
        let Some(conn) = self.conn_now() else {
            return Err(io::Error::new(io::ErrorKind::NotConnected, "no broker"));
        };
        if let Some(ident) = conn.seen_log_identity() {
            return Ok(Some(ident));
        }
        if !no_ring {
            return Ok(None);
        }
        let started = fx.now();
        let answer = conn.log_identity();
        self.observe(started, answer, Some("attach"))
    }

    fn tell_endpoint_log(
        &mut self,
        ident: astream_broker::LogIdentity,
        continues: Option<(astream_broker::LogIdentity, u64)>,
    ) -> bool {
        // LANE 2 (`aterm_types::control_verbs::BRIDGE_LANE`): an endpoint that
        // did not name it in `hello` is not told — it would answer `ERR usage`,
        // and it holds no `link log` to forget its offsets by anyway.
        if self.attachment != Attachment::Inherited || self.endpoint_lane < LINK_LOG_LANE {
            return true;
        }
        let line = match continues {
            Some((was, fork)) if self.endpoint_lane >= LINK_CONTINUES_LANE => {
                format!("link log {ident} continues={fork} was={was}")
            }
            _ => format!("link log {ident}"),
        };
        match self.ctl_request(&line) {
            Ok(reply) if matches!(reply.header(), "OK" | "OK forgot=1") => true,
            Ok(reply) => {
                stderr_line!("aterm-link: `{line}` not acknowledged: {}", reply.header());
                false
            }
            Err(e) => {
                stderr_line!("aterm-link: `{line}` failed: {e}");
                false
            }
        }
    }

    fn observe<T>(
        &mut self,
        started: Instant,
        r: io::Result<T>,
        other: Option<&'static str>,
    ) -> io::Result<T> {
        self.observe_as(crate::wait_tally::Row::Request, started, r, other)
    }

    /// [`Bridge::observe`], the request counted in the wait tally's `row`.
    fn observe_as<T>(
        &mut self,
        row: crate::wait_tally::Row,
        started: Instant,
        r: io::Result<T>,
        other: Option<&'static str>,
    ) -> io::Result<T> {
        self.waits.request(row, started);
        self.judge(started, r.as_ref().map(drop), other);
        self.service_urgent();
        r
    }

    fn judge(&mut self, started: Instant, r: Result<(), &io::Error>, other: Option<&'static str>) {
        let now = self.fx.now();
        self.judge_at(started, now, r, other);
    }

    /// [`Bridge::judge`] of an answer whose read began at `started` and ended at `read`.
    fn judge_at(
        &mut self,
        started: Instant,
        read: Instant,
        r: Result<(), &io::Error>,
        other: Option<&'static str>,
    ) {
        match r {
            Ok(()) => self.note_ack(read.saturating_duration_since(started)),
            Err(e) if other.is_none() && refusal_is_ack(e) => {
                self.note_ack(read.saturating_duration_since(started));
            }
            Err(e) => {
                let bug = sent_past_an_owed_ack(e);
                debug_assert!(
                    !bug,
                    "aterm-link bug: a request was sent past an owed ack, which Link::ready \
                     exists to prevent: {e}"
                );
                let reason = link_reason(e, other.unwrap_or("error"));
                self.link_down(reason);
                self.conn = None;
            }
        }
    }

    fn conn_now(&mut self) -> Option<&mut Conn> {
        self.settle_acks();
        self.lift_gap_fences();
        self.conn.as_mut().and_then(Link::ready)
    }

    /// ASK THE BROKER TO GAP-FENCE this link's writes where its `Hello` offered it
    /// ([`Link::ask_gap_fences`]), before the attach writes anything: a broker fences only a
    /// connection that asks, so only then does a refused write hold back what the bridge sent
    /// after it, and only then may a sender keep several posts in flight
    /// ([`Bridge::pipelines_posts`]). `false` once the link is lost.
    fn ask_gap_fences(&mut self) -> bool {
        #[cfg(test)]
        if self
            .fx
            .mutant(crate::sim::world::Mutant::NeverAskForTheFence)
        {
            if let Some(link) = self.conn.as_mut() {
                link.assume_asked();
            }
            return true;
        }
        let started = self.fx.now();
        let Some(link) = self.conn.as_mut() else {
            return false;
        };
        if !link.supports_gap_fences() {
            return true;
        }
        let asked = link.ask_gap_fences();
        self.observe(started, asked, Some("attach")).is_ok()
    }

    /// Whether a sender may have several posts in flight: only where the broker GAP-FENCES
    /// (its `Hello` offered it and the link asked, [`Link::gap_fences`]), so a refused post lets
    /// nothing the bridge sent after it land. Against any other broker (one opened without the
    /// option, or an older `aterm link broker` that outlived an update) a refused batch is
    /// rolled back and the next post lands ahead of its retry, and a sender keeps one post in
    /// flight ([`Bridge::settle_sender`]).
    fn pipelines_posts(&self) -> bool {
        #[cfg(test)]
        if self.fx.mutant(crate::sim::world::Mutant::PipelineSender) {
            return self.conn.is_some();
        }
        self.conn.as_ref().is_some_and(Link::gap_fences)
    }

    /// A broker's answer to one of this bridge's writes, refused: on a broker that
    /// gap-fences, its connection now holds a fence ([`Link::note_refused`]); a rate-limited
    /// one holds every publish of this producer for the wait it names
    /// ([`crate::rate_hold::RateHold::refused`]).
    fn note_refused<T>(&mut self, answer: &io::Result<T>) {
        let Some(e) = answer
            .as_ref()
            .err()
            .filter(|e| astream_broker::broker_refusal(e).is_some())
        else {
            return;
        };
        if let Some(link) = self.conn.as_mut() {
            link.note_refused();
        }
        if let Some(wait) = crate::rate_hold::wait_of(e) {
            #[cfg(test)]
            if self
                .fx
                .mutant(crate::sim::world::Mutant::RetryRateLimitedAtOnce)
            {
                return;
            }
            let now = self.fx.now();
            self.rate.refused(now, wait);
        }
    }

    /// How much longer every publish of this producer waits behind a rate-limited refusal, if
    /// it does ([`Bridge::rate`]).
    fn rate_held(&self) -> Option<Duration> {
        self.rate.producer_left(self.fx.now())
    }

    /// Every answer owed read and the fences lifted, as a write in step needs them
    /// ([`Bridge::conn_now`]), and the error of a publish held behind the broker's rate limit if
    /// one of them, or an earlier one, holds this producer ([`Bridge::rate`]).
    fn settle_unheld(&mut self) -> io::Result<()> {
        self.settle_acks();
        self.lift_gap_fences();
        self.rate_held()
            .map_or(Ok(()), |left| Err(crate::rate_hold::held(left)))
    }

    /// LIFT THE GAP FENCES a refused write set ([`Link::lift_gap_fences`]), before the bridge
    /// writes again: every owed answer is read first, so each refused post has been concluded
    /// (its sender held for the drain, its pin kept for the retry, [`Bridge::conclude_post`])
    /// and nothing the bridge wrote after a refused write is left to land behind the lift.
    /// Whatever the refused write was, the bridge goes on without a reconnect: an `ev` the fence
    /// refused is sent again ([`Bridge::resend_fenced_evs`]), and one refused for its own sake is
    /// lost, as a refused batch's `ev` records always were; presence and the mirror's
    /// slots are sent again; a post is retried at its pin by a later drain, after the posts
    /// of its sender ahead of it; and the node's will, which rides this connection, does not
    /// fire. A lift the broker refuses or cannot answer drops the connection, which lifts
    /// them too. The attach's replay holds them up ([`Bridge::settle_in_flight`]).
    fn lift_gap_fences(&mut self) {
        if self.fences_held || !self.conn.as_ref().is_some_and(Link::fenced) {
            return;
        }
        #[cfg(test)]
        if self.fx.mutant(crate::sim::world::Mutant::KeepGapFences) {
            return;
        }
        self.settle_acks();
        // A conclusion read there may have written again, and lifted them first.
        if !self.conn.as_ref().is_some_and(Link::fenced) {
            return;
        }
        let started = self.fx.now();
        let Some(lifted) = self.conn.as_mut().and_then(Link::lift_gap_fences) else {
            return;
        };
        if let Err(e) = &lifted {
            stderr_line!("aterm-link: the broker's gap fences could not be lifted: {e}");
        }
        self.judge(started, lifted.as_ref().map(drop), Some("fence"));
    }

    /// Read every owed answer to a publish sent above `seq`, before one is sent at `seq`, on
    /// a broker that gap-fences: a write at or under a fence lands and lifts it there, so a
    /// publish below a refused one still in flight would let a later write land ahead of
    /// that one's retry. Every publish but the bus mirror's slots takes a fresh sequence,
    /// above every one sent, and reads nothing here.
    fn settle_above(&mut self, seq: u64) {
        let waiting = self.fx.now();
        while self
            .conn
            .as_ref()
            .is_some_and(|link| link.gap_fences() && link.owes_above(seq))
            && self.settle_one(waiting)
        {}
    }

    fn settle_acks(&mut self) {
        let waiting = self.fx.now();
        self.grouped(|bridge| while bridge.settle_one(waiting) {});
    }

    fn settle_one(&mut self, waiting: Instant) -> bool {
        let reading = self.waits.ack_starts();
        let Some(settled) = self.conn.as_mut().and_then(Link::recv) else {
            return false;
        };
        self.settled(settled, reading, waiting);
        true
    }

    fn try_settle_acks(&mut self) {
        self.grouped(|bridge| while bridge.try_settle_one() {});
    }

    /// A PASS THAT READS ACKS IN A RUN, ITS LANDED POSTS SETTLED AS ONE GROUP (critic item E07):
    /// while `pass` reads, each landed post's bookkeeping waits ([`Bridge::landings`]), and the
    /// group is settled ([`Bridge::flush_landings`]) ahead of any other answer's conclusion
    /// ([`Bridge::settled`]) and at the pass's end, so nothing the bridge does between two reads
    /// of the pass, nor after it, finds a landed post unsettled. Only a pass whose reads go on
    /// whatever a conclusion does groups: one that reads until a conclusion changes what it
    /// looks at (a post's self-lane offset, `Bridge::is_forged_self`) concludes each at once.
    fn grouped(&mut self, pass: impl FnOnce(&mut Self)) {
        let outer = std::mem::replace(&mut self.grouping, true);
        pass(self);
        self.grouping = outer;
        self.flush_landings();
    }

    fn try_settle_one(&mut self) -> bool {
        #[cfg(test)]
        if self.fx.mutant(crate::sim::world::Mutant::WaitForEveryAck) {
            let waiting = self.fx.now();
            return self.settle_one(waiting);
        }
        let fx = self.fx.clone();
        let reading = self.waits.ack_starts();
        let Some(settled) = self
            .conn
            .as_mut()
            .and_then(|link| fx.unwaited(|| link.try_recv()))
        else {
            return false;
        };
        let now = self.fx.now();
        self.settled(settled, reading, now);
        true
    }

    /// Conclude one answer a pass read. A post the broker landed waits for its pass's group, in
    /// a pass that groups ([`Bridge::grouped`]); any other answer is concluded only once the
    /// group ahead of it is settled, and alone, so whatever its conclusion reads or sends (a
    /// refused post's later posts in flight, a notice, the drain's next look at aterm) finds
    /// every post landed ahead of it settled, and any answer read under it concluded at once.
    fn settled(
        &mut self,
        settled: crate::pipeline::Settled<Then>,
        reading: Option<Instant>,
        waiting: Instant,
    ) {
        let (owed, answer) = &settled;
        let lands = matches!(owed.tag, Then::Post(_))
            && matches!(answer, Ok((_, deduped)) if !(*deduped && owed.first));
        let (mut reading, mut read) = (reading, None);
        if !lands && !self.landings.is_empty() {
            // THE ANSWER'S READ ENDS AT ITS RECV: its ack span (and its commit's) and its round
            // trip are taken ahead of the group's settle, which the post rows count.
            self.waits.ack(reading.take(), owed.tag.is_commit());
            read = Some(self.fx.now());
            self.flush_landings();
        }
        let grouping = self.grouping;
        self.grouping = grouping && lands;
        self.settled_now(settled, reading, waiting, read);
        self.grouping = grouping;
    }

    /// Conclude one answer: `reading` is when its read began, for the ack span not yet taken,
    /// and `read` when its read ended, where a group's settle ran since ([`Bridge::settled`]).
    fn settled_now(
        &mut self,
        (owed, answer): crate::pipeline::Settled<Then>,
        reading: Option<Instant>,
        waiting: Instant,
        read: Option<Instant>,
    ) {
        let commit = owed.tag.is_commit();
        self.waits.ack(reading, commit);
        let rider = owed.rider;
        if owed.tag.is_settle() {
            // A settle's answer is a full flush's, not a round trip's: no link rtt is judged by
            // it, and a refusal (a broker that cannot settle here) leaves the link in step.
            match answer {
                Ok(_) => self.pin_settled(),
                Err(e) if refusal_is_ack(&e) => self.pin_settle_refused(&e),
                Err(e) => {
                    let read = read.unwrap_or_else(|| self.fx.now());
                    self.judge_at(owed.sent.max(waiting), read, Err(&e), None);
                }
            }
            if let Some(off) = rider.filter(|_| self.conn.is_some()) {
                self.inbox_pending = Some(self.inbox_pending.map_or(off, |have| have.max(off)));
            }
            return;
        }
        let other = (commit && !commit_refusal_is_an_answer(&owed.tag, &answer)).then_some("read");
        self.note_refused(&answer);
        let read = read.unwrap_or_else(|| self.fx.now());
        self.judge_at(
            owed.sent.max(waiting),
            read,
            answer.as_ref().map(drop),
            other,
        );
        if let Then::Mirror { slot } = owed.tag {
            self.conclude_mirror(slot, owed.seq, owed.first, answer);
            if let Some(off) = rider.filter(|_| self.conn.is_some()) {
                self.inbox_pending = Some(self.inbox_pending.map_or(off, |have| have.max(off)));
            }
            return;
        }
        let outcome = match answer {
            Ok((off, deduped)) => {
                self.bus_high = self.bus_high.max(off.saturating_add(1));
                if deduped && owed.first {
                    Err(self.collision(owed.seq, off))
                } else {
                    Ok(off)
                }
            }
            Err(e) => Err(e),
        };
        self.conclude(owed.tag, outcome);
        if let Some(off) = rider.filter(|_| self.conn.is_some()) {
            self.inbox_pending = Some(self.inbox_pending.map_or(off, |have| have.max(off)));
        }
    }

    fn conclude(&mut self, then: Then, outcome: io::Result<u64>) {
        match (then, outcome) {
            (Then::Ev { .. }, Ok(_)) => {}
            (Then::Ev { payload, sid, .. }, Err(e))
                if gap_fenced(&e) || crate::rate_hold::rate_limited(&e) =>
            {
                self.evs_fenced.push((sid, payload));
            }
            (Then::Ev { payload, .. }, Err(e)) => {
                stderr_line!("aterm-link: could not publish ev {payload:?}: {e}");
            }
            (Then::Presence { sid, mark, exited }, Ok(_)) => {
                if exited {
                    self.exits_owed.remove(&sid);
                }
                if let (Some(mark), Some(slot)) = (mark, self.per_sid.presence.get_mut(&sid)) {
                    slot.landed(mark);
                }
            }
            (Then::Presence { sid, mark, exited }, Err(e)) => {
                stderr_line!("aterm-link: could not publish presence for {sid}: {e}");
                if exited {
                    self.exits_owed.insert(sid.clone());
                }
                if let (Some(mark), Some(slot)) = (mark, self.per_sid.presence.get_mut(&sid)) {
                    slot.unmark(mark);
                }
            }
            (Then::HaltAck { off }, Ok(_)) => {
                let _ = self.state.set_halt_acked(off);
                if self.halt_ack_pending.is_some_and(|pending| pending <= off) {
                    self.halt_ack_pending = None;
                }
            }
            (Then::HaltAck { off }, Err(e)) => {
                stderr_line!("aterm-link: could not ack the halt at {off}: {e}");
            }
            (Then::Mirror { .. }, _) => {}
            (Then::Post(sent), outcome) => self.conclude_post(*sent, outcome),
            (Then::Commit { upto, .. }, Ok(_)) => {
                self.note_committed(upto);
                self.commit_refused = false;
                if self
                    .inbox_pending
                    .is_some_and(|owed| self.committed_through(owed))
                {
                    self.inbox_pending = None;
                }
            }
            (Then::Commit { upto, .. }, Err(e)) => {
                stderr_line!("aterm-link: commit through {upto} failed: {e}");
                self.commit_refused = self.conn.is_some();
            }
            // Concluded where its answer is read ([`Bridge::settled_now`]); one never sent was
            // never owed ([`Bridge::send_pin_settle`]).
            (Then::Settle, _) => {}
        }
    }

    fn publish_pipelined(&mut self, subject: &str, body: &[u8], then: Then) {
        if self.conn.is_none() {
            return self.conclude(then, Err(broker_unreachable()));
        }
        let seq = match self.next_seq() {
            Ok(seq) => seq,
            Err(e) => return self.conclude(then, Err(e)),
        };
        self.send_pipelined(seq, subject, body, then);
    }

    fn send_pipelined(&mut self, seq: u64, subject: &str, body: &[u8], then: Then) {
        // HELD BEHIND THE BROKER'S RATE LIMIT: concluded unsent, as the broker would refuse it,
        // each kind kept for its retry (a post at its pin, an `ev` sent again, a presence row
        // republished), and nothing on the wire while the bucket is empty.
        if let Some(left) = self.rate_held() {
            return self.conclude(then, Err(crate::rate_hold::held(left)));
        }
        let first = self.unsent == Some(seq);
        self.settle_above(seq);
        let waiting = self.fx.now();
        while self
            .conn
            .as_ref()
            .is_some_and(|link| link.unacked() >= PIPELINE_WINDOW)
            && self.settle_one(waiting)
        {}
        self.lift_gap_fences();
        // An answer read at the window may be the refusal for the rate that holds this one too.
        if let Some(left) = self.rate_held() {
            return self.conclude(then, Err(crate::rate_hold::held(left)));
        }
        if let Some(e) = then.post().and_then(|post| self.unsendable(&post.sid)) {
            return self.conclude(then, Err(unsent(e)));
        }
        let producer_id = self.producer_id;
        self.note_remote_lane(subject);
        self.count_opening(subject);
        let held = self
            .take_held_commit()
            .map(|upto| (upto, subject::inbox_group(&self.cfg.fleet, &self.node)));
        let retry = self.commit_refused;
        let Some(link) = self.conn.as_mut() else {
            return self.conclude(then, Err(unsent(broker_unreachable())));
        };
        let started = self.fx.now();
        let commit = held.as_ref().map(|(upto, group)| HeldCommit {
            group,
            upto: *upto,
            tag: Then::Commit { upto: *upto, retry },
        });
        let publish = Publish {
            producer_id,
            seq,
            first,
            subject,
            body,
            tag: then,
        };
        let sent = link.send_burst(commit, vec![publish], started);
        if first && self.unsent == Some(seq) {
            self.unsent = None;
        }
        if let Err(unsent) = sent {
            let e = self.burst_unsent(started, unsent.error, unsent.commit);
            if let Some(then) = unsent.tags.into_iter().next() {
                self.conclude(then, Err(e));
            }
            return;
        }
        if !self.in_round && self.ack_batch == 0 {
            self.settle_acks();
        }
    }

    /// A burst the link did not take ([`Link::send_burst`]): the failure judged once, and the
    /// commit that rode it, if one did, concluded failed with a copy of the error; the error
    /// is handed back for the burst's publishes, which nothing is owed for either.
    fn burst_unsent(&mut self, started: Instant, e: io::Error, commit: Option<Then>) -> io::Error {
        self.judge(started, Err(&e), None);
        if let Some(commit) = commit {
            self.conclude(commit, Err(io::Error::new(e.kind(), e.to_string())));
        }
        e
    }

    fn batched<R>(&mut self, step: impl FnOnce(&mut Self) -> R) -> R {
        self.ack_batch += 1;
        let r = step(self);
        self.ack_batch -= 1;
        if !self.in_round && self.ack_batch == 0 {
            self.settle_acks();
        }
        r
    }

    fn service_urgent(&mut self) {
        if self.no_yield {
            return;
        }
        let unsent = self.unsent.take();
        while let Some(item) = self.mailbox.take_urgent() {
            match item {
                Item::Fleet(r) => self.on_fleet_record(&r),
                Item::Halt(r) => self.on_halt_record(&r),
                Item::Gap(source, g) => self.on_gap(source, g),
                Item::Resume(source @ (Source::Fleet | Source::Halt), from) => {
                    self.resume_face(source, from);
                    if self.conn.is_none() {
                        break;
                    }
                }
                Item::Lost(source, end) => {
                    self.subscription_lost(source, end);
                    break;
                }
                Item::Refused(source, reason) => {
                    self.subscription_refused(source, reason);
                    break;
                }
                Item::Event(_)
                | Item::Inbox(..)
                | Item::Say(_)
                | Item::Resume(..)
                | Item::Closed(_) => {}
            }
        }
        self.unsent = unsent;
    }

    fn unyielding<R>(&mut self, step: impl FnOnce(&mut Self) -> R) -> R {
        let was = std::mem::replace(&mut self.no_yield, true);
        let r = step(self);
        self.no_yield = was;
        r
    }

    fn last_all(&mut self, filter: &str) -> io::Result<(Vec<BrokerRecord>, (u64, u64))> {
        self.last_all_judged(filter, Some("read"))
    }

    /// [`Bridge::last_all`], each answer judged as `judged` says ([`Bridge::observe`]): `None` takes a
    /// refusal for an answer, the link up.
    fn last_all_judged(
        &mut self,
        filter: &str,
        judged: Option<&'static str>,
    ) -> io::Result<(Vec<BrokerRecord>, (u64, u64))> {
        let mut cursor = String::new();
        let mut first = None;
        let mut rows = Vec::new();
        let mut remaining = u32::MAX;
        for _ in 0..astream_broker::LAST_WALK_PAGES_MAX {
            let fx = self.fx.clone();
            let conn = self.conn_now().ok_or_else(|| {
                io::Error::new(io::ErrorKind::NotConnected, "the broker is unreachable")
            })?;
            let started = fx.now();
            let answer = conn.last_page(filter, &cursor, remaining);
            let (page, page_mark, resume) = self.observe(started, answer, judged)?;
            let mark = *first.get_or_insert(page_mark);
            remaining = remaining.saturating_sub(u32::try_from(page.len()).unwrap_or(u32::MAX));
            rows.extend(page);
            if resume.is_empty() {
                return Ok((rows, mark));
            }
            if remaining == 0 {
                return self.observe(
                    self.fx.now(),
                    Err(io::Error::other(format!(
                        "the last-value face of {filter} did not end within {} rows",
                        u32::MAX
                    ))),
                    judged,
                );
            }
            cursor = resume;
        }
        self.observe(
            self.fx.now(),
            Err(io::Error::other(format!(
                "the last-value walk of {filter} did not end within {} pages",
                astream_broker::LAST_WALK_PAGES_MAX
            ))),
            judged,
        )
    }

    fn last_face(&mut self, filter: &str) -> io::Result<Vec<BrokerRecord>> {
        self.last_face_judged(filter, Some("read"))
    }

    /// [`Bridge::last_face`], each answer judged as `judged` says ([`Bridge::last_all_judged`]).
    fn last_face_judged(
        &mut self,
        filter: &str,
        judged: Option<&'static str>,
    ) -> io::Result<Vec<BrokerRecord>> {
        const STRETCH_MAX: u32 = transport::ROW_PAGE;
        let mut cut_under = false;
        let (rows, stretch) = loop {
            let (rows, (from, _)) = self.last_all_judged(filter, judged)?;
            let fx = self.fx.clone();
            let conn = self.conn_now().ok_or_else(|| {
                io::Error::new(io::ErrorKind::NotConnected, "the broker is unreachable")
            })?;
            let started = fx.now();
            let answer = transport::fetch_kept(conn, from, filter, STRETCH_MAX);
            let ((stretch, (next, head)), floor) = self.observe(started, answer, judged)?;
            if floor.is_some() && !cut_under {
                cut_under = true;
                continue;
            }
            if next < head || floor.is_some() {
                return self.observe(
                    self.fx.now(),
                    Err(io::Error::other(format!(
                        "the records on {filter} since its last-value walk did not fit one read"
                    ))),
                    judged,
                );
            }
            break (rows, stretch);
        };
        let mut face: BTreeMap<String, BrokerRecord> =
            rows.into_iter().map(|row| (row.1.clone(), row)).collect();
        for rec in stretch {
            if face.get(&rec.1).is_none_or(|have| have.0 < rec.0) {
                face.insert(rec.1.clone(), rec);
            }
        }
        Ok(face.into_values().collect())
    }

    fn note_ack(&mut self, rtt: Duration) {
        if self.attaching {
            self.attach_rtt = Some(rtt);
            return;
        }
        let rtt_ms = rtt_ms_of(rtt);
        let now = self.fx.now();
        if !self.link.ack_wants_report(rtt_ms, now) {
            return;
        }
        self.link = LinkReport {
            up: true,
            reason: "",
            rtt_ms: Some(rtt_ms),
            reported_at: Some(now),
        };
        self.report_link(&format!("link up rtt={rtt_ms}"));
    }

    fn dial_failed(&mut self, e: &io::Error) {
        let reason = link_reason(e, "attach");
        if self.link.down_wants_report(reason) {
            stderr_line!("aterm-link: the broker link is down ({reason}): {e}");
        } else {
            // A BROKER THAT STAYS DOWN FOR ONE REASON IS SAID AGAIN: said once, a
            // minute of redials that all failed alike reads as one failed redial.
            let now = self.fx.now();
            self.quiet_dials.kept += 1;
            let quiet = self
                .quiet_dials
                .said_at
                .or(self.link.reported_at)
                .map_or(Duration::ZERO, |at| now.saturating_duration_since(at));
            if quiet >= LINK_DOWN_RETELL {
                stderr_line!(
                    "aterm-link: the broker link is still down ({reason}): {} more redials \
                     failed in {}s, the last: {e}",
                    self.quiet_dials.kept,
                    quiet.as_secs()
                );
                self.quiet_dials = QuietDials {
                    said_at: Some(now),
                    kept: 0,
                };
            }
        }
        self.link_down(reason);
    }

    fn link_down(&mut self, reason: &'static str) {
        if !self.link.down_wants_report(reason) {
            return;
        }
        self.link.up = false;
        self.link.reason = reason;
        self.link.reported_at = Some(self.fx.now());
        self.quiet_dials = QuietDials::default();
        self.report_link(&format!("link down reason={reason}"));
    }

    fn report_link(&mut self, line: &str) {
        if self.attachment != Attachment::Inherited {
            return;
        }
        match self.ctl_request(line) {
            Ok(reply) if reply.ok() => {}
            Ok(reply) => stderr_line!("aterm-link: `{line}` refused: {}", reply.header()),
            Err(e) => stderr_line!("aterm-link: `{line}` failed: {e}"),
        }
    }

    fn next_seq(&mut self) -> io::Result<u64> {
        let next = match self.seq_block {
            Some(block) if block.inc == self.inc && block.next <= block.last => block,
            _ => {
                let fresh = self.state.reserve_sequence_block(self.inc, SEQ_BLOCK)?;
                SeqBlock {
                    inc: self.inc,
                    next: *fresh.start(),
                    last: *fresh.end(),
                }
            }
        };
        self.seq_block = Some(SeqBlock {
            next: next.next + 1,
            ..next
        });
        self.unsent = Some(next.next);
        Ok(next.next)
    }

    fn publish(&mut self, subject: &str, body: &[u8]) -> io::Result<u64> {
        let seq = self.next_seq()?;
        self.publish_at(seq, subject, body).map(|(off, _)| off)
    }

    /// A record onto `subject`: a lane this node writes to another node's session makes
    /// every sweep look for that session's leaving ([`Bridge::reclaim_remote`]).
    fn note_remote_lane(&mut self, subject: &str) {
        if !self.reclaim_remote
            && lane_addr(&self.cfg.fleet, &self.node, subject).is_some_and(|(n, _)| n != self.node)
        {
            self.reclaim_remote = true;
        }
    }

    fn publish_at(&mut self, seq: u64, subject: &str, body: &[u8]) -> io::Result<(u64, bool)> {
        self.settle_unheld()?;
        let producer_id = self.producer_id;
        self.note_remote_lane(subject);
        self.count_opening(subject);
        let first = self.unsent == Some(seq);
        let fx = self.fx.clone();
        let Some(conn) = self.conn_now() else {
            return Err(io::Error::new(
                io::ErrorKind::NotConnected,
                "the broker is unreachable",
            ));
        };
        let started = fx.now();
        let answer = conn.publish(producer_id, seq, subject, body);
        if first {
            self.unsent = None;
        }
        self.note_refused(&answer);
        let (off, deduped) = self.observe(started, answer, None)?;
        self.bus_high = self.bus_high.max(off.saturating_add(1));
        if deduped && first {
            return Err(self.collision(seq, off));
        }
        Ok((off, deduped))
    }

    fn publish_reserved(
        &mut self,
        pin: &PublishPin,
        subject: &str,
        body: &[u8],
    ) -> io::Result<(u64, bool, Option<PublishIdentity>)> {
        if !reconstructs(pin, body) {
            return Err(self.unreconstructed(pin));
        }
        let attempt = pin
            .identity
            .clone()
            .unwrap_or_else(|| publication_identity(subject, body));
        let (off, deduped) = self.publish_at(pin.seq, &attempt.subject, body)?;
        if !deduped {
            return Ok((off, false, Some(attempt)));
        }
        #[cfg(test)]
        if let Some(cut) = self.cut_after_the_answer.as_mut() {
            cut(off);
        }
        // A reservation made before publication identities (7a3d78d0f6; every build since pins
        // each attempt's identity, and a keyed re-post's winner's where the key pin holds one):
        // a dedup answer to it is taken only where the read back finds its words at the offset,
        // and is otherwise left queued, never taken for a collision. Nothing else vouches for
        // one; the self-lane log did until critic item F20. Left queued, it is refused as every
        // unverifiable reservation is ([`Bridge::unverified_publication`]): it holds its own
        // sender, on a back-off, until retention gives up the record the answer named, and its
        // key with it, and the retry at the pin appends anew. One a build of v0.104.0 or older
        // landed on a face its session has since left waits so for that cut, its pin naming no
        // first face to read back.
        let legacy = pin.identity.is_none() || (pin.via_key && pin.original.is_none());
        let mut candidates = vec![attempt];
        if let Some(original) = &pin.original {
            candidates.push(original.clone());
        }
        let mut faces = BTreeSet::new();
        let mut exact_record = false;
        let (mut cut, mut untold) = (None, None);
        for candidate in &candidates {
            if !faces.insert(candidate.subject.clone()) {
                continue;
            }
            // The answer named `off` at or above the base (a cut drops the keys below it), so
            // the read meets the subject's oldest only where retention cut past `off` since:
            // read again told, it finds the record if retention still holds it there.
            let rows = match self
                .read_at(off, &candidate.subject)
                .map_err(|e| io::Error::new(e.kind(), PublicationReadFailure(e)))?
            {
                ReadAt::Rows(rows) => rows,
                ReadAt::Cut(floor) => {
                    cut = Some(floor);
                    Vec::new()
                }
                ReadAt::Untold(floor) => {
                    untold = Some(floor);
                    Vec::new()
                }
                // The read back failed, on the told read's connection: judged as a failed read
                // of the link's, the link dropped, so the post's fate stays unknown
                // (`Bridge::fate_unknown`) and nothing is sent until a read decides it.
                ReadAt::Unasked(e) => {
                    self.judge(self.fx.now(), Err(&e), Some("read"));
                    return Err(io::Error::new(e.kind(), PublicationReadFailure(e)));
                }
            };
            for (at, face, bytes) in rows {
                if at != off || face != candidate.subject {
                    continue;
                }
                exact_record = true;
                let actual = publication_identity(&face, &bytes);
                if candidates.contains(&actual) {
                    #[cfg(test)]
                    self.fx.reservation_tap(crate::fx::ReservationTap::Verified);
                    return Ok((off, true, Some(actual)));
                }
            }
        }
        if exact_record && !legacy {
            #[cfg(test)]
            self.fx
                .reservation_tap(crate::fx::ReservationTap::Collision);
            return Err(self.collision(pin.seq, off));
        }
        // A CUT VERIFIES NOTHING: retention gave up the record the answer named, and its dedup
        // key with it, so the retry at the pinned sequence appends anew, as every retry past a
        // cut does where no copy can be read (`Bridge::landed_since`). On an own lane a post's
        // or an owed receipt's record is not delivered while aterm still holds it in its outbox
        // (its entry stands until aterm answered its `outbox sent` or its `deliver <sid>
        // receipt=<rid> off=<n>` saying it left, `Bridge::land_group`, critic items E07 and
        // F15), so the copy given up went unread and the one the retry appends is the one a
        // reader is handed. On another node's lane nothing held the copy back from that node's
        // reader, which may have read it before retention gave it up: there the retry is a
        // second copy that node is handed (the limit `Bridge::landed_since` states, a second
        // node on one broker being only aterm-lab's, the tests' and the bench's today). A record
        // retention still holds is read, not taken for a cut, on a broker that tells fetches
        // ([`Bridge::read_at`]), so there the retry never puts a second copy beside one a
        // reader can still be handed. On one that does not (`ReadAt::Untold`) the
        // refusal decides nothing: the retry is sent as past a cut, and where retention still
        // holds the record it puts a second copy beside it, which the reason says.
        let reason = match (legacy, untold, cut) {
            (true, _, _) => "legacy reservation has no verifiable original identity (a keyed \
                             re-post may differ)"
                .to_string(),
            (false, Some(floor), _) => format!(
                "its face reads none below @{floor}, and the broker does not tell fetches, so \
                 retention may still hold the record there; it is sent again at its pinned \
                 sequence, a second copy where it is held"
            ),
            (false, None, Some(floor)) => format!(
                "retention gave up the record since the answer (its face holds none below \
                 @{floor}), so it is sent again at its pinned sequence"
            ),
            (false, None, None) => {
                "record is not readable at its original offset and pinned face".to_string()
            }
        };
        #[cfg(test)]
        self.fx
            .reservation_tap(crate::fx::ReservationTap::Unverifiable(
                match (legacy, untold, cut) {
                    (true, _, _) => 2,
                    (false, Some(_), _) => 3,
                    (false, None, Some(_)) => 4,
                    (false, None, None) => 5,
                },
            ));
        Err(self.unverified_publication(format!(
            "deduped reservation {} at @{off}: {reason}; left queued",
            pin.seq
        )))
    }

    /// A RESERVATION'S ANSWER ITS READ BACK CANNOT VOUCH FOR ([`Bridge::publish_reserved`]), a
    /// verdict, not a transport fault (critic item F21). Its cases:
    ///
    /// 1. ITS WORDS NO LONGER MATCH ITS PIN (`cannot reconstruct its pinned attempt`): the words
    ///    aterm lists for the post now, its `t=` aside, are not those of the attempt its pin
    ///    names (a reply's wire offset a mesh route reads again, an aterm that encodes the post
    ///    otherwise since). Nothing is sent and the verdict is local: no retry and no cut changes
    ///    it while the post and its pin stand, so it holds its sender until the post leaves
    ///    aterm's outbox.
    /// 2. A LEGACY PIN: it names no identity (a build before publication identities,
    ///    7a3d78d0f6), or a keyed re-post's names no winner's (v0.104.0 or older), and the read
    ///    finds no record of its words at the answered offset: nothing tells its own first copy
    ///    from another writer's. The cut that drops the record's dedup key releases it.
    /// 3. AN UNTOLD REFUSAL: the read of the answered offset was refused below its face's oldest
    ///    on a broker that does not tell fetches, so retention may still hold the record. The
    ///    cut whose base passes the offset releases it, its retry appending anew (a second copy
    ///    where the record is still held, which its reason says).
    /// 4. A CUT BETWEEN THE ANSWER AND THE READ BACK: the told read finds that retention gave up
    ///    the record since the answer, and its key with it, so the next try appends anew.
    /// 5. AN AMBIGUOUS DEDUP: the answered offset holds no record of the pin's faces (another
    ///    writer's under this producer id on another face, or one the pin's read cannot see).
    ///    The cut that drops its key releases it.
    ///
    /// None is taken for a delivery, and the post is never retired dead while its first copy may
    /// have landed: it stays queued at its pin, its sender held ([`Bridge::hold_unverifiable`]),
    /// and every other sender's drain goes on. A READ BACK THAT FAILED is no verdict
    /// (`PublicationReadFailure`: the told read's connection refused, the link's read failed):
    /// it is judged as the link's failed read, the link dropped and the post's fate unknown.
    /// Every build before F21 dropped the link here too (`reason=read`), so every drain ended
    /// at the reservation and every sender's posts on the node waited with it, for as long as
    /// retention kept the record (`Mutant::UnverifiableDropsTheLink` is that control).
    fn unverified_publication(&mut self, reason: String) -> io::Error {
        #[cfg(test)]
        let drops = self
            .fx
            .mutant(crate::sim::world::Mutant::UnverifiableDropsTheLink);
        #[cfg(not(test))]
        let drops = false;
        if drops {
            self.link_down("read");
            self.conn = None;
        }
        io::Error::new(io::ErrorKind::InvalidData, Unverifiable(reason))
    }

    /// [`Bridge::unverified_publication`]'s case 1: the words aterm lists for `pin`'s post or
    /// receipt now are not those its attempt published ([`reconstructs`]).
    fn unreconstructed(&mut self, pin: &PublishPin) -> io::Error {
        #[cfg(test)]
        self.fx
            .reservation_tap(crate::fx::ReservationTap::Unverifiable(1));
        self.unverified_publication(format!(
            "reservation {} cannot reconstruct its pinned attempt; left queued",
            pin.seq
        ))
    }

    /// THE LANE OF `sid`'S OUTBOX THAT AN UNVERIFIABLE ANSWER AT ITS PIN `seq` HOLDS (critic item
    /// F21, [`crate::unverified_hold`]): its posts, or its owed receipts, wait on a back-off
    /// from a second doubling to a minute, and the drain then tries the reservation again,
    /// while every other sender's go on. Said at its first answer and at each power of two
    /// after, so one a cut releases only days later says a line an hour at most. `what` names
    /// the post or receipt.
    fn hold_unverifiable(
        &mut self,
        sid: &str,
        domain: crate::unverified_hold::Domain,
        seq: u64,
        what: &str,
        e: &io::Error,
    ) {
        let (tries, wait) = self.unverified.hold(sid, domain, seq, self.fx.now());
        if tries.is_power_of_two() {
            let lane = match domain {
                crate::unverified_hold::Domain::Posts => "posts",
                crate::unverified_hold::Domain::Receipts => "receipts",
            };
            stderr_line!(
                "aterm-link: {what} of {sid} cannot be verified ({e}); {sid}'s {lane} wait \
                 behind it, every other sender's go on, and it is tried again in {}s (answer \
                 {tries} at sequence {seq})",
                wait.as_secs()
            );
        }
    }

    fn collision(&mut self, seq: u64, off: u64) -> io::Error {
        stderr_line!(
            "aterm-link: sequence {seq} was already used by another writer under this node's \
             producer id (@{off}): dropping the connection to roll the incarnation"
        );
        self.collided.get_or_insert((seq, off));
        self.link_down("attach");
        self.conn = None;
        io::Error::new(
            io::ErrorKind::AlreadyExists,
            Answered(format!(
                "sequence {seq} was already used by another writer (@{off})"
            )),
        )
    }

    fn publish_ev(&mut self, payload: &str) {
        self.publish_ev_for(None, payload);
    }

    fn publish_ev_for(&mut self, sid: Option<&str>, payload: &str) {
        let (subject, body) = self.ev_record(sid, payload);
        self.publish_pipelined(
            &subject,
            body.as_bytes(),
            Then::Ev {
                payload: payload.to_string(),
                sid: sid.map(str::to_string),
                record: self.group_record,
            },
        );
    }

    fn try_publish_ev_for(&mut self, sid: Option<&str>, payload: &str) -> io::Result<u64> {
        let (subject, body) = self.ev_record(sid, payload);
        self.publish(&subject, body.as_bytes())
    }

    fn ev_record(&self, sid: Option<&str>, payload: &str) -> (String, String) {
        let body = format!(
            "v=1 t={} ev={}",
            self.fx.wall_ms(),
            crate::pct::encode(payload)
        );
        (self.ev_face(sid), body)
    }

    fn ev_face(&self, sid: Option<&str>) -> String {
        match sid {
            Some(sid) => subject::session_face(&self.cfg.fleet, &self.node, sid, "ev"),
            None => subject::node_face(&self.cfg.fleet, &self.node, "ev"),
        }
    }

    fn bring_presence_up(&mut self) -> io::Result<()> {
        self.mark = self.state.mark(&self.node);
        self.topics_said = None;
        let subject = subject::node_face(&self.cfg.fleet, &self.node, "presence");
        let (bus_inc, witnessed_dead) = {
            let fx = self.fx.clone();
            let conn = self
                .conn_now()
                .ok_or_else(|| io::Error::new(io::ErrorKind::NotConnected, "no broker"))?;
            let started = fx.now();
            let answer = conn.last(&subject, "", 8);
            let (rows, _) = self.observe(started, answer, Some("attach"))?;
            let newest = rows
                .iter()
                .filter(|(_, s, _)| *s == subject)
                .filter_map(|(_, _, b)| {
                    let (body, _) = Body::decode(b);
                    let inc = presence_inc(&body)?;
                    Some((inc, body))
                })
                .max_by_key(|(inc, _)| *inc);
            if self.attachment == Attachment::Observer {
                if let Some((inc, body)) = &newest {
                    if let Some(holder) = carried_elsewhere(body, *inc, self.inc, &hostname()) {
                        let why = format!(
                            "node {} is carried by a live bridge ({holder}): this state dir \
                             is a copy of a running node's, and an observer needs its own \
                             --state (its own node, with its own cap)",
                            self.node
                        );
                        self.refused =
                            Some(io::Error::new(io::ErrorKind::AlreadyExists, why.clone()));
                        return Err(io::Error::new(io::ErrorKind::AlreadyExists, why));
                    }
                }
            }
            match newest {
                Some((inc, body)) => {
                    let will = body
                        .unknown
                        .get("state")
                        .map(String::as_str)
                        .is_some_and(crate::glance::is_will);
                    let dead = will
                        .then(|| body.unknown.get(crate::state::PRESENCE_MARK).cloned())
                        .flatten()
                        .filter(|mark| {
                            mark.len() == 16 && mark.bytes().all(|b| b.is_ascii_hexdigit())
                        })
                        .map(|mark| (inc, mark));
                    (inc, dead)
                }
                None => (0, None),
            }
        };
        #[cfg(test)]
        let witnessed_dead = witnessed_dead.filter(|_| {
            !self
                .fx
                .mutant(crate::sim::world::Mutant::IgnoreWitnessedWill)
        });
        self.witnessed_dead = witnessed_dead;
        #[cfg(test)]
        self.state.adopt_after_attach(
            self.fx
                .mutant(crate::sim::world::Mutant::AdoptAfterTheAttach),
        );
        #[cfg(test)]
        let bus_inc = if self
            .fx
            .mutant(crate::sim::world::Mutant::IgnoreBusIncarnation)
        {
            0
        } else {
            bus_inc
        };
        self.inc = self.state.take_incarnation(bus_inc)?;
        let mut gone = format!(
            "v=1 t={} state={} inc={} fabric=disconnected {}",
            self.fx.wall_ms(),
            crate::glance::AWAY,
            self.inc,
            release_tokens()
        );
        if let Some(mark) = &self.mark {
            gone.push_str(&format!(" {}={mark}", crate::state::PRESENCE_MARK));
        }
        let will_seq = astream_broker::will_seq(self.inc).ok_or_else(|| {
            io::Error::other(format!(
                "incarnation {} has no will sequence (past {INC_MAX})",
                self.inc
            ))
        })?;
        {
            let producer_id = self.producer_id;
            let fx = self.fx.clone();
            self.count_opening(&subject);
            let conn = self
                .conn_now()
                .ok_or_else(|| io::Error::new(io::ErrorKind::NotConnected, "no broker"))?;
            let started = fx.now();
            let answer = conn.will(producer_id, will_seq, &subject, gone.as_bytes());
            self.observe(started, answer, Some("attach"))?;
        }
        let mut live = format!(
            "v=1 t={} state=live inc={} fabric=connected host={} pid={} {}",
            self.fx.wall_ms(),
            self.inc,
            crate::pct::encode(&hostname()),
            std::process::id(),
            release_tokens()
        );
        if let Some(mark) = &self.mark {
            live.push_str(&format!(" {}={mark}", crate::state::PRESENCE_MARK));
        }
        if self.attachment == Attachment::Observer {
            live.push_str(" observer=1");
        }
        self.publish(&subject, live.as_bytes())?;
        Ok(())
    }

    fn publish_session_presence(&mut self, sid: &str, state: &str) -> bool {
        // However the session left (its `session-exited` line, the roster, the ghost sweep, a
        // quit), the next sweep looks for what it held ([`Bridge::reclaim_subjects`]).
        self.reclaim_owed |= state == "exited";
        if self.bye == Some(Bye::Quit) && (state == "live" || self.withdrawn.contains(sid)) {
            return false;
        }
        // THE ROW READS NO SCREEN (critic item D03). Its `gen=` is the newest generation a status
        // read named for the session ([`Slot::row_gen`]): the round's `sessions status` batch, the
        // attach's ([`Bridge::presence_from_status_batch`]), an agent verdict's, or the
        // `@<sid> status` read here. A screen read for it (`text --json`) would wait out any open
        // DEC 2026 frame, up to the sync timeout, on the bridge's one loop, once a row. Here
        // `@<sid> status` is read for a slot those reads left short of a generation or a verdict
        // ([`Slot::knows_status`]: a batch row said `gen=-` or `agent_deferred=1`, or none
        // reached it), so a first row says the verdict, and for every row of an observer, which
        // runs no round to keep its slots current. NOR DOES IT READ `@<sid> meta` FOR A SLOT A
        // STATUS ROW SAMPLED (critic item E05): every Ready `sessions status` row carries the
        // session's `role= attention= user_title=`, so the attach's batch and each round's
        // sample them, and a watch ack carries them too, so an ack that agrees with its slot
        // reads nothing ([`watch_ack_meta`]). `meta` is read here only for a slot no row
        // sampled: a refused batch, a row that left them out (a writer held the session's meta,
        // or `Stale`), and an observer's, whose attach reads no batch.
        if state == "live" {
            let slot = self.per_sid.presence.get(sid);
            let (knows_status, sampled) =
                slot.map_or((false, false), |s| (s.knows_status(), s.sampled));
            if !knows_status || self.attachment == Attachment::Observer {
                if let Some(sample) = self.status_sample(sid) {
                    self.status_into_presence(sid, &sample);
                }
            }
            if !sampled {
                self.sample_meta(sid);
            }
        }
        #[cfg(test)]
        if self
            .fx
            .mutant(crate::sim::world::Mutant::ReadTheScreenEveryRow)
        {
            let _ = self.ctl_request(&format!("@{sid} text --json"));
        }
        let subject = subject::session_face(&self.cfg.fleet, &self.node, sid, "presence");
        let epoch = self
            .per_sid
            .epochs
            .get(sid)
            .cloned()
            .unwrap_or_else(|| "-".into());
        let hold = u8::from(self.halt_applied);
        let (gen, fields) = self.per_sid.presence.get(sid).map_or_else(
            || {
                (
                    crate::presence::ABSENT.to_string(),
                    Fields::default().tokens(self.cfg.presence),
                )
            },
            |slot| (slot.row_gen(), slot.fields.tokens(self.cfg.presence)),
        );
        let mut body = format!(
            "v=1 t={} inc={} epoch={epoch} gen={gen} state={state} hold={hold}{fields}",
            self.fx.wall_ms(),
            self.inc
        );
        if let Some(mark) = &self.mark {
            body.push_str(&format!(" {}={mark}", crate::state::PRESENCE_MARK));
        }
        if self.attachment == Attachment::Observer {
            body.push_str(" observer=1");
        }
        let mode = self.cfg.presence;
        if state == "exited" && self.bye == Some(Bye::Quit) {
            return match self.publish_withdrawal(&subject, body.as_bytes()) {
                Ok(_) => {
                    if let Some(slot) = self.per_sid.presence.get_mut(sid) {
                        let mark = slot.mark_published(mode, self.fx.now());
                        slot.landed(mark);
                    }
                    true
                }
                Err(e) => {
                    stderr_line!("aterm-link: could not publish presence for {sid}: {e}");
                    false
                }
            };
        }
        let mark = self
            .per_sid
            .presence
            .get_mut(sid)
            .map(|slot| slot.mark_published(mode, self.fx.now()));
        self.publish_pipelined(
            &subject,
            body.as_bytes(),
            Then::Presence {
                sid: sid.to_string(),
                mark,
                exited: state == "exited",
            },
        );
        true
    }

    fn refresh_sessions(&mut self) -> io::Result<Vec<String>> {
        self.unyielding(Self::refresh_sessions_now)
    }

    fn refresh_sessions_now(&mut self) -> io::Result<Vec<String>> {
        let reply = self.ctl_request("sessions bridge")?;
        if !reply.ok() {
            let header = reply.header();
            let kind = if header.starts_with("ERR usage") || header.starts_with("ERR unknown verb")
            {
                io::ErrorKind::Unsupported
            } else {
                io::ErrorKind::Other
            };
            return Err(io::Error::new(
                kind,
                format!("aterm refused the roster: {header}"),
            ));
        }
        let Reply::Lines { rows, .. } = &reply else {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "bridge roster reply was not Lines-framed",
            ));
        };
        let (locals, epochs) = parse_bridge_roster(rows.iter().map(String::as_str))?;
        self.locals = locals;
        self.roster_seen = roster_token(reply.header()).filter(|_| self.bye.is_none());
        let mut fresh = Vec::new();
        for (sid, nonce) in epochs {
            if self.per_sid.epochs.insert(sid.clone(), nonce).is_none() {
                fresh.push(sid.clone());
                self.per_sid.pending_admit.insert(sid);
            }
        }
        let live: BTreeSet<String> = self.locals.values().cloned().collect();
        let departed: Vec<String> = self
            .per_sid
            .epochs
            .keys()
            .filter(|sid| !live.contains(*sid))
            .cloned()
            .collect();
        self.batched(|bridge| {
            for sid in departed {
                bridge.publish_session_presence(&sid, "exited");
            }
        });
        for sid in self.per_sid.topics.keys() {
            if !live.contains(sid) {
                self.state.forget_topics(sid);
            }
        }
        self.per_sid.prune_to(&live);
        let orphaned: Vec<u64> = self
            .deadlines
            .iter()
            .filter(|(_, d)| !live.contains(&d.sid))
            .map(|(off, _)| *off)
            .collect();
        for off in &orphaned {
            self.drop_deadline(*off);
        }
        if !orphaned.is_empty() {
            self.persist_asks();
        }
        Ok(fresh)
    }

    fn admit_fresh_sessions(&mut self) {
        if self.conn.is_none() {
            return;
        }
        if !self.refill_pending_admissions(true) {
            self.conn = None;
        }
    }

    fn refill_pending_admissions(&mut self, publish_presence: bool) -> bool {
        let pending: Vec<String> = self.per_sid.pending_admit.iter().cloned().collect();
        let live: Vec<String> = pending
            .iter()
            .filter(|sid| self.per_sid.epochs.contains_key(*sid))
            .cloned()
            .collect();
        if live.len() > 1 && self.attachment != Attachment::Observer {
            for sid in pending.iter().filter(|sid| !live.contains(sid)) {
                self.per_sid.pending_admit.remove(sid);
            }
            return self.refill_together(&live, publish_presence);
        }
        for sid in pending {
            if !self.per_sid.epochs.contains_key(&sid) {
                self.per_sid.pending_admit.remove(&sid);
                continue;
            }
            if publish_presence {
                self.publish_session_presence(&sid, "live");
            }
            if !self.refill(&sid) {
                return false;
            }
            self.per_sid.pending_admit.remove(&sid);
        }
        true
    }

    fn refill(&mut self, sid: &str) -> bool {
        if self.attachment == Attachment::Observer {
            return true;
        }
        // FROM `seen` A FLOOR IS A GIVE-UP THE SESSION IS TOLD OF: the lane's records past the
        // last one it handled went before this refill offered them. A session with no `seen`
        // (it never ran `inbox seen`) starts at the floor span below, and a floor met there is
        // said on stderr alone. That is a choice, not a property: with no watermark the bridge
        // cannot tell the mail the session handled from the mail it did not, and the span's
        // start is arbitrary, so a report could name mail it acted on long ago. The cost is a
        // real loss left silent: a session delivered mail it never acked, cut by retention
        // before this refill, is told nothing of it.
        let watermark = self.state.seen_off(sid);
        #[cfg(test)]
        self.fx.refill_tap(crate::fx::RefillTap::Began {
            sid,
            lane: &self.lane_filter(sid),
            watermark,
        });
        let from = match watermark {
            Some(off) => off + 1,
            None => {
                let filter = self.lane_filter(sid);
                let fx = self.fx.clone();
                let Some(conn) = self.conn_now() else {
                    return false;
                };
                let started = fx.now();
                let answer = conn.fetch(0, &filter, 0);
                let Ok((_, (_, head))) = self.observe(started, answer, Some("read")) else {
                    return false;
                };
                head.saturating_sub(REFILL_FLOOR_SPAN)
            }
        };
        self.refill_lane(sid, from, watermark.is_some(), None) != Lane::Failed
    }

    /// The `Fetch` filter of one session's inbox lane, `/f/<F>/in/<node>/<sid>/>`.
    fn lane_filter(&self, sid: &str) -> String {
        format!("/f/{}/in/{}/{sid}/>", self.cfg.fleet, self.node)
    }

    /// THE ATTACH'S REFILL OF SEVERAL SESSIONS IN ONE READ (B09). Each session is offered what
    /// its own refill ([`Self::refill`]) offers, its lane's records from its watermark to the
    /// head, but the lanes are read together where that costs less: one paged `Fetch` of the
    /// node's whole inbox, each record routed by its lane to its session and offered only at or
    /// past that session's start. astream scans every record from a `Fetch`'s `from` to its
    /// page's end under its log lock, matched or not, so lane by lane an attach scanned the log
    /// past every watermark once per session: N sessions trailing the head by R records scanned
    /// N x R (`docs/measured/fabric` B09: 32 sessions 500,000 records behind, +137-171 ms
    /// before the first live mail). Together it scans R once, but it carries every lane's
    /// records, those it offers to no session (a lane's below its session's start, a lane it
    /// reads for no session) skipped, where lane by lane each read carried its own lane's
    /// only. [`Self::refill_node`] weighs the two as it reads, and hands each session to its own
    /// lane once what it carried costs more than the scans it saved.
    ///
    /// THE SAME RECORDS, FLOORS INCLUDED. The shared read starts at or above the node inbox's
    /// oldest (where the filter is complete, astream's `retention::oldest`: the greatest floor
    /// of what it matches, so at or above every lane's), where no record of any lane is
    /// missing. A start below it is one its own lane's floor or some other lane's newest
    /// given-up record lies above: once retention gives up read mail, each lane's floor sits
    /// past its own newest given-up record, so a quiet session's start lies below a busier
    /// lane's floor. That session reads its own lane up to the node's oldest, as its own read
    /// would ([`Self::refill_lane`]: below the lane's oldest told what retention gave up, the
    /// mail it still holds there offered, what it gave up said and, from a watermark, told),
    /// and joins the shared read there: a page of its lane asking for one record, which the
    /// broker answers scanning at most a page's `FETCH_SCAN_MAX` records. A floor that rises under
    /// the shared read (a retention tick between two pages) ends it where it got to, and each
    /// session not yet done reads on alone from there, meeting its own lane's floor. A session
    /// whose delivery is owed stops there, as its own refill stops, whatever the others do.
    /// One session left to read, or a broker whose `Mark` names no oldest: lane by lane.
    fn refill_together(&mut self, sids: &[String], publish_presence: bool) -> bool {
        if publish_presence {
            for sid in sids {
                self.publish_session_presence(sid, "live");
            }
        }
        let node_filter = subject::inbox_filter(&self.cfg.fleet, &self.node);
        let (oldest, head) = {
            let fx = self.fx.clone();
            let Some(conn) = self.conn_now() else {
                return false;
            };
            let started = fx.now();
            let answer = conn.oldest(&node_filter);
            let Ok(read) = self.observe(started, answer, Some("read")) else {
                return false;
            };
            read
        };
        #[cfg(test)]
        let oldest = oldest.filter(|_| {
            !self
                .fx
                .mutant(crate::sim::world::Mutant::RefillWithoutTheFloor)
        });
        let mut together: BTreeMap<String, Refilling> = BTreeMap::new();
        let mut alone: Vec<(String, u64, bool)> = Vec::new();
        for sid in sids {
            let watermark = self.state.seen_off(sid);
            #[cfg(test)]
            self.fx.refill_tap(crate::fx::RefillTap::Began {
                sid,
                lane: &self.lane_filter(sid),
                watermark,
            });
            let after_seen = watermark.is_some();
            let from = watermark.map_or_else(|| head.saturating_sub(REFILL_FLOOR_SPAN), |w| w + 1);
            let Some(oldest) = oldest else {
                alone.push((sid.clone(), from, after_seen));
                continue;
            };
            let mut from = from;
            if from < oldest {
                if !self.per_sid.epochs.contains_key(sid) {
                    #[cfg(test)]
                    self.fx.refill_tap(crate::fx::RefillTap::Dropped { sid });
                    self.per_sid.pending_admit.remove(sid);
                    continue;
                }
                #[cfg(test)]
                self.fx
                    .refill_tap(crate::fx::RefillTap::BelowTheOldest { sid, oldest });
                #[cfg(test)]
                let joins_at_once = self.refill_joins_below_the_oldest
                    || self
                        .fx
                        .mutant(crate::sim::world::Mutant::RefillJoinBelowTheOldest);
                #[cfg(not(test))]
                let joins_at_once = false;
                let read = if joins_at_once {
                    Lane::To(oldest)
                } else {
                    self.refill_lane(sid, from, after_seen, Some(oldest))
                };
                match read {
                    Lane::Failed => return false,
                    Lane::Done => {
                        self.per_sid.pending_admit.remove(sid);
                        continue;
                    }
                    Lane::To(at) => from = at,
                }
            }
            together.insert(
                sid.clone(),
                Refilling {
                    from,
                    after_seen,
                    stopped: false,
                },
            );
        }
        if let (Some(oldest), true) = (oldest, together.len() > 1) {
            let lowest = together.values().map(|s| s.from).min().unwrap_or(head);
            #[cfg(test)]
            let lowest = if self
                .fx
                .mutant(crate::sim::world::Mutant::RefillTogetherFromTheHighest)
            {
                together.values().map(|s| s.from).max().unwrap_or(lowest)
            } else {
                lowest
            };
            match self.refill_node(&node_filter, oldest, lowest, &mut together) {
                Together::Read => {}
                Together::Failed => return false,
                Together::HandedOn(at) => {
                    for (sid, s) in std::mem::take(&mut together) {
                        if s.stopped {
                            self.per_sid.pending_admit.remove(&sid);
                        } else {
                            alone.push((sid, s.from.max(at), s.after_seen));
                        }
                    }
                }
            }
            for sid in together.keys() {
                self.per_sid.pending_admit.remove(sid);
            }
        } else {
            alone.extend(
                together
                    .into_iter()
                    .map(|(sid, s)| (sid, s.from, s.after_seen)),
            );
        }
        for (sid, from, after_seen) in alone {
            if !self.per_sid.epochs.contains_key(&sid) {
                #[cfg(test)]
                self.fx
                    .refill_tap(crate::fx::RefillTap::Dropped { sid: &sid });
                self.per_sid.pending_admit.remove(&sid);
                continue;
            }
            if self.refill_lane(&sid, from, after_seen, None) == Lane::Failed {
                return false;
            }
            self.per_sid.pending_admit.remove(&sid);
        }
        true
    }

    /// The shared read of [`Self::refill_together`]: pages of `filter`, the node's inbox, from
    /// `from` (at or above the inbox's `oldest`), each record offered to its lane's session in
    /// `sessions` at or past that session's start, until the head.
    ///
    /// WEIGHED AS IT READS, by astream's `SharedRefillMeter` ([`weigh_shared_page`]). A page
    /// costs the broker its scan once, where a read of each lane would have scanned it once for
    /// each session it reads for; and it carries the records it offers to no session (a lane's
    /// below its session's start, a lane it reads for no session, a session stopped or gone),
    /// which lane by lane no read carried, each costing about `REFILL_ROW_COST` records
    /// scanned. Once what it carried passes what it saved by the meter's slack (one page's
    /// scan), it ends where it got to and each session reads on alone (`HandedOn`), checked
    /// after each page: so the shared read never costs more than lane by lane did by more than
    /// the slack and one page of records carried, and over a log of `ev` records past the
    /// watermarks it reads once for every session.
    fn refill_node(
        &mut self,
        filter: &str,
        oldest: u64,
        from: u64,
        sessions: &mut BTreeMap<String, Refilling>,
    ) -> Together {
        let lanes = format!("/f/{}/in/{}/", self.cfg.fleet, self.node);
        #[cfg(test)]
        self.fx.refill_tap(crate::fx::RefillTap::Together {
            sessions: sessions.len(),
        });
        let mut meter =
            SharedRefillMeter::new(oldest, from, astream_broker::broker::FETCH_SCAN_MAX as u64);
        loop {
            let cursor = meter.cursor();
            let page = {
                let fx = self.fx.clone();
                let Some(conn) = self.conn_now() else {
                    return Together::Failed;
                };
                let started = fx.now();
                let answer = transport::fetch_kept(conn, cursor, filter, transport::POST_PAGE);
                let origins = conn.page_origins().to_vec();
                match self
                    .observe(started, answer, Some("read"))
                    .map(|p| (p, origins))
                {
                    Ok(((_, Some(floor)), _)) => {
                        stderr_line!(
                            "aterm-link: the attach's refill of {} sessions together met the \
                             broker's retention at @{floor} (from @{cursor}); each reads on alone \
                             from there",
                            sessions.values().filter(|s| !s.stopped).count()
                        );
                        return Together::HandedOn(cursor);
                    }
                    Ok(((p, None), origins)) => (p, origins),
                    Err(e) => {
                        stderr_line!("aterm-link: the attach's refill stopped: {e}");
                        return Together::Failed;
                    }
                }
            };
            let ((rows, (next, head)), origins) = page;
            #[cfg(test)]
            let (rows, next) = if self.refill_page_stalls {
                (Vec::new(), cursor)
            } else {
                (rows, next)
            };
            let mut carried: u64 = 0;
            for ((off, subj, body), origin) in with_origins(rows, refill_origins(origins)) {
                let owed_to = subj
                    .strip_prefix(lanes.as_str())
                    .and_then(|rest| rest.split_once('/'))
                    .map(|(sid, _)| sid)
                    .filter(|sid| {
                        sessions
                            .get(*sid)
                            .is_some_and(|s| !s.stopped && off >= s.from)
                            && self.per_sid.epochs.contains_key(*sid)
                    })
                    .map(str::to_string);
                let Some(sid) = owed_to else {
                    carried += 1;
                    continue;
                };
                let delivery = self.deliver_record(off, &subj, &body, origin);
                #[cfg(test)]
                self.fx.refill_tap(crate::fx::RefillTap::Offered {
                    sid: &sid,
                    off,
                    owed: delivery == Delivery::Owed,
                });
                match delivery {
                    Delivery::Accounted | Delivery::Queued => {}
                    Delivery::Unaccounted => return Together::Failed,
                    Delivery::Owed => {
                        if let Some(s) = sessions.get_mut(&sid) {
                            s.stopped = true;
                        }
                        #[cfg(test)]
                        self.fx
                            .refill_tap(crate::fx::RefillTap::Ended { sid: &sid, head });
                    }
                }
                if self.conn.is_none() {
                    return Together::Failed;
                }
                self.service_urgent();
            }
            #[cfg(test)]
            {
                self.refill_carried += carried;
            }
            let reading = sessions
                .iter()
                .filter(|(sid, s)| {
                    !s.stopped && s.from < next && self.per_sid.epochs.contains_key(*sid)
                })
                .count();
            let Some(step) = weigh_shared_page(&mut meter, next, head, carried, reading) else {
                stderr_line!("aterm-link: the attach's refill made no progress at {cursor}");
                return Together::Failed;
            };
            match step {
                RefillStep::Complete { .. } => {
                    #[cfg(test)]
                    for (sid, s) in sessions.iter() {
                        if s.stopped {
                            continue;
                        }
                        if self.per_sid.epochs.contains_key(sid) {
                            self.fx
                                .refill_tap(crate::fx::RefillTap::Ended { sid, head });
                        } else {
                            self.fx.refill_tap(crate::fx::RefillTap::Dropped { sid });
                        }
                    }
                    return Together::Read;
                }
                RefillStep::Continue { .. } => {}
                RefillStep::HandOn { handoff_offset } => {
                    #[cfg(test)]
                    if self.refill_never_hands_on {
                        continue;
                    }
                    stderr_line!(
                        "aterm-link: the attach's refill of {} sessions together carried more of \
                         the node's inbox than reading once saved (to @{handoff_offset}); each \
                         reads on alone from there",
                        sessions.values().filter(|s| !s.stopped).count()
                    );
                    return Together::HandedOn(handoff_offset);
                }
            }
        }
    }

    /// One session's refill on its own lane from `from`: pages of its lane, each record
    /// offered, read below its oldest told what retention gave up ([`Bridge::lane_page`]), so
    /// the mail retention still holds there is offered, never skipped and told as given up, and
    /// (`after_seen`) what it gave up told; to the head. `until` (the node inbox's oldest,
    /// [`Self::refill_together`]) ends it there instead: the records at or past it are left for
    /// the shared read, which the session joins at `until` (`Lane::To`). Below `until` the
    /// first page asks for one record, so a lane that holds none there carries one record past
    /// it, not a page.
    fn refill_lane(&mut self, sid: &str, from: u64, after_seen: bool, until: Option<u64>) -> Lane {
        let filter = self.lane_filter(sid);
        let mut cursor = from;
        let mut max = if until.is_some() {
            1
        } else {
            transport::POST_PAGE
        };
        let mut told = None;
        loop {
            let page = {
                #[cfg(test)]
                let answer = if self
                    .fx
                    .mutant(crate::sim::world::Mutant::RefillWithoutTheFloor)
                {
                    let fx = self.fx.clone();
                    let Some(conn) = self.conn_now() else {
                        return Lane::Failed;
                    };
                    let started = fx.now();
                    let answer = conn
                        .fetch(cursor, &filter, max)
                        .map(|page| (page, None, None, Vec::new()));
                    self.observe(started, answer, Some("read"))
                } else {
                    self.lane_page(&mut told, cursor, &filter, max)
                };
                #[cfg(not(test))]
                let answer = self.lane_page(&mut told, cursor, &filter, max);
                match answer {
                    Ok((p, gone, _, origins)) => {
                        if let Some(gone) = gone {
                            stderr_line!(
                                "aterm-link: refill of {sid}: the broker's retention gave up {} \
                                 of this lane; reading on past them",
                                transport::gap_lost(&gone)
                            );
                            if after_seen {
                                self.tell_given_up(sid, None, gone);
                            }
                            #[cfg(test)]
                            self.fx.refill_tap(crate::fx::RefillTap::Floor {
                                sid,
                                floor: gone.to,
                            });
                        }
                        (p, origins)
                    }
                    Err(e) => {
                        stderr_line!("aterm-link: refill of {sid} stopped: {e}");
                        return Lane::Failed;
                    }
                }
            };
            let ((rows, (next, head)), origins) = page;
            for ((off, subj, body), origin) in with_origins(rows, refill_origins(origins)) {
                if let Some(until) = until.filter(|until| off >= *until) {
                    return Lane::To(until);
                }
                let delivery = self.deliver_record(off, &subj, &body, origin);
                #[cfg(test)]
                self.fx.refill_tap(crate::fx::RefillTap::Offered {
                    sid,
                    off,
                    owed: delivery == Delivery::Owed,
                });
                match delivery {
                    Delivery::Accounted | Delivery::Queued => {}
                    Delivery::Unaccounted => return Lane::Failed,
                    Delivery::Owed => {
                        #[cfg(test)]
                        self.fx
                            .refill_tap(crate::fx::RefillTap::Ended { sid, head });
                        return Lane::Done;
                    }
                }
                if self.conn.is_none() {
                    return Lane::Failed;
                }
                self.service_urgent();
            }
            if let Some(until) = until.filter(|until| next >= *until) {
                return Lane::To(until);
            }
            if next >= head {
                #[cfg(test)]
                self.fx
                    .refill_tap(crate::fx::RefillTap::Ended { sid, head });
                return Lane::Done;
            }
            if next <= cursor {
                stderr_line!("aterm-link: refill of {sid} made no progress at {cursor}");
                return Lane::Failed;
            }
            cursor = next;
            max = transport::POST_PAGE;
        }
    }

    /// One inbox record, read with the `origin` its read stamped on it (`None` where it did not
    /// ask): delivered, refused or held, and accounted against the inbox's commit.
    fn on_inbox_record(&mut self, rec: &BrokerRecord, origin: crate::mailbox::Origin) {
        let (off, subject, body) = rec;
        if self.conn.is_none() {
            return;
        }
        if let Ok(addr) = subject::parse_in(&self.cfg.fleet, &self.node, subject) {
            if !self.per_sid.epochs.contains_key(&addr.sid) && self.refresh_sessions().is_err() {
                self.conn = None;
                return;
            }
        }
        self.admit_fresh_sessions();
        if self.conn.is_none() {
            return;
        }
        self.group_record = Some(*off);
        #[cfg(test)]
        if self
            .fx
            .mutant(crate::sim::world::Mutant::CommitBeforeDelivery)
        {
            self.commit_upto(*off);
        }
        let delivery = self.deliver_record(*off, subject, body, origin);
        self.group_record = None;
        if delivery == Delivery::Queued {
            return;
        }
        self.send_rows();
        self.account_inbox(*off, subject, delivery);
    }

    /// Record `off`'s delivery against the inbox's commit, every record ahead of it accounted
    /// already.
    fn account_inbox(&mut self, off: u64, subject: &str, delivery: Delivery) {
        match delivery {
            Delivery::Accounted => {
                self.inbox_pending_records += 1;
                self.owe_inbox_commit(off);
            }
            Delivery::Unaccounted => {
                stderr_line!(
                    "aterm-link: record {off} left for the next bridge: the connection to aterm \
                     is lost"
                );
                self.commit_floor = Some(self.commit_floor.map_or(off, |have| have.min(off)));
            }
            Delivery::Owed => {
                let floor = match subject::parse_in(&self.cfg.fleet, &self.node, subject) {
                    Ok(addr) => self.per_sid.lane_owed.entry(addr.sid).or_default(),
                    Err(_) => &mut self.commit_floor,
                };
                *floor = Some(floor.map_or(off, |have| have.min(off)));
            }
            Delivery::Queued => return,
        }
        if !self.in_round {
            self.land_inbox_commit();
        } else if self.inbox_pending_records >= INBOX_COMMIT_EVERY {
            self.flush_inbox_commit();
        }
    }

    /// A GAP NOTICE on a face (astream R4's addendum A9, session log Phase 6 B22): the broker's
    /// retention gave up records of the face's filter at `g.from..g.to` before this bridge
    /// read them. On the inbox it is this node's unread mail, whose sessions the bridge cannot
    /// name (the subjects went with the records): aterm is told once for the node (`link
    /// gap`), and tells every session it serves (each inbox's `given_up=`, each log's
    /// `mail-gap` mark). The group's cursor is not moved for it: a record delivered after it
    /// commits past it, and a notice told again before one is aterm's to recognise.
    fn on_gap(&mut self, source: Source, g: astream_broker::Gap) {
        let lost = transport::gap_lost(&g);
        #[cfg(test)]
        let runs_past = self.fx.mutant(crate::sim::world::Mutant::SayFaceGapUntold);
        #[cfg(not(test))]
        let runs_past = false;
        if source == Source::Say && !runs_past {
            // NO FOLLOWER IS RUN PAST WHAT RETENTION GAVE UP (item F4). The face's tail was
            // told `g` went before it read it, which the broadcast filter's notice counts over
            // every topic, so the face delivers nothing below `g.to`: its drain position moves
            // there, and every cursor below it is caught up from where it stands, one topic at
            // a time ([`Bridge::catch_up_topic_page`]), whose told read hands the follower
            // what retention still holds of its topic and tells it what went (`link gap …
            // sid= topic=`). Read on at the face, the cursors at its frontier went past the
            // range with its next record, and nobody was told.
            stderr_line!(
                "aterm-link: the broker's retention gave up {lost} below the say subscription; \
                 each topic it ran past is caught up on its own"
            );
            self.say_drained_to = self.say_drained_to.max(g.to);
            self.say_bus_head = self.say_bus_head.max(g.to);
            self.replay_topics(false);
            return;
        }
        if source != Source::Inbox {
            stderr_line!(
                "aterm-link: the broker's retention gave up {lost} below the {} subscription; \
                 reading on from there",
                face_name(source)
            );
            return;
        }
        if self.attachment != Attachment::Inherited || self.endpoint_lane < LINK_GAP_LANE {
            let why = if self.attachment == Attachment::Inherited {
                "its aterm is older than the `link gap` lane"
            } else {
                "this bridge only observes"
            };
            stderr_line!(
                "aterm-link: the broker's retention gave up {lost} of this node's unread mail \
                 before this bridge read it; no session is told, as {why}"
            );
            return;
        }
        stderr_line!(
            "aterm-link: the broker's retention gave up {lost} of this node's unread mail \
             before this bridge read it; every session this node serves is told"
        );
        #[cfg(test)]
        if self.fx.mutant(crate::sim::world::Mutant::CommitPastTheGap) {
            // As if the whole range were gone: the held records in it are not.
            self.owe_inbox_commit(g.to.saturating_sub(1));
        }
        #[cfg(test)]
        if self.fx.mutant(crate::sim::world::Mutant::TellNoGap) {
            return;
        }
        let mut line = format!("link gap from={} to={}", g.from, g.to);
        if let Some(n) = g.records {
            line.push_str(&format!(" records={n}"));
        }
        match self.ctl_request(&line) {
            Ok(reply) if reply.ok() => {}
            Ok(reply) => stderr_line!(
                "aterm-link: aterm did not take `{line}`: {}",
                reply.header()
            ),
            Err(e) => stderr_line!("aterm-link: `{line}` failed: {e}"),
        }
    }

    /// WHAT THE BRIDGE'S OWN READ FOUND GIVEN UP on one session's lane (B12): its refill from
    /// the session's `seen` watermark, its deadline sweep, or (`topic`) its replay of a
    /// broadcast topic to it met retention below where it read: a `Fetch` refused below a floor
    /// (`retained-from=`), so the lane's records at `from..to`, if any, were given up before
    /// the session handled them; or, read told ([`Bridge::lane_page`]), a page's gap notice,
    /// so the records of `from..to` the page did not deliver were, `records` of them where the
    /// broker counted. The subjects are known here, unlike an inbox gap notice's
    /// ([`Self::on_gap`]): aterm is told for that session alone (`link gap from=<a> to=<b>
    /// [records=<n>] sid=<sid> [topic=<t>]`), and it judges whether its ring still holds mail
    /// of the range. Told once per (session, scope, `to`), a higher `to` telling again; a
    /// refusal carries no count, so none is said for one.
    fn tell_given_up(&mut self, sid: &str, topic: Option<&str>, gone: astream_broker::Gap) {
        let astream_broker::Gap { from, to, records } = gone;
        if from >= to || records == Some(0) {
            return;
        }
        let key = (sid.to_string(), topic.unwrap_or_default().to_string());
        if self
            .per_sid
            .given_up_told
            .get(&key)
            .is_some_and(|told| *told >= to)
        {
            return;
        }
        if self.attachment != Attachment::Inherited || self.endpoint_lane < LINK_GAP_SCOPED_LANE {
            return;
        }
        let mut line = format!("link gap from={from} to={to}");
        if let Some(n) = records {
            line.push_str(&format!(" records={n}"));
        }
        line.push_str(&format!(" sid={sid}"));
        if let Some(t) = topic {
            line.push_str(&format!(" topic={t}"));
        }
        match self.ctl_request(&line) {
            Ok(reply) if reply.ok() => {
                self.per_sid.given_up_told.insert(key, to);
            }
            Ok(reply) => stderr_line!(
                "aterm-link: aterm did not take `{line}`: {}",
                reply.header()
            ),
            Err(e) => stderr_line!("aterm-link: `{line}` failed: {e}"),
        }
    }

    fn owe_inbox_commit(&mut self, off: u64) {
        let Some(link) = self.conn.as_mut() else {
            return;
        };
        let waits = link.ride(off, |then| {
            matches!(
                then,
                Then::Ev {
                    record: Some(_),
                    ..
                }
            )
        });
        if !waits {
            self.inbox_pending = Some(self.inbox_pending.map_or(off, |have| have.max(off)));
        }
    }

    /// A ROUND WITH NO INBOX RECORD QUEUED HOLDS THE INBOX COMMIT for the loop's next write
    /// to carry (critic item B08), instead of sending it alone: the reply a session posts
    /// the moment a delivery woke it is that write, whether it is queued already or comes
    /// while the loop parks, and the commit for the delivery it answers goes out in the
    /// same burst ([`Bridge::take_held_commit`]), so the broker takes both into one batch
    /// behind one flush where it took two. Whether it is held: only while a commit sent now
    /// would name something ([`Bridge::inbox_commit_due`]) or a record rides an owed ack, on
    /// a link that can send it, and only until [`IDLE_ACK_GRACE`] has passed since a round
    /// first held it; otherwise the caller sends it now ([`Bridge::flush_inbox_commit`]),
    /// which sends nothing when no commit is due. So a commit a record owed to a session
    /// stops is never held, and the loop parks its full wait as it did before B08 rather
    /// than wake every grace to send nothing. A hold the round's settle makes is dated
    /// `since`, the round's start ([`Bridge::mirror_pass_then_settle`]), so what the round did
    /// ahead of its settle counts against the grace.
    ///
    /// What bounds it: the loop parks no longer than the grace left while it is held, and
    /// sends it once it has waited that out with nothing taken ([`Bridge::park_and_take`]).
    /// Only aterm's push lane brings the reply it waits for, so an item off the bus sends it
    /// before it is handled ([`Bridge::take_one`]); an endpoint line keeps it while it is
    /// handled, so one that publishes nothing keeps it for as long as it runs, and the next
    /// settle sends it once the grace is out. A held commit is the owed `inbox_pending`
    /// itself, so whatever lands the inbox commit (a cursor read, a subscription replaced,
    /// the wind-down) lands it too.
    fn hold_inbox_commit(&mut self, since: Instant) -> bool {
        let owed =
            self.inbox_commit_due().is_some() || self.conn.as_ref().is_some_and(Link::riding);
        if !owed || self.conn.is_none() || self.attachment == Attachment::Observer {
            self.commit_held = None;
            return false;
        }
        let now = self.fx.now();
        let since = *self.commit_held.get_or_insert(since.min(now));
        if now.saturating_duration_since(since) >= IDLE_ACK_GRACE {
            self.commit_held = None;
            return false;
        }
        true
    }

    /// The offset an inbox commit sent now would name: through what is owed, clamped as
    /// every commit is ([`Bridge::commit_target`]: below the first record still owed to a
    /// session or left unaccounted). `None` when none is due: no link that may commit,
    /// nothing owed past the committed cursor, a record owed to a session just past it, or
    /// an owed commit already covering it. Records still riding an owed ack are not counted:
    /// what they owe is committed by a later commit.
    fn inbox_commit_due(&self) -> Option<u64> {
        if self.conn.is_none() || self.attachment == Attachment::Observer {
            return None;
        }
        let upto = self.inbox_pending?;
        if self.committed_through(upto) {
            return None;
        }
        #[cfg(test)]
        let past_the_floor = self
            .fx
            .mutant(crate::sim::world::Mutant::HeldCommitPastTheFloor);
        #[cfg(not(test))]
        let past_the_floor = false;
        let off = if past_the_floor {
            upto
        } else {
            self.commit_target(upto)?
        };
        if self
            .conn
            .as_ref()
            .is_some_and(|link| link.owed().any(|then| then.commits_through(off)))
        {
            return None;
        }
        Some(off)
    }

    /// The held inbox commit ([`Bridge::hold_inbox_commit`]), taken for the publish about to
    /// go out to carry: what [`Bridge::inbox_commit_due`] names, or `None` when none is held
    /// or none is due.
    fn take_held_commit(&mut self) -> Option<u64> {
        self.commit_held.take()?;
        let off = self.inbox_commit_due()?;
        self.inbox_pending_records = 0;
        Some(off)
    }

    /// The inbox commit the bus mirror's burst carries (critic item C13): the held one
    /// ([`Bridge::take_held_commit`]), or, with no inbox record queued, the one the round's
    /// settle would hold for the loop's next write ([`Bridge::hold_inbox_commit`]). The
    /// mirror's pass runs before that settle ([`Bridge::mirror_then_round`]), so when the
    /// endpoint's line for a delivery's mail count was read before the delivery's round
    /// ended, the snapshot it owes is taken first, and its send IS that next write. With an
    /// inbox record queued the commit waits for the record's delivery, as the settle leaves
    /// it.
    pub(super) fn take_commit_for_the_mirror(&mut self) -> Option<u64> {
        #[cfg(test)]
        let only_held = self
            .fx
            .mutant(crate::sim::world::Mutant::MirrorRidesOnlyAHeldCommit);
        #[cfg(not(test))]
        let only_held = false;
        if self.commit_held.is_none()
            && (only_held || self.mailbox.inbox_queued() || !self.hold_inbox_commit(self.fx.now()))
        {
            return None;
        }
        self.take_held_commit()
    }

    /// Whether the bus mirror's burst, sent now, would carry an inbox commit
    /// ([`Bridge::take_commit_for_the_mirror`]): one is due ([`Bridge::inbox_commit_due`]), and
    /// a round holds it or no inbox record is queued (a queued record keeps a commit no round
    /// holds for its own delivery). A take that would carry none is held until one falls due, at
    /// most [`IDLE_ACK_GRACE`] (critic item E04, [`Bridge::mirror_rides_nothing`]). A commit
    /// whose record rides an owed ack is not due until that ack is read, which the round's
    /// settle or the park's backstop does.
    pub(super) fn mirror_has_a_commit_to_carry(&self) -> bool {
        (self.commit_held.is_some() || !self.mailbox.inbox_queued())
            && self.inbox_commit_due().is_some()
    }

    /// THE COMMIT AHEAD OF THE FENCE (critic item D06's review): a mirror pass whose open has
    /// yet to make the pin's fence ([`Bridge::make_the_pin_fence`]: two full flushes, or two
    /// barriers and the broker's settle, critic item F07)
    /// sends the inbox commit its burst would carry ([`Bridge::take_commit_for_the_mirror`])
    /// first, alone. The first delivery after an attach or a handoff goes ahead of that pass
    /// ([`Bridge::mirror_yields`]), so its commit trails it by no more than the round's
    /// [`IDLE_ACK_GRACE`], never by the fence and the take as well. An open holds its fence
    /// once its first pass made it, so this is one write of its own an open at most.
    pub(super) fn commit_ahead_of_the_fence(&mut self) {
        #[cfg(test)]
        if self
            .fx
            .mutant(crate::sim::world::Mutant::CommitBehindTheFence)
        {
            return;
        }
        let owed = self.commit_held.is_some()
            || (!self.mailbox.inbox_queued() && self.inbox_commit_due().is_some());
        if owed && !self.state.logpin_fence_held() {
            self.flush_inbox_commit();
        }
    }

    fn flush_inbox_commit(&mut self) {
        self.commit_held = None;
        self.inbox_pending_records = 0;
        let waiting = self.fx.now();
        while self.conn.as_ref().is_some_and(Link::riding) && self.settle_one(waiting) {}
        let Some(upto) = self.inbox_pending else {
            return;
        };
        if self.committed_through(upto) {
            self.inbox_pending = None;
            return;
        }
        if self.conn.is_none() {
            return;
        }
        if self.attachment == Attachment::Observer {
            self.inbox_pending = None;
            return;
        }
        self.commit_upto(upto);
    }

    fn wind_down(&mut self) {
        #[cfg(test)]
        if self.commit_held.is_some()
            && self
                .fx
                .mutant(crate::sim::world::Mutant::DropHeldCommitAtWindDown)
        {
            self.commit_held = None;
            self.inbox_pending = None;
        }
        if self.conn.is_some() {
            self.land_inbox_commit();
            self.settle_acks();
        }
        self.hand_over_inbox_commit();
        self.end_run();
    }

    fn hand_over_inbox_commit(&mut self) {
        if self.attachment == Attachment::Observer {
            return;
        }
        let Some(upto) = self.inbox_pending.and_then(|off| self.commit_target(off)) else {
            return;
        };
        if self.committed_through(upto) {
            return;
        }
        match self.state.set_inbox_owed(&self.node, upto) {
            Ok(()) => stderr_line!(
                "aterm-link: the inbox group's commit through {upto} is left for the next bridge \
                 (no broker connection to make it on)"
            ),
            Err(e) => stderr_line!(
                "aterm-link: could not leave the inbox group's commit through {upto} for the \
                 next bridge ({e}); it offers those records again"
            ),
        }
    }

    fn on_say_record(&mut self, rec: &BrokerRecord) {
        let (off, subject, raw) = rec;
        let frontier = self.say_drained_to;
        let next = off.saturating_add(1);
        self.say_drained_to = self.say_drained_to.max(next);
        self.say_bus_head = self.say_bus_head.max(next);
        if self.attachment == Attachment::Observer {
            return;
        }
        if self.fan_in_say(frontier, *off, subject, raw) == Delivery::Unaccounted {
            return;
        }
        self.advance_frontier(frontier, next);
    }

    fn fan_in_say(&mut self, frontier: u64, off: u64, subject: &str, raw: &[u8]) -> Delivery {
        // For a record that reaches here with none all the same, do not parse
        // or allocate; the caller still moves the head and checkpoint clocks.
        if self.per_sid.topics.is_empty() {
            return Delivery::Accounted;
        }
        let Some((say, body)) = say_record(&self.cfg.fleet, subject, raw) else {
            return Delivery::Accounted;
        };
        let recipients: Vec<(String, u64)> = self
            .per_sid
            .topics
            .iter()
            .filter(|(sid, _)| !self.per_sid.lane_owed.contains_key(*sid))
            .filter_map(|(sid, by_topic)| {
                let cursor = by_topic.get(&say.topic)?;
                (cursor.next >= frontier && cursor.next <= off).then(|| (sid.clone(), cursor.next))
            })
            .collect();
        let mut outcome = Delivery::Accounted;
        for (sid, expect) in recipients {
            if self.deliver_broadcast(&sid, &say, &body, off, expect) == Delivery::Unaccounted {
                outcome = Delivery::Unaccounted;
            }
        }
        outcome
    }

    fn advance_frontier(&mut self, frontier: u64, next: u64) {
        self.say_checkpoint_tick = self.say_checkpoint_tick.wrapping_add(1);
        let slot = self.say_checkpoint_tick % SAY_CURSOR_CHECKPOINT_SHARDS;
        let mut blocked = BTreeMap::new();
        let held = if self.per_sid.topic_floors_dirty.is_empty() {
            &self.per_sid.lane_owed
        } else {
            blocked.clone_from(&self.per_sid.lane_owed);
            blocked.extend(
                self.per_sid
                    .topic_floors_dirty
                    .iter()
                    .map(|sid| (sid.clone(), None)),
            );
            &blocked
        };
        for sid in advance_frontier_cursors(&mut self.per_sid.topics, held, frontier, next, slot) {
            self.persist_topics(&sid);
        }
    }

    fn deliver_broadcast(
        &mut self,
        sid: &str,
        say: &subject::SayAddr,
        body: &Body,
        off: u64,
        expect: u64,
    ) -> Delivery {
        let addr = subject::InAddr {
            node: self.node.clone(),
            sid: sid.to_string(),
            src: say.node.clone(),
            kind: body
                .kind
                .as_deref()
                .and_then(WakeClass::parse)
                .unwrap_or(WakeClass::Note),
        };
        let (kind, demoted, _stray) = self.classify_delivery(&addr, body);
        let (outcome, taken) = self.deliver_line_taking(&DeliverLine {
            addr: &addr,
            body,
            off,
            kind,
            demoted,
            late: false,
            topic: Some(&say.topic),
            was: None,
        });
        if outcome != Delivery::Accounted {
            return outcome;
        }
        if let Some(id) = taken {
            if !self.note_broadcast_taken(sid, &say.topic, id, off) {
                return Delivery::Unaccounted;
            }
        }
        self.advance_topic(sid, &say.topic, expect, off.saturating_add(1));
        outcome
    }

    fn advance_topic(&mut self, sid: &str, topic: &str, expect: u64, next: u64) {
        if self.per_sid.topic_floors_dirty.contains(sid) && !self.persist_topic_floors(sid) {
            return;
        }
        let moved = {
            let Some(by_topic) = self.per_sid.topics.get_mut(sid) else {
                return;
            };
            let Some(cursor) = by_topic.get_mut(topic) else {
                return;
            };
            if cursor.next != expect || next <= cursor.next {
                return;
            }
            cursor.next = next;
            true
        };
        if moved {
            self.persist_topics(sid);
        }
    }

    fn persist_topics(&mut self, sid: &str) -> bool {
        if self.per_sid.topic_floors_dirty.contains(sid) && !self.persist_topic_floors(sid) {
            return false;
        }
        let Some(snapshot) = self.per_sid.topics.get(sid).cloned() else {
            return true;
        };
        if let Err(e) = self.state.set_topics(sid, &snapshot) {
            stderr_line!("aterm-link: the broadcast cursors of {sid} could not be persisted: {e}");
            return false;
        }
        true
    }

    fn note_broadcast_taken(&mut self, sid: &str, topic: &str, id: u64, off: u64) -> bool {
        if id == 0
            || self
                .per_sid
                .handled_to
                .get(sid)
                .is_some_and(|seen| id <= *seen)
        {
            return !self.per_sid.topic_floors_dirty.contains(sid)
                || self.persist_topic_floors(sid);
        }
        let by_topic = self.per_sid.unhandled.entry(sid.to_string()).or_default();
        let before = floors_of(by_topic);
        note_unhandled(by_topic.entry(topic.to_string()).or_default(), id, off);
        while by_topic.values().map(VecDeque::len).sum::<usize>() > UNHANDLED_KEEP {
            let Some(oldest) = by_topic
                .iter()
                .filter_map(|(t, q)| q.front().map(|&(id, _)| (id, t.clone())))
                .min()
                .map(|(_, t)| t)
            else {
                break;
            };
            if let Some(q) = by_topic.get_mut(&oldest) {
                q.pop_front();
            }
            by_topic.retain(|_, q| !q.is_empty());
        }
        if floors_of(by_topic) != before || self.per_sid.topic_floors_dirty.contains(sid) {
            return self.persist_topic_floors(sid);
        }
        true
    }

    fn note_handled(&mut self, sid: &str, seen: u64) {
        let held = self.per_sid.handled_to.entry(sid.to_string()).or_default();
        if seen <= *held {
            return;
        }
        *held = seen;
        let Some(by_topic) = self.per_sid.unhandled.get_mut(sid) else {
            return;
        };
        let before = floors_of(by_topic);
        for queue in by_topic.values_mut() {
            prune_handled(queue, seen);
        }
        by_topic.retain(|_, q| !q.is_empty());
        let moved = floors_of(by_topic) != before;
        if by_topic.is_empty() {
            self.per_sid.unhandled.remove(sid);
        }
        if moved {
            self.persist_topic_floors(sid);
        }
    }

    fn forget_unhandled_topic(&mut self, sid: &str, topic: &str) {
        let Some(by_topic) = self.per_sid.unhandled.get_mut(sid) else {
            return;
        };
        if by_topic.remove(topic).is_none() {
            return;
        }
        if by_topic.is_empty() {
            self.per_sid.unhandled.remove(sid);
        }
        self.persist_topic_floors(sid);
    }

    fn persist_topic_floors(&mut self, sid: &str) -> bool {
        let floors = self
            .per_sid
            .unhandled
            .get(sid)
            .map(floors_of)
            .unwrap_or_default();
        if let Err(e) = self.state.set_topic_floors(sid, &floors) {
            stderr_line!(
                "aterm-link: the broadcast refill floors of {sid} could not be persisted: {e}"
            );
            self.per_sid.topic_floors_dirty.insert(sid.to_string());
            return false;
        }
        self.per_sid.topic_floors_dirty.remove(sid);
        true
    }

    /// Deliver the inbox record at `off`, read with the `origin` its read stamped on it
    /// ([`Bridge::moved_from`] reads it), or say what became of it.
    fn deliver_record(
        &mut self,
        off: u64,
        subject: &str,
        raw: &[u8],
        origin: crate::mailbox::Origin,
    ) -> Delivery {
        // OBSERVER MODE delivers nothing (§11.2: "presence and `ev` only, no
        // delivery, no hold"). It is checked HERE rather than left to the
        // endpoint's refusal so an observer produces one honest record per
        // message instead of a stream of `ERR denied`.
        if self.attachment == Attachment::Observer {
            self.publish_ev_for(None, &format!("undeliverable off={off} reason=observer"));
            return Delivery::Accounted;
        }
        if let Some(reply) = self.person_reply(subject) {
            return self.hand_to_person(off, &reply, raw, origin);
        }
        let addr = match subject::parse_in(&self.cfg.fleet, &self.node, subject) {
            Ok(a) => a,
            Err(reject) => {
                self.record_undeliverable(off, None, reject);
                return Delivery::Accounted;
            }
        };
        if !self.per_sid.epochs.contains_key(&addr.sid) {
            let _ = self.refresh_sessions();
            if !self.per_sid.epochs.contains_key(&addr.sid) {
                self.publish_ev(&format!(
                    "undeliverable off={off} reason={} sid={}",
                    Reject::NotHosted.token(),
                    addr.sid
                ));
                return Delivery::Accounted;
            }
        }
        if addr.src == self.node && self.is_forged_self(off) {
            // NOT JUDGED WHEN THE ANSWER THAT COULD CLEAR IT WAS LOST: the
            // publisher connection failed while an own-lane post of ours was
            // still owed its ack ([`Bridge::is_forged_self`]), so this record
            // may be that post. Its in-flight entry is the evidence now, and
            // the reconnect's attach replays it before the group offers the
            // record again ([`Bridge::settle_in_flight`]). Nothing is said and
            // nothing commits past it: with no connection the commit owes
            // nothing ([`Bridge::owe_inbox_commit`]) and a refill stops at its
            // next page.
            if self.conn.is_none() {
                return Delivery::Accounted;
            }
            self.record_undeliverable(off, Some(&addr.sid.clone()), Reject::ForgedSelf);
            return Delivery::Accounted;
        }
        if addr.src == self.node && !self.settled_before_delivery(off) {
            if self.link.down_wants_report("error") {
                let why = if self.held_for_the_drain(off) {
                    "aterm has not retired its post or receipt yet"
                } else {
                    "the state dir refused a write"
                };
                stderr_line!(
                    "aterm-link: @{off} is not delivered while its in-flight entry stands \
                     ({why}); reconnecting to offer it again"
                );
            }
            self.link_down("error");
            self.conn = None;
            return Delivery::Accounted;
        }
        let (body, _raw_tail) = Body::decode(raw);
        if !body.via.as_deref().is_none_or(via_ok) {
            return self.refuse_locally(&addr, &body, off, "via");
        }
        // Where it was first, read on its lane's own address: the person carry below names
        // another sender.
        let was = self.moved_from(&addr, origin);
        let addr = match self.carried_person(&addr, &body) {
            Ok(None) => addr,
            Ok(Some(src)) => subject::InAddr { src, ..addr },
            Err(why) => return self.refuse_locally(&addr, &body, off, why),
        };
        let (kind, demoted, stray) = self.classify_delivery(&addr, &body);
        let answers = body
            .re
            .filter(|_| !stray && ANSWER_KINDS.contains(&kind.as_str()));
        let expiry = body
            .re
            .filter(|_| kind == WakeClass::Expired && addr.src == self.node);
        // AN ANSWER, OR THIS NODE'S `expired`, IS DELIVERED ONLY ONCE THE DEADLINE IT SETTLES IS
        // OFF THE DISK (item F4), as an own record waits for its in-flight entry: a bridge that
        // dies between the two leaves the next one a deadline that a sweep expires once
        // retention gave the read record up, an `expired` after the answer or the `expired` the
        // asker holds.
        let settled = answers
            .or(expiry)
            .is_none_or(|re| self.settle_deadline_for(&addr.sid, re));
        #[cfg(test)]
        let settled = settled
            || self
                .fx
                .mutant(crate::sim::world::Mutant::DeliverOverAnUnwrittenSettle);
        if !settled {
            if self.link.down_wants_report("error") {
                stderr_line!(
                    "aterm-link: @{off} is not delivered while the deadline it settles stands in \
                     the deadline table (the state dir refused a write); reconnecting to offer it \
                     again"
                );
            }
            self.link_down("error");
            self.conn = None;
            return Delivery::Accounted;
        }
        let late = answers.is_some_and(|re| {
            self.expired
                .get(&re)
                .is_some_and(|asker| *asker == addr.sid)
        });
        let row = DeliverLine {
            addr: &addr,
            body: &body,
            off,
            kind,
            demoted,
            late,
            topic: None,
            was,
        };
        if !self.queue_rows || self.group_record != Some(off) {
            return self.deliver_line(&row);
        }
        let send = match self.deliver_text(&row) {
            Ok(send) => send,
            Err(done) => return done,
        };
        self.queued_rows.push(QueuedRow {
            subject: subject.to_string(),
            off,
            kind,
            demoted,
            late,
            send,
            addr,
            body,
        });
        Delivery::Queued
    }

    fn deliver_line(&mut self, row: &DeliverLine<'_>) -> Delivery {
        self.deliver_line_taking(row).0
    }

    fn deliver_line_taking(&mut self, row: &DeliverLine<'_>) -> (Delivery, Option<u64>) {
        let send = match self.deliver_text(row) {
            Ok(send) => send,
            Err(done) => return (done, None),
        };
        let answer = self.ctl_request(&send.line);
        self.deliver_answered(row, &send, answer)
    }

    /// `row`'s request line, or what became of the record when no line goes.
    fn deliver_text(&mut self, row: &DeliverLine<'_>) -> Result<DeliverSend, Delivery> {
        let &DeliverLine {
            addr,
            body,
            off,
            kind,
            demoted,
            late,
            topic,
            was,
        } = row;
        if self.per_sid.lane_owed.contains_key(&addr.sid) {
            return Err(Delivery::Owed);
        }
        let trust = trust_of(&addr.src, body.via.is_some());
        let from = self.render_from(addr, body);
        let mut line = format!(
            "deliver {} off={off} from={from} kind={kind} trust={trust}",
            addr.sid
        );
        if let Some(t) = topic {
            line.push_str(&format!(" topic={t}"));
        }
        if let Some(was) = was {
            line.push_str(&format!(" was={was}"));
        }
        if let Some(re) = body.re {
            line.push_str(&format!(" re={re}"));
        }
        if let Some(dl) = body.dl {
            line.push_str(&format!(" dl={dl}"));
        }
        if late {
            line.push_str(" late=1");
        }
        if let Some(d) = demoted {
            line.push_str(&format!(" demoted={d}"));
        }
        if let Some(via) = &body.via {
            line.push_str(&format!(" via={via}"));
        }
        if kind == WakeClass::Ack {
            if let Some(v) = &body.verdict {
                line.push_str(&format!(" verdict={v}"));
            }
        }
        let Some(budget) = REQUEST_LINE_MAX.checked_sub(line.len() + " len= text=".len() + 20)
        else {
            return Err(self.refuse_delivery(addr, body, off, topic, "oversize"));
        };
        let (encoded, cut) = encode_bounded(&body.text, budget);
        if cut {
            line.push_str(&format!(" len={}", body.text.len()));
        }
        line.push_str(&format!(" text={encoded}"));
        if self.fault == Fault::OversizeDeliverLine {
            self.fault = Fault::None;
            Fault::OversizeDeliverLine.fired(&self.state);
            line.push_str(&"x".repeat(REQUEST_LINE_MAX));
        }
        if line.len() > REQUEST_LINE_MAX {
            return Err(self.refuse_delivery(addr, body, off, topic, "oversize"));
        }
        if self.fault == Fault::LoseAtermBeforeDeliver {
            self.fault = Fault::None;
            Fault::LoseAtermBeforeDeliver.fired(&self.state);
            let _ = self.ctl.shutdown(std::net::Shutdown::Both);
        }
        Ok(DeliverSend { line, encoded, cut })
    }

    /// What aterm's answer to `row`'s line makes of the record, and the row id it took.
    fn deliver_answered(
        &mut self,
        row: &DeliverLine<'_>,
        send: &DeliverSend,
        answer: io::Result<Reply>,
    ) -> (Delivery, Option<u64>) {
        let &DeliverLine {
            addr,
            body,
            off,
            topic,
            ..
        } = row;
        let mut taken = None;
        let outcome = match answer {
            Ok(reply) if reply.ok() => {
                taken = reply
                    .header()
                    .split_whitespace()
                    .nth(1)
                    .and_then(crate::parse_uint::<u64>);
                Delivery::Accounted
            }
            Ok(reply) => match deliver_verdict(reply.header(), topic.is_some()) {
                Some(why) => self.refuse_delivery(addr, body, off, topic, why),
                None => self.hold_back(&addr.sid, off, reply.header()),
            },
            Err(e) => {
                stderr_line!("aterm-link: deliver failed: {e}");
                if self.ctl.lost() {
                    Delivery::Unaccounted
                } else {
                    self.refuse_delivery(addr, body, off, topic, "refused")
                }
            }
        };
        if send.cut && outcome == Delivery::Accounted && topic.is_none() {
            self.publish_ev_for(
                Some(&addr.sid),
                &format!(
                    "truncated off={off} len={} sent={}",
                    body.text.len(),
                    crate::pct::decode(&send.encoded).len()
                ),
            );
        }
        (outcome, taken)
    }

    /// PIPELINED DELIVERIES (critic item C08): the inbox records queued behind the one in hand
    /// take a round trip to aterm for as many of their lines as an empty socket takes at once,
    /// instead of one each ([`Ctl::pipeline`]), while aterm is this bridge's own build on its
    /// own lane. There aterm answers every `deliver` with a verdict, so `hold_back` (a reply no
    /// verdict names) never stops a sid with rows of it already sent behind the one that
    /// stopped it.
    ///
    /// The queued rows go, and are accounted, ahead of what comes after them. Every other lane
    /// request sends them first ([`Bridge::ctl_request`]), so aterm reads the lane in the order
    /// the bridge decided it. A record of the run that takes no line sends them before it is
    /// accounted itself ([`Bridge::on_inbox_record`]), so the inbox is accounted in offset
    /// order. A local refusal sends them before its notice ([`Bridge::refuse_locally`]), which
    /// commits the group through the refused record: each row ahead of it is accounted by then,
    /// and one aterm did not take holds that commit below itself. Every other refusal's notice
    /// is an `ev` ([`Bridge::record_undeliverable`]'s among them), which carries no commit but
    /// the held one, and that covers only records accounted already, every one of them ahead of
    /// the queued rows.
    fn on_inbox_run(&mut self, first: &BrokerRecord, origin: crate::mailbox::Origin) {
        // Which nodes this Mac held before, as the node service's hand-off says now: mail it moved
        // across a key rotation since this attach is this Mac's own (§11 C3b).
        self.refresh_old_nodes();
        let run = if self.lane_pipelines() {
            self.mailbox
                .take_inbox_run(self.fx.now(), DELIVER_RUN_MAX - 1)
        } else {
            Vec::new()
        };
        self.queue_rows = !run.is_empty();
        self.on_inbox_record(first, origin);
        for (rec, origin) in &run {
            self.mirror_took_inbox();
            self.on_inbox_record(rec, *origin);
        }
        self.queue_rows = false;
        self.send_rows();
    }

    /// Whether the verb lane takes pipelined requests ([`Ctl::pipeline`]): aterm is this bridge's
    /// own build on its own lane, attached by the descriptors it handed this bridge, so the
    /// bridge knows how it answers each request a batch carries (a `deliver` with a verdict,
    /// [`Bridge::on_inbox_run`]; an `outbox sent` with its status, [`Bridge::retire_on_the_lane`]).
    fn lane_pipelines(&self) -> bool {
        self.attachment == Attachment::Inherited
            && self.endpoint_lane == aterm_types::control_verbs::BRIDGE_LANE
            && self.endpoint_build.as_deref() == Some(aterm_stamp::build_id().as_str())
    }

    /// Send what [`Bridge::deliver_record`] queued, in order and in as few writes as
    /// [`Ctl::pipeline`] keeps whole, and account each record by its reply, as
    /// [`Bridge::on_inbox_record`] accounts one it delivered alone.
    fn send_rows(&mut self) {
        if self.queued_rows.is_empty() {
            return;
        }
        let rows = std::mem::take(&mut self.queued_rows);
        #[cfg(test)]
        self.rows_sent.push(rows.len());
        let lines: Vec<&str> = rows.iter().map(|row| row.send.line.as_str()).collect();
        let mut started = self.waits.starts();
        let (ctl, waits) = (&mut self.ctl, &mut self.waits);
        let replies = self.fx.lane(|| {
            ctl.pipeline(&lines, |_| {
                if started.is_some() {
                    waits.note(crate::wait_tally::Row::LaneDeliver, started);
                    started = waits.starts();
                }
            })
        });
        note_ctl_loss(
            &self.ctl,
            &self.mailbox,
            replies.iter().find_map(|reply| reply.as_ref().err()),
        );
        let held = self.group_record.take();
        for (row, answer) in rows.iter().zip(replies) {
            self.group_record = Some(row.off);
            let view = DeliverLine {
                addr: &row.addr,
                body: &row.body,
                off: row.off,
                kind: row.kind,
                demoted: row.demoted,
                late: row.late,
                topic: None,
                was: None,
            };
            let (delivery, _) = self.deliver_answered(&view, &row.send, answer);
            self.group_record = None;
            self.account_inbox(row.off, &row.subject, delivery);
        }
        self.group_record = held;
    }

    fn refuse_delivery(
        &mut self,
        addr: &subject::InAddr,
        body: &Body,
        off: u64,
        topic: Option<&str>,
        why: &str,
    ) -> Delivery {
        match topic {
            Some(t) => {
                stderr_line!(
                    "aterm-link: broadcast off={off} topic={t} not delivered to {}: {why}",
                    addr.sid
                );
                Delivery::Accounted
            }
            None => self.refuse_locally(addr, body, off, why),
        }
    }

    fn hold_back(&mut self, sid: &str, off: u64, header: &str) -> Delivery {
        if !self.per_sid.lane_owed.contains_key(sid) {
            self.per_sid.lane_owed.insert(sid.to_string(), None);
            let reply = reason_token(header);
            stderr_line!(
                "aterm-link: aterm answered `deliver {sid} off={off}` with no verdict ({}): \
                 this bridge and aterm do not read the lane alike; {sid}'s mail stays on the \
                 bus for a bridge that does, and this one delivers it nothing more",
                header
                    .chars()
                    .filter(|c| !c.is_control())
                    .take(160)
                    .collect::<String>()
            );
            self.publish_ev_for(
                Some(sid),
                &format!(
                    "lane-fault off={off} verb=deliver reply={reply} lane={}",
                    self.endpoint_lane
                ),
            );
        }
        Delivery::Owed
    }

    fn is_forged_self(&mut self, off: u64) -> bool {
        let waiting = self.fx.now();
        while self.window_accuses(off)
            && self
                .conn
                .as_ref()
                .is_some_and(|link| link.owed().any(Then::own_lane_post))
            && self.settle_one(waiting)
        {}
        self.window_accuses(off)
    }

    /// [`forged_self`] over this bridge's window and its floor: nothing while the floor waits
    /// for the log's head ([`Bridge::self_floor`]).
    fn window_accuses(&self, off: u64) -> bool {
        #[cfg(test)]
        let floor = if self.fx.mutant(crate::sim::world::Mutant::FloorOffTheOldest) {
            Some(self.self_acked.first().copied().unwrap_or(0))
        } else {
            self.self_floor
        };
        #[cfg(not(test))]
        let floor = self.self_floor;
        floor.is_some_and(|floor| forged_self(&self.self_acked, floor, off))
    }

    /// The kind `addr`'s record is delivered as, the kind it claimed when the receiver demoted
    /// it (every relayed record's, §6.7), and whether it is a stray reply. The kind is
    /// astream's sender demotion ([`WakeClass::classify_for_sender`], claim
    /// `wire.wake-and-control-acks`) over this bridge's reading of the record: its sender
    /// accepted ([`Self::accepted`]) or not, relayed (`via=`) or not, and a stray reply or not,
    /// by the asks table (an answer, report or ack to an ask that went to another principal)
    /// or by its node (another node's `expired`).
    fn classify_delivery(
        &mut self,
        addr: &subject::InAddr,
        body: &Body,
    ) -> (WakeClass, Option<WakeClass>, bool) {
        let stray = if addr.kind == WakeClass::Expired {
            !self.is_my_node(&addr.src)
        } else {
            ANSWER_KINDS.contains(&addr.kind.as_str())
                && body.re.is_some_and(|re| {
                    self.read_where_asks_went(re);
                    self.reply_from(re, &addr.src) == Some(false)
                })
        };
        let relayed = body.via.is_some();
        #[cfg(test)]
        let relayed = relayed
            && !self
                .fx
                .mutant(crate::sim::world::Mutant::RelayedKeepsItsKind);
        let kind = addr
            .kind
            .classify_for_sender(self.accepted(&addr.src), relayed, stray);
        let demoted = (kind != addr.kind || relayed).then_some(addr.kind);
        (kind, demoted, stray)
    }

    /// The person another Mac's record speaks for (§11 C3c), as `<person>@<node>`: `None` for a
    /// record that names none (no [`crate::person_key::BY_TOKEN`]). It is the person's here only
    /// once its seal verifies under a certificate of the node key that names the Mac whose lane
    /// brought it, at this bridge's clock ([`crate::person_key::carried_by`], which every
    /// observer view reads it by too). The sender becomes `<person>@<node>`, so it reads as a
    /// human's ([`trust_of`]) and says which Mac's.
    ///
    /// # Errors
    /// The `reason=` it is refused under: a record that names a person from no Mac's lane or
    /// names no person, or whose seal does not verify ([`crate::person_key::Refusal::token`]).
    fn carried_person(
        &self,
        addr: &subject::InAddr,
        body: &Body,
    ) -> Result<Option<String>, &'static str> {
        use crate::person_key::{self, Refusal};
        #[cfg(test)]
        if TRUST_ANY_PERSON.with(std::cell::Cell::get) {
            if let Some(by) = body.unknown.get(person_key::BY_TOKEN) {
                return Ok(Some(format!("{by}@{}", addr.src)));
            }
        }
        person_key::carried_by(
            &self.cfg.fleet,
            &addr.sid,
            &addr.src,
            addr.kind.as_str(),
            body,
            self.fx.wall_ms(),
        )
        .map_err(Refusal::token)
    }

    /// Another Mac's record for a person of this Mac (§11 C3d), with the mesh on:
    /// `/f/<F>/in/<me>/<person>/<src>/<kind>`, the lane back to the person that Mac's bridge
    /// wrote ([`Bridge::person_route`]) and the push carried here, `src` being that Mac's node,
    /// as the push's grant forces it. `None` for any other subject.
    fn person_reply(&self, subject: &str) -> Option<PersonReply> {
        self.cfg.mesh.as_ref()?;
        let rest = subject.strip_prefix(&format!("/f/{}/in/{}/", self.cfg.fleet, self.node))?;
        let mut parts = rest.split('/');
        let (person, src, kind) = (parts.next()?, parts.next()?, parts.next()?);
        let fits = parts.next().is_none()
            && crate::person::is_person_id(person)
            && src.starts_with("n-")
            && subject::is_principal(src)
            && !self.is_my_node(src);
        fits.then_some(())?;
        Some(PersonReply {
            person: person.to_owned(),
            src: src.to_owned(),
            kind: WakeClass::parse(kind)?,
        })
    }

    /// HAND ANOTHER MAC'S ANSWER TO A PERSON OF THIS MAC TO THEIR INBOX (§11 C3d). The record at
    /// `off`, on the lane back to `reply.person` ([`Bridge::person_reply`]), is written again on
    /// the person's lane here, `/f/<F>/in/p/<person>/<me>/<kind>`, which their `inbox` reads: from
    /// the session that wrote it (`from=<sid>@<src>`), its `re=` the offset the person's own post
    /// has here ([`Bridge::person_re`]), and with none of [`crate::person_key`]'s tokens, so
    /// nothing in it speaks as a person. It is published as a notice is
    /// ([`Bridge::send_notice`]): numbered by `off`, so a replay writes it once, and with the
    /// commit of the inbox group through `off`. One that names no session, or that a hop this
    /// bridge does not trust relayed, is refused and journaled; one whose `re=` names no post of
    /// the person carried from here keeps no `re=`, and the journal says so.
    ///
    /// AN ANSWER TO THE COPY NAMES WHAT IT ANSWERS AS THE SENDER'S MAC DOES: the copy carries
    /// [`crate::person::WIRE_RE_TOKEN`], the offset the record has on its sender's bus and the
    /// producer that wrote it there, which the push's `origin` stamp says, followed back across
    /// this Mac's own rotations ([`Bridge::first_of_moves`], as [`Bridge::wire_re`] maps a
    /// session's answer), and only this bridge's: one the record brought is dropped, and none is
    /// stamped where the origin says no offset there (a move not followed back, or a sequence a
    /// receipt is numbered by). A person's answer to the copy carries the offset as its `re=`
    /// where the producer is the sender's Mac's, by the hand-off it is routed by
    /// ([`crate::person::wire_re`]), so the session that asked, whose wait is on the offset its
    /// ask has on its own Mac, reads the person's answer as one. A copy no move put here is
    /// stamped with no hand-off read. A moved one is followed back by the hand-off's retired
    /// nodes and the node service's reader's ring; where it cannot be (a hand-off that is stale,
    /// missing or unreadable, or a ring that does not reach), it is handed with no stamp, and the
    /// inbox goes on: a bridge of this Mac's old node runs on until aterm relaunches it, and the
    /// next node's move waits for it, so a copy left unread would hold its inbox that long.
    fn hand_to_person(
        &mut self,
        off: u64,
        reply: &PersonReply,
        raw: &[u8],
        origin: crate::mailbox::Origin,
    ) -> Delivery {
        // THE ROWS QUEUED AHEAD GO FIRST, as before a notice ([`Bridge::refuse_locally`]): this
        // write can commit the group through `off`.
        self.send_rows();
        let (mut body, _) = Body::decode(raw);
        let sid = body
            .from
            .clone()
            .filter(|sid| sid.starts_with("s-") && subject::is_principal(sid));
        let Some(sid) = sid else {
            self.record_undeliverable(off, None, Reject::Malformed);
            return Delivery::Accounted;
        };
        if !body.via.as_deref().is_none_or(via_ok) {
            self.publish_ev(&format!("undeliverable off={off} reason=via"));
            return Delivery::Accounted;
        }
        use crate::person_key::{BY_TOKEN, CERT_TOKEN, SEAL_TOKEN};
        for token in [
            BY_TOKEN,
            CERT_TOKEN,
            SEAL_TOKEN,
            crate::person::WIRE_RE_TOKEN,
        ] {
            body.unknown.remove(token);
        }
        #[cfg(test)]
        let stamped = !UNSTAMPED_COPY.with(std::cell::Cell::get);
        #[cfg(not(test))]
        let stamped = true;
        // Its own lane names the person where a session's names the session: a move across
        // this Mac's own rotation read back on it as on any lane ([`Bridge::first_of_moves`]).
        let addr = subject::InAddr {
            node: self.node.clone(),
            sid: reply.person.clone(),
            src: reply.src.clone(),
            kind: reply.kind,
        };
        // A move not followed back (a hand-off that is stale, missing or unreadable, or a ring
        // that does not reach the old lane) is handed with no stamp, and the inbox goes on.
        let first = match self.first_of_moves(&addr, off, origin) {
            Ok(Ok(first)) => Some(first),
            Ok(Err(_)) => None,
            Err(e) => return self.unread_for_person(off, &reply.person, &e),
        };
        // Another Mac's write: under no producer of this Mac's (a move not followed back), and
        // below the sequences a receipt is numbered by.
        let mine = |producer: u64| {
            std::iter::once(&self.node)
                .chain(&self.old_nodes)
                .any(|node| astream_cap::producer_id_of(node) == producer)
        };
        let pushed = first
            .and_then(|(_, origin)| origin)
            .filter(|(producer, seq)| *seq < astream_broker::ACK_SEQ_BASE && !mine(*producer));
        #[cfg(test)]
        let pushed = if STAMP_BY_SENDER_SEQ.with(std::cell::Cell::get) {
            pushed.filter(|origin| self.sender_seq(&reply.src, Some(*origin)).is_some())
        } else {
            pushed
        };
        if let Some((producer, seq)) = pushed.filter(|_| stamped) {
            body.unknown.insert(
                crate::person::WIRE_RE_TOKEN.to_owned(),
                crate::person::wire_stamp(seq, producer),
            );
        }
        #[cfg(test)]
        let mapped = body
            .re
            .filter(|_| !WIRE_RE_TO_PERSON.with(std::cell::Cell::get));
        #[cfg(not(test))]
        let mapped = body.re;
        if let Some(re) = mapped {
            body.re = match self.person_re(re, &reply.person, &sid) {
                Ok(Some(post)) => Some(post),
                Ok(None) => {
                    self.publish_ev(&format!(
                        "unmapped off={off} re={re} person={}",
                        reply.person
                    ));
                    None
                }
                Err(e) => return self.unread_for_person(off, &reply.person, &e),
            };
        }
        body.from = Some(format!("{sid}@{}", reply.src));
        let lane = subject::person_lane(&self.cfg.fleet, &reply.person, &self.node);
        let subject = format!("{lane}/{}", reply.kind.as_str());
        self.send_notice(off, subject, body.encode());
        Delivery::Accounted
    }

    /// What becomes of the record at `off` for `person` while what it names cannot be read
    /// ([`Bridge::hand_to_person`]): nothing is written, and the link goes down, so the record is
    /// offered again once it is back.
    fn unread_for_person(&mut self, off: u64, person: &str, e: &io::Error) -> Delivery {
        if self.link.down_wants_report("error") {
            stderr_line!(
                "aterm-link: @{off} for {person} is not handed to their inbox while what it \
                 names cannot be read ({e}); reconnecting to offer it again"
            );
        }
        self.link_down("error");
        self.conn = None;
        Delivery::Accounted
    }

    /// The offset of `person`'s own post that the record at `re` on this bus carried to `sid`
    /// (§11 C3d): another Mac's answer or receipt names the carried copy by the offset it has
    /// here ([`Bridge::wire_re`] there), and the person carry wrote that copy on this Mac's lane
    /// to `sid` under [`crate::federate::CARRIER`], numbered by the post's offset and naming the
    /// person ([`crate::person_key::BY_TOKEN`]). `None` for a record that is no such copy, or one
    /// retention gave up.
    ///
    /// # Errors
    /// A broker that cannot be read.
    fn person_re(&self, re: u64, person: &str, sid: &str) -> io::Result<Option<u64>> {
        let lanes = format!("/f/{}/in/*/{sid}/{}/*", self.cfg.fleet, self.node);
        let Some(((_, _, raw), origin)) = self.held_record(re, &lanes, &[])? else {
            return Ok(None);
        };
        let carrier = astream_cap::producer_id_of(crate::federate::CARRIER);
        let by = Body::decode(&raw)
            .0
            .unknown
            .remove(crate::person_key::BY_TOKEN);
        Ok(match origin {
            Some((pid, post))
                if pid == carrier
                    && by.as_deref() == Some(person)
                    && post < astream_broker::ACK_SEQ_BASE =>
            {
                Some(post)
            }
            _ => None,
        })
    }

    fn accepted(&self, src: &str) -> bool {
        src.starts_with("h-") || self.cfg.accept_from.iter().any(|p| p == src)
    }

    fn render_from(&self, addr: &subject::InAddr, body: &Body) -> String {
        match &body.from {
            Some(sid)
                if addr.src.starts_with("n-")
                    && sid.starts_with("s-")
                    && subject::is_principal(sid)
                    && (self.is_my_node(&addr.src) || self.may_speak_for(&addr.src, sid)) =>
            {
                format!("{sid}@{}", addr.src)
            }
            _ => addr.src.clone(),
        }
    }

    fn may_speak_for(&self, src: &str, sid: &str) -> bool {
        !self.per_sid.epochs.contains_key(sid)
            && self.state.pins().get(sid).is_none_or(|n| n == src)
    }

    fn record_undeliverable(&mut self, off: u64, sid: Option<&str>, reject: Reject) {
        self.publish_ev_for(
            sid,
            &format!("undeliverable off={off} reason={}", reject.token()),
        );
        if reject == Reject::ForgedSelf {
            self.publish_ev_for(sid, &format!("cap-compromised off={off}"));
        }
    }

    fn refuse_locally(
        &mut self,
        addr: &subject::InAddr,
        body: &Body,
        off: u64,
        why: &str,
    ) -> Delivery {
        // THE ROWS QUEUED AHEAD GO FIRST ([`Bridge::on_inbox_run`]): the notice below can commit
        // the group through `off`, past every one of them, and each must be accounted by then.
        #[cfg(test)]
        let after = self
            .fx
            .mutant(crate::sim::world::Mutant::RefusalAheadOfQueuedRows);
        #[cfg(not(test))]
        let after = false;
        if !after {
            self.send_rows();
        }
        self.publish_ev_for(
            Some(&addr.sid),
            &format!("undeliverable off={off} reason={why}"),
        );
        self.notify_sender_undeliverable(addr, body, off, why);
        Delivery::Accounted
    }

    fn notify_sender_undeliverable(
        &mut self,
        addr: &subject::InAddr,
        body: &Body,
        off: u64,
        why: &str,
    ) {
        // A VERDICT NEVER EARNS A VERDICT — see [`VERDICT_KINDS`]. The notice is
        // a record like any other: addressed, delivered, and refusable. With two
        // saturated quotas, answering a refused `undeliverable` with another one
        // is a publish loop with a durable `fsync` per iteration and nothing that
        // bounds it. The refusal is still recorded as an `ev` by the caller; what
        // does not happen is another notice.
        if VERDICT_KINDS.contains(&addr.kind) {
            self.publish_ev_for(
                Some(&addr.sid),
                &format!("unnotified off={off} src={} reason=verdict", addr.src),
            );
            return;
        }
        let prefix = match self.sender_lane(addr, body) {
            Ok(prefix) => prefix,
            Err(no) => {
                let reason = match no {
                    NoLane::Undrained => why,
                    NoLane::Capped => "lane-cap",
                    NoLane::Handoff(no) => no.word(),
                };
                self.publish_ev_for(
                    Some(&addr.sid),
                    &format!("unnotified off={off} src={} reason={reason}", addr.src),
                );
                return;
            }
        };
        // The record it refuses as its sender's bus numbers it, with the mesh on (§11 C3d), as a
        // receipt names it ([`Bridge::wire_re`]).
        let re = match self.wire_re(&prefix, off) {
            Ok(re) => re,
            Err(e) => {
                self.publish_ev_for(
                    Some(&addr.sid),
                    &format!("unnotified off={off} src={} reason=unreadable", addr.src),
                );
                stderr_line!("aterm-link: could not name @{off} for its notice: {e}");
                return;
            }
        };
        let subject = format!("{prefix}/undeliverable");
        let mut notice = Body::new(self.fx.wall_ms());
        notice.re = Some(re);
        notice.from = Some(addr.sid.clone());
        notice.text = format!("not delivered: {why}");
        self.send_notice(off, subject, notice.encode());
    }

    /// Publish `encoded` on `subject`, a record this bridge writes for the inbox record at `off`
    /// (an `undeliverable` notice, or another Mac's answer to a person handed to their inbox,
    /// [`Bridge::hand_to_person`]), under this bridge's producer numbered by `off` so a replay
    /// writes it once: with the commit of the inbox group through `off` where the drain owes it.
    fn send_notice(&mut self, off: u64, subject: String, encoded: Vec<u8>) {
        let seq = astream_broker::ACK_SEQ_BASE | off;
        let commits = self.group_record == Some(off)
            && self.attachment != Attachment::Observer
            && self.inbox_commit_floor().is_none();
        let published = if commits {
            let pin = plain_pin(seq, &subject, &encoded);
            self.publish_journaled_answering(&pin, &subject, &encoded, Some(off))
                .map(drop)
        } else {
            self.publish_own_at(seq, &subject, &encoded).map(drop)
        };
        self.notice_sent(off, subject, encoded, published);
    }

    /// The end of an `undeliverable` notice's publish for the record at `off`: one the broker's
    /// rate limit refused, or the bridge held back behind it, is kept for its resend
    /// ([`Bridge::notices_held`]); any other end is the notice's last.
    fn notice_sent(&mut self, off: u64, subject: String, body: Vec<u8>, sent: io::Result<()>) {
        match sent {
            Err(e) if crate::rate_hold::rate_limited(&e) => {
                #[cfg(test)]
                if self
                    .fx
                    .mutant(crate::sim::world::Mutant::NoticeHeldIsDropped)
                {
                    self.notices_held.remove(&off);
                    stderr_line!("aterm-link: could not publish {subject}: {e}");
                    return;
                }
                self.notices_held.insert(off, (subject, body));
            }
            Err(e) => {
                self.notices_held.remove(&off);
                stderr_line!("aterm-link: could not publish {subject}: {e}");
            }
            Ok(()) => {
                self.notices_held.remove(&off);
            }
        }
    }

    /// SEND AGAIN THE NOTICES THE BROKER'S RATE LIMIT HELD BACK ([`Bridge::notices_held`]),
    /// oldest first, each at its own sequence, once no hold keeps this producer's publishes
    /// ([`Bridge::rate`]). One the limit refuses again waits with every later one for the next
    /// round; the inbox commit follows each that lands ([`Bridge::inbox_commit_floor`]).
    fn resend_held_notices(&mut self) {
        while self.conn.is_some() && self.rate_held().is_none() {
            let Some((off, (subject, body))) = self.notices_held.pop_first() else {
                return;
            };
            let seq = astream_broker::ACK_SEQ_BASE | off;
            let sent = self.publish_own_at(seq, &subject, &body).map(drop);
            let again = sent.as_ref().is_err_and(crate::rate_hold::rate_limited);
            self.notice_sent(off, subject, body, sent);
            if again {
                return;
            }
        }
    }

    fn send_receipt(&mut self, r: &OwedReceipt) -> io::Result<()> {
        if !self.cfg.receipts
            || !WAITING_KINDS.contains(&r.kind.as_str())
            || !crate::body::is_verdict(&r.verdict)
        {
            self.retire_receipt(r);
            return Ok(());
        }
        if r.earlier_log {
            self.publish_ev_for(
                Some(&r.sid),
                &format!(
                    "unacked re={} verdict={} reason=log-replaced",
                    r.off, r.verdict
                ),
            );
            self.retire_receipt(r);
            return Ok(());
        }
        let prefix = match self.receipt_lane(&r.from)? {
            Ok(prefix) => prefix,
            Err(no) => {
                let reason = match no {
                    NoLane::Undrained => "no-lane",
                    NoLane::Capped => "lane-cap",
                    NoLane::Handoff(no) => no.word(),
                };
                self.publish_ev_for(
                    Some(&r.sid),
                    &format!("unacked re={} verdict={} reason={reason}", r.off, r.verdict),
                );
                self.retire_receipt(r);
                return Ok(());
            }
        };
        let subject = format!("{prefix}/ack");
        let re = self.wire_re(&prefix, r.off)?;
        let encoded = receipt_body(self.fx.wall_ms(), &r.sid, re, &r.verdict).encode();
        let pinned = self.state.receipt_pin(&r.sid, r.rid)?;
        #[cfg(test)]
        let pinned = pinned.filter(|_| !self.fx.mutant(crate::sim::world::Mutant::FreshReceiptSeq));
        let retry = pinned.is_some();
        let pin = match pinned {
            Some(pin) => pin,
            None => {
                let pin = PublishPin {
                    seq: self.next_seq()?,
                    via_key: false,
                    identity: Some(publication_identity(&subject, &encoded)),
                    original: None,
                    original_off: None,
                    owner: None,
                    at: Some(self.bus_high),
                };
                self.state.set_receipt_pin(&r.sid, r.rid, &pin)?;
                pin
            }
        };
        // A retry at a pin an earlier attempt took, read back first: past a cut astream's
        // dedup no longer answers the pin with the record that attempt landed.
        if retry {
            if let Some(off) = self.landed_at_its_pin(&pin, &subject, &encoded)? {
                self.claim(off, pin.seq);
                // The entry an earlier attempt left standing, its record held for this retry
                // (`Bridge::awaiting_drain`): aterm's answer here ends it, as a post's read back
                // does ([`Bridge::land_group`]).
                let entry = self.awaiting_drain.get(&pin.seq).map(|&(_, ours)| ours);
                self.receipt_landed(r, subject, pin, off, false, entry);
                return Ok(());
            }
        }
        // Past its lane's hold only to be retired where the replay read it back, never sent
        // ([`Bridge::landed_behind_an_unverifiable_hold`]).
        if self.unverified.holds(
            &r.sid,
            crate::unverified_hold::Domain::Receipts,
            self.fx.now(),
        ) {
            return Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                Unsent(format!(
                    "{}'s receipts wait behind an unverifiable reservation",
                    r.sid
                )),
            ));
        }
        // Its entry stands until aterm retired the receipt ([`Bridge::land_group`]); the control
        // ends it at the ack, as every build before critic item F15 did.
        #[cfg(test)]
        let at_the_ack = self
            .fx
            .mutant(crate::sim::world::Mutant::ReceiptRetiredAtAck);
        #[cfg(not(test))]
        let at_the_ack = false;
        let published = match self.staged_refusal(&r.sid) {
            Some(refused) => Err(refused),
            None if at_the_ack => self
                .publish_journaled(&pin, &subject, &encoded)
                .map(|(off, deduped, actual)| (off, deduped, actual, None)),
            None => self.publish_journaled_entry(&pin, &subject, &encoded, None),
        };
        let (at, deduped, _, entry) = match published {
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
                self.state.clear_receipt_seq(&r.sid, r.rid);
                return Err(e);
            }
            // UNVERIFIABLE (critic item F21): the receipt stays owed at its pin, and its
            // session's receipts wait on a back-off; every other session's go on.
            Err(e) if unverifiable(&e) && self.conn.is_some() => {
                let what = format!("the receipt for @{}", r.off);
                self.hold_unverifiable(
                    &r.sid,
                    crate::unverified_hold::Domain::Receipts,
                    pin.seq,
                    &what,
                    &e,
                );
                return Err(e);
            }
            Err(e) if refused_for_good(&e) => {
                stderr_line!(
                    "aterm-link: the broker refused the receipt for {}: {e}",
                    r.off
                );
                self.publish_ev_for(
                    Some(&r.sid),
                    &format!("unacked re={} verdict={} reason=refused", r.off, r.verdict),
                );
                self.retire_receipt(r);
                return Ok(());
            }
            Err(e) if past_the_budget(&e) && self.writes_alone(&subject) => {
                stderr_line!(
                    "aterm-link: the receipt for {} opens a lane past this node's subject \
                     budget: {e}",
                    r.off
                );
                self.publish_ev_for(
                    Some(&r.sid),
                    &format!(
                        "unacked re={} verdict={} reason={SUBJECT_BUDGET}",
                        r.off, r.verdict
                    ),
                );
                self.retire_receipt(r);
                return Ok(());
            }
            answer => answer?,
        };
        if retry {
            self.claim(at, pin.seq);
        }
        if !deduped {
            self.publish_ev_for(
                Some(&r.sid),
                &format!("ack re={} off={at} verdict={}", r.off, r.verdict),
            );
        }
        self.receipt_landed(r, subject, pin, at, deduped, entry);
        Ok(())
    }

    /// A RECEIPT THE BROKER LANDED at `off`, settled alone through a post's steps (critic item
    /// F15, [`Bridge::land_group`]): its offset in the self-lane log where its record is this
    /// node's own, aterm's `deliver <sid> receipt=<rid> off=<off>`, then its in-flight entry's
    /// end (`entry`, the one its publish or an earlier attempt left standing) and its
    /// reservation's. So the entry stands until the receipt left aterm's outbox, and its record
    /// is not delivered before: a bridge killed between the ack and the retirement leaves the
    /// next one the entry, whose replay holds the record until the drain's retry at the pin
    /// reads it back and retires the receipt, so a cut before that retry gives up a copy no
    /// reader was handed.
    fn receipt_landed(
        &mut self,
        r: &OwedReceipt,
        subject: String,
        pin: PublishPin,
        off: u64,
        deduped: bool,
        entry: Option<bool>,
    ) {
        self.land_alone(Landing {
            of: Outboxed::Receipt(r.clone()),
            subject,
            pin,
            off,
            deduped,
            actual: None,
            entry,
        });
    }

    /// Retire an owed receipt that was not published (`off=-`): no record, so no entry.
    fn retire_receipt(&mut self, r: &OwedReceipt) {
        match self.ctl_request(&format!("deliver {} receipt={} off=-", r.sid, r.rid)) {
            Ok(reply) if left_the_outbox(&reply) => {
                self.state.clear_receipt_seq(&r.sid, r.rid);
            }
            Ok(reply) => stderr_line!(
                "aterm-link: retiring receipt {} for {} refused: {}",
                r.rid,
                r.sid,
                reply.header()
            ),
            Err(e) => stderr_line!(
                "aterm-link: retiring receipt {} for {} failed: {e}",
                r.rid,
                r.sid
            ),
        }
    }

    fn receipt_lane(&mut self, from: &str) -> io::Result<Result<String, NoLane>> {
        let fleet = self.cfg.fleet.clone();
        if let Some((sid, node)) = from.split_once('@') {
            if crate::person::is_person_id(sid) {
                return self.person_answer_lane(sid, node);
            }
            if !(sid.starts_with("s-")
                && subject::is_principal(sid)
                && node.starts_with("n-")
                && subject::is_principal(node))
            {
                return Ok(Err(NoLane::Undrained));
            }
            let node = if self.cfg.mesh.is_some() && node != self.node {
                match self.member_for(node) {
                    Ok(Some(member)) => member,
                    Ok(None) => return Ok(Err(NoLane::Undrained)),
                    Err(no) => return Ok(Err(NoLane::Handoff(no))),
                }
            } else {
                node.to_owned()
            };
            let lane = format!("/f/{fleet}/in/{node}/{sid}/{}", self.node);
            let allowed = self.may_answer_on(&node, &lane)?;
            return Ok(allowed.then_some(lane).ok_or(NoLane::Capped));
        }
        if !subject::is_principal(from) {
            return Ok(Err(NoLane::Undrained));
        }
        if from.starts_with("s-") {
            return Ok(match self.resolve_to(&format!("@{from}"), None)? {
                Route::To(prefix) => Ok(prefix),
                _ => Err(self
                    .unrouted
                    .take()
                    .map_or(NoLane::Undrained, NoLane::Handoff)),
            });
        }
        if from.starts_with("n-") {
            return Ok(Err(NoLane::Undrained));
        }
        Ok(Ok(subject::person_lane(&fleet, from, &self.node)))
    }

    /// The lane back to `person` on the Mac `node` names (§11 C3d), as a person's post another
    /// Mac carried here names them (`<person>@<node>`, [`Bridge::carried_person`]): this Mac's own
    /// node is the person's lane here, and another member, the one `node` is now, takes it on this
    /// Mac's lane to that member under the person's name, `/f/<F>/in/<member>/<person>/<me>`,
    /// which the push to that member carries there ([`crate::federate::mail_lanes`]) and its
    /// bridge hands to the person's inbox ([`Bridge::hand_to_person`]). The member is named
    /// beside the lane when it is another Mac's.
    ///
    /// # Errors
    /// `Undrained` for a name that is no person on a node, or a node that is no member (any
    /// other Mac's with the mesh off); `Handoff` where the hand-off cannot say which member
    /// `node` is ([`Bridge::member_for`]).
    fn person_route(
        &mut self,
        person: &str,
        node: &str,
    ) -> Result<(String, Option<String>), NoLane> {
        #[cfg(test)]
        if NO_WAY_BACK.with(std::cell::Cell::get) {
            return Err(NoLane::Undrained);
        }
        if !(crate::person::is_person_id(person)
            && node.starts_with("n-")
            && subject::is_principal(node))
        {
            return Err(NoLane::Undrained);
        }
        let member = self
            .member_for(node)
            .map_err(NoLane::Handoff)?
            .ok_or(NoLane::Undrained)?;
        let fleet = self.cfg.fleet.clone();
        if member == self.node {
            return Ok((subject::person_lane(&fleet, person, &self.node), None));
        }
        let lane = format!("/f/{fleet}/in/{member}/{person}/{}", self.node);
        Ok((lane, Some(member)))
    }

    /// [`Bridge::person_route`] for a receipt or a notice, within the lanes this bridge may open
    /// on the member's node ([`Bridge::may_answer_on`]).
    ///
    /// # Errors
    /// A broker that cannot be read.
    fn person_answer_lane(
        &mut self,
        person: &str,
        node: &str,
    ) -> io::Result<Result<String, NoLane>> {
        let (lane, member) = match self.person_route(person, node) {
            Ok(route) => route,
            Err(no) => return Ok(Err(no)),
        };
        let allowed = match member {
            Some(member) => self.may_answer_on(&member, &lane)?,
            None => true,
        };
        Ok(allowed.then_some(lane).ok_or(NoLane::Capped))
    }

    fn answer_fetch(&mut self, sid: &str, off: u64) {
        let lines = match self.fetch_record(sid, off).and_then(|(fields, text)| {
            fetched_lines(sid, off, &fields, &text).map_err(Unfetched::from)
        }) {
            Ok(lines) => lines,
            Err(why) => vec![format!("deliver {sid} fetched={off} {}", why.tail())],
        };
        for line in lines {
            match self.ctl_request(&line) {
                Ok(reply) if reply.ok() => {}
                Ok(reply) => {
                    stderr_line!(
                        "aterm-link: fetched {off} for {sid} refused: {}",
                        reply.header()
                    );
                    if !line.contains(" err=") {
                        let _ =
                            self.ctl_request(&format!("deliver {sid} fetched={off} err=refused"));
                    }
                    return;
                }
                Err(e) => {
                    stderr_line!("aterm-link: fetched {off} for {sid} failed: {e}");
                    return;
                }
            }
        }
    }

    fn fetch_record(&mut self, sid: &str, off: u64) -> Result<(String, String), Unfetched> {
        if self.attachment == Attachment::Observer {
            return Err("observer".into());
        }
        let filter = format!("/f/{}/in/{}/{sid}/>", self.cfg.fleet, self.node);
        // The session's mail is one subject per sender and kind, each with its own floor, so
        // the filter's oldest is the greatest of them and the record at `off` can lie below
        // it, held: a read refused there is read again told before it is answered `retained`
        // ([`Bridge::read_at`]). On a broker that does not tell fetches the refusal is answered
        // `retained` with the oldest it named, the record perhaps still held below it, as
        // before told fetches. A told read that could not be made is no answer about the
        // record: `unreadable`, the link kept, since a read-only fetch has no fate to keep
        // unknown.
        //
        // A row a bridge of a node this Mac held before delivered stays on that node's lanes
        // (§11 C3b): where this node's hold none at `off`, it is read there.
        let (node, (_, subj, raw)) = match self.read_at(off, &filter) {
            Ok(ReadAt::Rows(rows)) => match rows.into_iter().next().filter(|row| row.0 == off) {
                Some(row) => (self.node.clone(), row),
                None => match self.old_inbox_record(off, Some(sid)) {
                    Ok(Some((node, row, _))) => (node, row),
                    Ok(None) => return Err("no-record".into()),
                    Err(_) => return Err("unreadable".into()),
                },
            },
            Ok(ReadAt::Cut(floor) | ReadAt::Untold(floor)) => {
                match self.old_inbox_record(off, Some(sid)) {
                    Ok(Some((node, row, _))) => (node, row),
                    Ok(None) => return Err(Unfetched::Retained(floor)),
                    Err(_) => return Err("unreadable".into()),
                }
            }
            Ok(ReadAt::Unasked(_)) | Err(_) => return Err("unreadable".into()),
        };
        let addr = subject::parse_in(&self.cfg.fleet, &node, &subj).map_err(|_| "malformed")?;
        if addr.sid != sid {
            return Err("no-record".into());
        }
        if addr.src == self.node && self.is_forged_self(off) {
            return Err("forged-self".into());
        }
        let (body, _) = Body::decode(&raw);
        if !body.via.as_deref().is_none_or(via_ok) {
            return Err("via".into());
        }
        let addr = match self.carried_person(&addr, &body) {
            Ok(None) => addr,
            Ok(Some(src)) => subject::InAddr { src, ..addr },
            Err(why) => return Err(why.into()),
        };
        let (kind, demoted, _) = self.classify_delivery(&addr, &body);
        let trust = trust_of(&addr.src, body.via.is_some());
        let from = self.render_from(&addr, &body);
        let mut fields = format!("from={from} kind={kind} trust={trust}");
        if let Some(re) = body.re {
            fields.push_str(&format!(" re={re}"));
        }
        if let Some(dl) = body.dl {
            fields.push_str(&format!(" dl={dl}"));
        }
        if let Some(d) = demoted {
            fields.push_str(&format!(" demoted={d}"));
        }
        if let Some(via) = &body.via {
            fields.push_str(&format!(" via={via}"));
        }
        if kind == WakeClass::Ack {
            if let Some(v) = &body.verdict {
                fields.push_str(&format!(" verdict={v}"));
            }
        }
        Ok((fields, body.text))
    }

    fn sender_lane(&mut self, addr: &subject::InAddr, body: &Body) -> Result<String, NoLane> {
        // A person on another Mac (§11 C3c, [`Bridge::carried_person`]) is answered on the lane
        // back to that Mac (§11 C3d).
        if let Some((person, node)) = addr.src.split_once('@') {
            return self
                .person_answer_lane(person, node)
                .unwrap_or(Err(NoLane::Undrained));
        }
        let fleet = self.cfg.fleet.clone();
        if addr.src.starts_with("s-") {
            return match self.resolve_to(&format!("@{}", addr.src), None) {
                Ok(Route::To(prefix)) => Ok(prefix),
                _ => Err(self
                    .unrouted
                    .take()
                    .map_or(NoLane::Undrained, NoLane::Handoff)),
            };
        }
        if addr.src.starts_with("n-") {
            let sid = body
                .from
                .as_deref()
                .filter(|s| s.starts_with("s-") && subject::is_principal(s))
                .ok_or(NoLane::Undrained)?;
            // With the mesh on (§11 C3) another Mac's notice goes to the member it is now.
            let src = if self.cfg.mesh.is_some() && addr.src != self.node {
                match self.member_for(&addr.src) {
                    Ok(Some(member)) => member,
                    Ok(None) => return Err(NoLane::Undrained),
                    Err(no) => return Err(NoLane::Handoff(no)),
                }
            } else {
                addr.src.clone()
            };
            let lane = format!("/f/{fleet}/in/{src}/{sid}/{}", self.node);
            return match self.may_answer_on(&src, &lane) {
                Ok(true) => Ok(lane),
                Ok(false) => Err(NoLane::Capped),
                Err(_) => Err(NoLane::Undrained),
            };
        }
        Ok(subject::person_lane(&fleet, &addr.src, &self.node))
    }

    fn may_answer_on(&mut self, node: &str, lane: &str) -> io::Result<bool> {
        if node == self.node || self.lanes_opened.contains(lane) {
            return Ok(true);
        }
        let fx = self.fx.clone();
        let Some(conn) = self.conn_now() else {
            return Err(io::Error::new(
                io::ErrorKind::NotConnected,
                "the broker is unreachable",
            ));
        };
        let mut held = false;
        let started = fx.now();
        let answer = conn.last_walk(&format!("{lane}/*"), "", 1, |_| {
            held = true;
            Ok(astream_broker::Walk::Stop)
        });
        self.observe(started, answer, Some("read"))?;
        if held {
            return Ok(true);
        }
        let peer = format!("/f/{}/in/{node}/", self.cfg.fleet);
        let opened = self
            .lanes_opened
            .range(peer.clone()..)
            .take_while(|l| l.starts_with(&peer))
            .count();
        if opened >= NEW_LANES_PER_NODE {
            return Ok(false);
        }
        self.lanes_opened.insert(lane.to_string());
        Ok(true)
    }

    fn publish_own_off(&mut self, subject: &str, body: &[u8]) -> io::Result<u64> {
        let pin = plain_pin(self.next_seq()?, subject, body);
        self.publish_journaled(&pin, subject, body)
            .map(|(off, _, _)| off)
    }

    fn publish_own_at(&mut self, seq: u64, subject: &str, body: &[u8]) -> io::Result<(u64, bool)> {
        let pin = plain_pin(seq, subject, body);
        self.publish_journaled(&pin, subject, body)
            .map(|(off, deduped, _)| (off, deduped))
    }

    fn on_own_lane(&self, subject: &str) -> bool {
        subject.starts_with(&format!("/f/{}/in/{}/", self.cfg.fleet, self.node))
    }

    fn publish_journaled(
        &mut self,
        pin: &PublishPin,
        subject: &str,
        body: &[u8],
    ) -> io::Result<(u64, bool, Option<PublishIdentity>)> {
        self.publish_journaled_answering(pin, subject, body, None)
    }

    fn publish_journaled_answering(
        &mut self,
        pin: &PublishPin,
        subject: &str,
        body: &[u8],
        answers: Option<u64>,
    ) -> io::Result<(u64, bool, Option<PublishIdentity>)> {
        let published = self.publish_journaled_entry(pin, subject, body, answers);
        let (off, deduped, actual, entry) = published?;
        if let Some(ours) = entry {
            self.settle_landed(pin.seq, off, ours);
        }
        Ok((off, deduped, actual))
    }

    /// [`Bridge::publish_journaled_answering`], the in-flight entry it journaled left standing
    /// once the record landed, for the caller to end (`Some(ours)`, by whether the record is
    /// this node's own): a post's retry ends it only once aterm answers its `outbox sent`
    /// ([`Bridge::land_group`]).
    fn publish_journaled_entry(
        &mut self,
        pin: &PublishPin,
        subject: &str,
        body: &[u8],
        answers: Option<u64>,
    ) -> io::Result<(u64, bool, Option<PublishIdentity>, Option<bool>)> {
        let face = pin
            .identity
            .as_ref()
            .map_or(subject, |identity| identity.subject.as_str())
            .to_string();
        let journaled = self.on_own_lane(&face);
        if journaled && self.conn.is_none() {
            return Err(broker_unreachable());
        }
        // A reservation whose words no longer match its pin is refused before anything is
        // journaled or sent (`publish_reserved`'s first check, critic item F21's case 1): a
        // verdict no retry changes costs its held lane a local check at each.
        if answers.is_none() && pin.seq < astream_broker::ACK_SEQ_BASE && !reconstructs(pin, body) {
            return Err(self.unreconstructed(pin));
        }
        if journaled {
            self.state.set_in_flight(&InFlight {
                pin: pin.clone(),
                subject: face,
                body: body.to_vec(),
                log: self.conn.as_ref().and_then(|conn| conn.seen_log_identity()),
                at: Some(self.bus_high),
            })?;
        }
        let published = match answers {
            Some(upto) => self
                .publish_answer(pin.seq, subject, body, upto)
                .map(|(off, deduped)| (off, deduped, None)),
            None => self.publish_claimed(pin, subject, body),
        };
        match published {
            Ok((off, deduped, actual)) => {
                let ours = actual
                    .as_ref()
                    .map_or(journaled, |identity| self.on_own_lane(&identity.subject));
                if !journaled && ours {
                    let _ = self.remember_self_ack(off);
                }
                Ok((off, deduped, actual, journaled.then_some(ours)))
            }
            Err(e) => {
                if journaled && !self.fate_unknown(&e) {
                    self.drop_in_flight(pin.seq);
                }
                Err(e)
            }
        }
    }

    fn drop_in_flight(&self, seq: u64) {
        #[cfg(test)]
        self.state
            .keep_refused_drops(self.fx.mutant(crate::sim::world::Mutant::KeepARefusedDrop));
        if let Err(e) = self.state.drop_in_flight(seq) {
            stderr_line!(
                "aterm-link: the in-flight entry at sequence {seq} was not dropped durably: {e}"
            );
        }
    }

    fn publish_answer(
        &mut self,
        seq: u64,
        subject: &str,
        body: &[u8],
        upto: u64,
    ) -> io::Result<(u64, bool)> {
        debug_assert_eq!(seq, astream_broker::ACK_SEQ_BASE | upto);
        self.settle_unheld()?;
        let producer_id = self.producer_id;
        let group = subject::inbox_group(&self.cfg.fleet, &self.node);
        self.count_opening(subject);
        let fx = self.fx.clone();
        let Some(conn) = self.conn_now() else {
            return Err(io::Error::new(
                io::ErrorKind::NotConnected,
                "the broker is unreachable",
            ));
        };
        let started = fx.now();
        let answer = astream_broker::ack(conn, producer_id, &group, upto, subject, body);
        self.note_refused(&answer);
        let (off, deduped) = self.observe(started, answer, None)?;
        self.bus_high = self.bus_high.max(off.saturating_add(1));
        if !deduped {
            self.note_committed(upto);
        }
        Ok((off, deduped))
    }

    fn publish_claimed(
        &mut self,
        pin: &PublishPin,
        subject: &str,
        body: &[u8],
    ) -> io::Result<(u64, bool, Option<PublishIdentity>)> {
        if pin.seq >= astream_broker::ACK_SEQ_BASE {
            let (off, deduped) = self.publish_at(pin.seq, subject, body)?;
            return Ok((off, deduped, None));
        }
        self.publish_reserved(pin, subject, body)
    }

    /// Whether a failed publish's record may yet be on the bus, so its in-flight entry stands
    /// for the attach's replay: only where the link went with no answer read for it
    /// ([`Answered`], [`Unverifiable`]) and some connection may have carried it. One its send
    /// concluded before the link took it ([`Unsent`]), or that a rate-limited refusal held back
    /// ([`crate::rate_hold::RateHeld`]), never left the bridge, and its entry goes whatever
    /// became of the link.
    fn fate_unknown(&self, e: &io::Error) -> bool {
        self.conn.is_none()
            && !e.get_ref().is_some_and(|inner| {
                inner.is::<Answered>()
                    || inner.is::<Unverifiable>()
                    || inner.is::<Unsent>()
                    || inner.is::<crate::rate_hold::RateHeld>()
            })
    }

    /// An attach's republish of this node's in-flight journal, by sequence, before the attach
    /// drains a post. Within one boot it keeps each sender's order on the bus. 06d102f827
    /// argued it for one post in flight per sender (`settle_sender`, against a broker that
    /// does not gap-fence); for several, behind the broker's gap fence
    /// ([`Bridge::pipelines_posts`]):
    ///
    /// 1. A sender's posts take their sequences in its order: a fresh pin is the next
    ///    sequence, made as the drain reaches the post, and a drain reaches a sender's posts in
    ///    its order and none past one it holds; a retry keeps its pin. A keyed post may take an
    ///    older sequence, and it is sent alone, every owed answer read first (`conn_now`).
    /// 2. Of the publishes in flight on one connection, sent by sequence
    ///    ([`Bridge::settle_above`] reads the answers above a lower one first), the log holds a
    ///    prefix: a refused one fences every later one ([`Link::gap_fences`]); the fence is
    ///    lifted only once every answer is read and each refused post is concluded, its sender
    ///    held until a later drain retries it first ([`Bridge::lift_gap_fences`]); and no write
    ///    lands under a fence ahead of one sent before it (`settle_above` again). A connection
    ///    that closes takes its fences, and nothing more is sent on it.
    /// 3. An entry outlives its answer where its `acked` could not be written (it landed) and
    ///    where its answer was never read (a kill, a lost link: its fate is unknown); a refused
    ///    one is dropped, its post kept queued at its pin. With several of a sender's posts in
    ///    flight, the later ones' entries are dropped with an earlier one's refusal, ahead of it:
    ///    by 2 none of them lands, and their posts stay queued at their pins for the drain, which
    ///    sends them after the earlier one's retry, each read back first
    ///    ([`Bridge::drop_behind_a_refused_post`]; C12s1's review). A stop between their drops
    ///    and its own leaves the refused one's entry, which the replay publishes again before the
    ///    drain sends them.
    /// 4. A post to another lane is never journaled ([`Bridge::publish_journaled`]): no entry
    ///    stands for it, so a kill leaves nothing that says it went unanswered, and it stays
    ///    queued at its pin, for the drain the replay comes before. So a post with an entry is
    ///    not sent behind one of those of its sender's until that one is answered
    ///    ([`Bridge::settle_unjournaled_ahead`]): a refused one holds the sender, and no entry
    ///    stands behind one whose fate a kill left unknown.
    /// 5. An entry whose answer its read back cannot vouch for (critic item F21,
    ///    [`Bridge::unverified_publication`]) is dropped, its post or receipt kept queued at its
    ///    pin for the drain, which holds its lane behind it; it sets no fence, so the replay
    ///    itself holds that lane's later entries. A lane is the one of aterm's outbox that lists
    ///    an entry's post or receipt at its pin, read once, at the first such answer
    ///    ([`Bridge::listed_pins`]). A later entry aterm lists in a held lane that is read back on
    ///    the bus is settled where it is, held for the drain, which retires it whatever holds its
    ///    lane ([`Bridge::landed_behind_an_unverifiable_hold`]), and one read back as not landed
    ///    is dropped, its post or receipt still queued at its pin, for the drain to send after
    ///    the held one's retry. Every other lane's entries go on, and so does every entry aterm
    ///    does not list: its post or receipt left the outbox (aterm was told it landed, where
    ///    its entry outlived the `OK`), and its entry is all that remembers it, since no drain
    ///    sends what aterm no longer lists; its republish at its pin is the one copy a reader
    ///    is handed where retention gave the record up unread, and dropped it would be lost
    ///    (F21's review: the first cut held by the record's `from=`, whatever aterm listed,
    ///    `Mutant::HoldPastTheListing`). So does every entry where no whole listing could be read
    ///    (`more`, a refusal, a failure) and every entry no outbox lists (an `expired` verdict,
    ///    which names no sender): each is published at its pin, as before F21. NOT HELD EITHER:
    ///    an older build's entry, which names no head to read back from, and one journaled on
    ///    another log, which nothing here binds, are published again at their pins as before
    ///    (their dedup answers where they landed): there a first attempt that never landed lands
    ///    ahead of the held one's retry. NEGATIVE CONTROL
    ///    (`Mutant::ReplayPastAnUnverifiableEntry`): the later entry is published, and lands
    ///    ahead of the held post.
    ///
    /// So, by 2, 3, 4 and 5, each entry the replay publishes comes after every post of its
    /// sender's that landed and, but for one 5 does not hold (no head, another log), after none
    /// still queued at a pin, or else aterm was told it landed already; and the replay, by
    /// sequence, publishes them in its order (1). While it runs it HOLDS THE FENCES
    /// ([`Bridge::fences_held`]): it drops a refused entry and does not retry it, so every later
    /// one is refused behind it (`gap-fenced=`) and dropped too, and the drain sends those posts
    /// again at their pins, each sender's in its order.
    fn settle_in_flight(&mut self) -> bool {
        #[cfg(test)]
        let hold = !self
            .fx
            .mutant(crate::sim::world::Mutant::LiftFencesDuringReplay);
        #[cfg(not(test))]
        let hold = true;
        self.fences_held = hold;
        let settled = self.replay_in_flight();
        self.fences_held = false;
        self.release_replays_by_a_peek();
        settled
    }

    fn replay_in_flight(&mut self) -> bool {
        let entries = match self.state.in_flight() {
            Ok(entries) => entries,
            Err(e) => {
                stderr_line!(
                    "aterm-link: the in-flight journal could not be read; not attaching: {e}"
                );
                return false;
            }
        };
        for m in self.state.take_stale_retired() {
            stderr_line!(
                "aterm-link: {} was in flight in a post journal that a bridge of an older build \
                 ran past after this build's last unclean stop; retired to {} in the state dir \
                 and never published again — that bridge published what it found without it",
                m.from,
                m.to
            );
        }
        // THE LANES AN UNVERIFIABLE ENTRY HOLDS in this replay (critic item F21), and aterm's
        // listing they are read from, by the pin each listed post or receipt holds, taken at
        // the first such answer: point 5.
        let mut unverified: BTreeSet<OutboxLane> = BTreeSet::new();
        let mut listed: Option<BTreeMap<u64, OutboxLane>> = None;
        for (seq, entry) in entries {
            let (decoded, _) = Body::decode(&entry.body);
            // The control's lane: the record's sender, whatever aterm lists.
            #[cfg(test)]
            let sender = decoded
                .from
                .clone()
                .filter(|_| {
                    self.fx
                        .mutant(crate::sim::world::Mutant::HoldPastTheListing)
                })
                .map(|sid| (sid, crate::unverified_hold::Domain::Posts));
            if decoded.re.is_some() {
                let current_log = self.conn.as_ref().and_then(|conn| conn.seen_log_identity());
                match (entry.log, current_log) {
                    (Some(was), Some(now)) if was == now => {}
                    (Some(was), Some(now)) => {
                        stderr_line!(
                            "aterm-link: in-flight sequence {seq} refers to an offset on log \
                             {was}, not {now}; not replaying its old reference"
                        );
                        self.drop_in_flight(seq);
                        continue;
                    }
                    _ => {
                        stderr_line!(
                            "aterm-link: in-flight sequence {seq} refers to an offset without \
                             a matching journal and broker log identity; keeping it and not attaching"
                        );
                        return false;
                    }
                }
            }
            #[cfg(test)]
            let read_back = !self
                .fx
                .mutant(crate::sim::world::Mutant::ReplayWithoutReadingBack);
            #[cfg(not(test))]
            let read_back = true;
            // An older build's entry names no head, and one journaled on a log that keeps its id
            // and continues nothing, or whose lineage is unknown, is bound by nothing here: each
            // published again as before. One journaled on another log is read back from its start
            // (`Bridge::read_back_from`).
            if let Some(from) = self
                .read_back_from(entry.log, entry.at)
                .filter(|_| read_back)
            {
                match self.landed_since(&entry.subject, &entry.body, from, seq) {
                    Ok(Some(off)) => {
                        stderr_line!(
                            "aterm-link: the in-flight record at sequence {seq} is on the bus \
                             at @{off}, read back; it is not published again"
                        );
                        self.claim(off, seq);
                        let landed = publication_identity(&entry.subject, &entry.body);
                        if !self.land_key_winner(decoded.from.as_deref(), seq, &landed, off) {
                            return false;
                        }
                        let ours = self.on_own_lane(&entry.subject);
                        self.land_replayed(seq, off, ours);
                        continue;
                    }
                    Ok(None) => {}
                    Err(e) => {
                        stderr_line!(
                            "aterm-link: the in-flight record at sequence {seq} could not be \
                             read back: {e}"
                        );
                        return false;
                    }
                }
                #[cfg(test)]
                let past = self
                    .fx
                    .mutant(crate::sim::world::Mutant::ReplayPastAnUnverifiableEntry);
                #[cfg(not(test))]
                let past = false;
                let lane = listed.as_ref().and_then(|listed| listed.get(&seq)).cloned();
                #[cfg(test)]
                let lane = lane.or_else(|| sender.clone());
                if lane.is_some_and(|lane| unverified.contains(&lane)) && !past {
                    stderr_line!(
                        "aterm-link: the in-flight record at sequence {seq} is not on the bus \
                         and waits behind an unverifiable record of its lane's; aterm lists it at \
                         its pin, so it goes again from the drain, after that one's"
                    );
                    self.drop_in_flight(seq);
                    continue;
                }
            }
            // A KEYED RE-POST'S ENTRY, its own words not read back on the bus: its key's winner,
            // read back where its key pin says it landed, of that winner's words
            // ([`Bridge::keyed_winner_at`]), which binds it on whatever log the entry was
            // journaled. Past a cut that carried that record and dropped its dedup key, the
            // publish at the pin would land a second copy of the key; settled at the winner, the
            // entry lands as astream's dedup at its pin would answer it.
            if entry.pin.via_key {
                match self.keyed_winner_at(&entry.pin) {
                    Ok(Some(off)) => {
                        stderr_line!(
                            "aterm-link: the in-flight keyed record at sequence {seq} has its \
                             key's winner on the bus at @{off}, read back; it is not published \
                             again"
                        );
                        #[cfg(test)]
                        self.fx
                            .reservation_tap(crate::fx::ReservationTap::ReplayedAtTheWinner);
                        let ours = entry
                            .pin
                            .original
                            .as_ref()
                            .is_none_or(|winner| self.on_own_lane(&winner.subject));
                        self.land_replayed(seq, off, ours);
                        continue;
                    }
                    Ok(None) => {}
                    Err(e) => {
                        stderr_line!(
                            "aterm-link: the in-flight keyed record at sequence {seq} could not \
                             read its key's winner back: {e}"
                        );
                        return false;
                    }
                }
            }
            // BEHIND THE BROKER'S RATE LIMIT (critic item E06) the replay waits: astream refuses
            // for the rate ahead of its dedup, and the bridge's hold sends nothing, so neither
            // says whether an earlier attempt landed, and only the replay's own answer can. An
            // entry still held past the wait, or refused for a wait no longer than a hold (a
            // record bucket's, at most a second), is kept, the attach given up as for one whose
            // fate is unknown: dropped, it would leave a record an earlier attempt landed
            // delivered while its post stood queued, and the post sent at its pin again. A
            // refusal naming a longer wait (a byte bucket far behind, or a record past its whole
            // burst, refused at every try) is judged as every other refusal is.
            let fx = self.fx.clone();
            let claimed = crate::rate_hold::waiting_out(
                || self.publish_claimed(&entry.pin, &entry.subject, &entry.body),
                crate::rate_hold::REPLAY_WAIT_MAX,
                |wait| fx.sleep(wait),
            );
            let for_now = |e: &io::Error| {
                crate::rate_hold::wait_for(e)
                    .is_some_and(|wait| wait <= crate::rate_hold::PRODUCER_HOLD_MAX)
            };
            match claimed {
                Ok((off, _, actual)) => {
                    // A keyed pin may land as its key's winner, another identity.
                    if !entry.pin.via_key {
                        self.claim(off, seq);
                    }
                    let landed = actual
                        .clone()
                        .unwrap_or_else(|| publication_identity(&entry.subject, &entry.body));
                    if !self.land_key_winner(decoded.from.as_deref(), seq, &landed, off) {
                        return false;
                    }
                    let ours = actual
                        .as_ref()
                        .is_none_or(|identity| self.on_own_lane(&identity.subject));
                    self.land_replayed(seq, off, ours);
                }
                Err(e) if self.fate_unknown(&e) || for_now(&e) => {
                    stderr_line!(
                        "aterm-link: the in-flight record at sequence {seq} is unsettled: {e}"
                    );
                    return false;
                }
                Err(e) => {
                    stderr_line!(
                        "aterm-link: the in-flight record at sequence {seq} is not this node's \
                         to remember: {e}"
                    );
                    if unverifiable(&e) {
                        let listed = listed.get_or_insert_with(|| self.listed_pins_now());
                        let lane = listed.get(&seq).cloned();
                        #[cfg(test)]
                        let lane = lane.or(sender);
                        unverified.extend(lane);
                    }
                    self.drop_in_flight(seq);
                    if self.conn.is_none() {
                        return false;
                    }
                }
            }
        }
        true
    }

    /// WHERE A READ BACK OF A RECORD JOURNALED AT HEAD `at` ON LOG `log` STARTS, on the log the
    /// link reads: `at` on that very log; on one that continues it (a move whose lineage keeps a
    /// prefix, `LogIdentity::continues`), the lower of `at` and where it continues, since past
    /// that the offsets name records published since the move, a copy an earlier replay
    /// published among them, which the entry's head on its own log may lie above (aterm-link
    /// B05 review: a replay at a head above the new log's put the copy below it, and the next
    /// replay, missing it, published a second); 0 on ANOTHER LOG (another id, told with its
    /// lineage: `state::another_log`), since every record on it landed after the entry was
    /// journaled, so a copy found there is one an earlier attempt on this very log published,
    /// whose settle the disk then refused (ATERM-KI-1111-4: the sim's seed 104 of
    /// `Opts::lossy_logs` met a replay on another log whose `land_replayed` write failed, and
    /// the next attach, reading nothing back, published a second copy once a cut dropped the
    /// first one's key); `None` for no head, for a log whose lineage the link cannot read, and
    /// for one that keeps the id and continues nothing (a restored or shortened copy, a move
    /// older than the lineage's window), which may still hold records from before the entry: a
    /// read back from 0 there would settle the entry on an earlier identical record and never
    /// publish it, so it is published again, as before.
    fn read_back_from(
        &self,
        log: Option<astream_broker::LogIdentity>,
        at: Option<u64>,
    ) -> Option<u64> {
        let at = at?;
        let conn = self.conn.as_ref()?;
        let (was, now) = (log?, conn.seen_log_identity()?);
        if was == now {
            return Some(at);
        }
        let lineage = conn.log_lineage()?;
        if crate::state::another_log(Some(was), Some(now), Some(lineage)) {
            #[cfg(test)]
            if self
                .fx
                .mutant(crate::sim::world::Mutant::NoReadBackOnAnotherLog)
            {
                return None;
            }
            #[cfg(test)]
            if self.fx.mutant(crate::sim::world::Mutant::AnotherLogAsAMove) {
                return Some(at);
            }
            return Some(0);
        }
        let fork = now.continues(was, lineage)?;
        #[cfg(test)]
        if self
            .fx
            .mutant(crate::sim::world::Mutant::ReadBackFromTheJournaledHead)
        {
            return Some(at);
        }
        Some(at.min(fork))
    }

    /// `off` holds `seq`'s record: a copy the replay or a retry published there, or read back
    /// there and settled ([`Bridge::claims`]).
    fn claim(&mut self, off: u64, seq: u64) {
        let Some(log) = self.conn.as_ref().and_then(|conn| conn.seen_log_identity()) else {
            return;
        };
        let claims = match &mut self.claims {
            Some((on, claims)) if *on == log => claims,
            slot => &mut slot.insert((log, BTreeMap::new())).1,
        };
        claims.insert(off, seq);
        while claims.len() > CLAIMS_KEEP {
            claims.pop_first();
        }
    }

    /// The claims of the log the link reads ([`Bridge::claims`]).
    fn claims_now(&self) -> Option<&BTreeMap<u64, u64>> {
        let log = self.conn.as_ref()?.seen_log_identity()?;
        self.claims
            .as_ref()
            .filter(|(on, _)| *on == log)
            .map(|(_, claims)| claims)
    }

    /// WHERE A RECORD AN EARLIER ATTEMPT PUBLISHED LANDED, read back before it is published
    /// again: the offset of a record of `subject` with `body`'s publication identity (its
    /// body but its `t=`, which a retry encodes anew) at or above `from`, the records retention
    /// carried below the base included, and not one this bridge knows holds another sequence's
    /// record ([`Bridge::claims`]), or `None` for none. `from` is the head the bridge knew when
    /// it made the pin or journaled the entry of sequence `seq` (`PublishPin::at`,
    /// `InFlight::at`), brought down to where a moved log continues it
    /// ([`Bridge::read_back_from`]). Below it lies only what landed before the attempt: a
    /// sender's earlier post of the same words (answered before this one's pin was made,
    /// [`Bridge::settle_same_words`]) is never taken for this one there. Above it, that earlier
    /// post's copy can lie only where the attach's replay published it again (an entry the disk
    /// refused to clear, whose copy retention gave up): a claim of the replay's, skipped here. A
    /// copy of it a replay published in an earlier process, whose entry is since cleared, is not
    /// known: there, before a cut drops its key,
    /// the later post is published again at its own pin (the replay settles its own entries
    /// before it clears any, so that later post's own copy is on the bus by then, unless it
    /// was only reserved and never journaled in flight, and the process stopped in between).
    ///
    /// astream's dedup reaches only the records at or above the base its retention leaves: a
    /// cut drops the keys below it, a carried record's too. So a republish at the entry's
    /// pinned sequence would append a record a cut passed again; one read back here is
    /// settled where it is. ON THIS NODE'S OWN LANES a record whose entry stands is never
    /// delivered, so never read: one retention gave up went unread, by its age or its
    /// class's bytes, and the republish after a `None` is the one copy a reader is handed.
    /// Every record in flight is on an own lane (only an own-lane publish is journaled,
    /// [`Bridge::publish_journaled`]), so the attach's replay is held to that. A retry at a
    /// pin ([`Bridge::landed_at_its_pin`]) reads back a post to ANOTHER NODE'S LANE too, and
    /// there nothing held its first copy back from that node's reader: past a cut that gave up
    /// a copy that node had read (astream drops a pruned record's dedup key with it), the read
    /// finds none and the retry appends a second, which that node is handed again. Today only
    /// aterm-lab, the tests and the bench put a second node on one broker (the fabric is one
    /// machine's bus: `docs/DESIGN-session-log-2026-10-15.md`).
    ///
    /// A read that went on at the floor is read again from where it was refused, TOLD what
    /// retention gave up ([`transport::fetch_told_kept`], on a connection of its own:
    /// [`Bridge::told_reader`]): a subject's oldest can lie above a record of it retention
    /// still holds and the inbox's tail still delivers (astream's `retention::oldest`: where
    /// its class became whole after a rules change, and the floors folded past their bound;
    /// the sim's seed 31508 once met a newer record of it given up before an older one, by a
    /// carried cut planned at no stamp's time, which astream no longer does); a read on at the
    /// floor misses that record, and the republish would be a second copy (sim item B05).
    /// Against a broker that does not tell fetches the told read goes on at the floor too.
    fn landed_since(
        &mut self,
        subject: &str,
        body: &[u8],
        from: u64,
        seq: u64,
    ) -> io::Result<Option<u64>> {
        let want = publication_identity(subject, body);
        #[cfg(test)]
        let tells = !self
            .fx
            .mutant(crate::sim::world::Mutant::ReadBackWithoutTheHeld);
        #[cfg(not(test))]
        let tells = true;
        #[cfg(test)]
        let past_claims = self.read_back_past_claims;
        #[cfg(not(test))]
        let past_claims = false;
        let others: BTreeSet<u64> = self
            .claims_now()
            .filter(|_| !past_claims)
            .map(|claims| {
                claims
                    .range(from..)
                    .filter(|&(_, &by)| by != seq)
                    .map(|(&off, _)| off)
                    .collect()
            })
            .unwrap_or_default();
        let mut told: Option<(Conn, BusCloser)> = None;
        let mut cursor = from;
        loop {
            let started = self.fx.now();
            let answer = match told.as_mut() {
                Some((reader, _)) => transport::fetch_told_kept(reader, cursor, subject, 256),
                None => match self.conn_now() {
                    Some(conn) => transport::fetch_kept(conn, cursor, subject, 256),
                    None => return Err(broker_unreachable()),
                },
            };
            let ((rows, (next, head)), floor) = self.observe(started, answer, Some("read"))?;
            // Below the subject's oldest: read again from the same cursor, told.
            if floor.is_some() && told.is_none() && tells {
                told = Some(self.told_reader()?);
                continue;
            }
            if let Some((off, _, _)) = rows.iter().find(|(off, face, bytes)| {
                face == subject
                    && !others.contains(off)
                    && publication_identity(face, bytes) == want
            }) {
                return Ok(Some(*off));
            }
            if next >= head || next <= cursor {
                return Ok(None);
            }
            cursor = next;
        }
    }

    /// WHERE A RETRY AT A PIN AN EARLIER ATTEMPT TOOK LANDED, before it is published again: where
    /// this bridge knows its sequence's record is ([`Bridge::claims`]: the attach's replay
    /// settled its entry there, or an earlier retry published it), or else read back
    /// ([`Bridge::landed_since`]) from the head the pin was made at (`PublishPin::at`, which a
    /// move brings down to where the new log continues the old, and another log to its start:
    /// `StateDir::publication_offset_resets`), or, for a pin whose file holds no head (a
    /// receipt's), from that of its record in flight while it stands. `None` for none; for a
    /// pin that names no head and has no record in flight on this log, which is published again
    /// as before; and, with no read, for words the pin's attempt never published
    /// (`reconstructs`), which no read finds its record by, and which the publish refuses before
    /// it journals or sends anything (critic item F21's case 1). A keyed re-post's pin, whose
    /// winner may be another identity on another face, is read back by
    /// [`Bridge::keyed_repost_landed`], not here.
    fn landed_at_its_pin(
        &mut self,
        pin: &PublishPin,
        subject: &str,
        body: &[u8],
    ) -> io::Result<Option<u64>> {
        #[cfg(test)]
        if self
            .fx
            .mutant(crate::sim::world::Mutant::RetryWithoutReadingBack)
        {
            return Ok(None);
        }
        if pin.via_key {
            return Ok(None);
        }
        let known = self.claims_now().and_then(|claims| {
            claims
                .iter()
                .find(|&(_, &seq)| seq == pin.seq)
                .map(|(&off, _)| off)
        });
        if known.is_some() {
            return Ok(known);
        }
        if !reconstructs(pin, body) {
            return Ok(None);
        }
        let from = match pin.at {
            Some(at) => Some(at),
            None => self
                .state
                .in_flight()
                .ok()
                .and_then(|entries| {
                    entries
                        .into_iter()
                        .find_map(|(seq, entry)| (seq == pin.seq).then_some(entry))
                })
                .and_then(|entry| self.read_back_from(entry.log, entry.at)),
        };
        let Some(from) = from else {
            return Ok(None);
        };
        let face = pin
            .identity
            .as_ref()
            .map_or(subject, |identity| identity.subject.as_str())
            .to_string();
        self.landed_since(&face, body, from, pin.seq)
    }

    /// WHERE A KEYED RE-POST'S KEY'S WINNER IS ON THE BUS, read back before the re-post is
    /// published (the F23 sim's keyed world): the record the key pin says its winner landed at
    /// (`PublishPin::original_off`, of the identity `PublishPin::original`), where retention
    /// still holds it. A cut that carried that record below the base, unread, drops its dedup
    /// key all the same (astream drops every key below the base), so astream's dedup at the pin
    /// no longer answers with it, and the re-post, published, would land a second copy of its
    /// key beside the one its reader is still handed. Answers the winner's offset, which the
    /// drain retires the re-post at as astream's dedup would answer it (`dup=1`). `None` where
    /// the pin names no winner's offset, or the read finds none of the winner's record there
    /// (retention gave it up, its key with it, or a broker that does not tell fetches refused
    /// the read below the face's oldest): the publish at the pin reconciles it, as before.
    /// NEGATIVE CONTROL (`Mutant::KeyedRepostWithoutReadingBack`): every build before it
    /// published the re-post with no read back.
    ///
    /// # Errors
    /// The read's, as [`Bridge::read_at`] says them; a told read that could not be made, its
    /// own connection's.
    fn keyed_winner_at(&mut self, pin: &PublishPin) -> io::Result<Option<u64>> {
        #[cfg(test)]
        if self
            .fx
            .mutant(crate::sim::world::Mutant::KeyedRepostWithoutReadingBack)
        {
            return Ok(None);
        }
        let (Some(off), Some(winner)) = (pin.original_off, pin.original.as_ref()) else {
            return Ok(None);
        };
        Ok(match self.read_at(off, &winner.subject)? {
            ReadAt::Rows(rows) => rows
                .iter()
                .any(|(at, face, bytes)| *at == off && publication_identity(face, bytes) == *winner)
                .then_some(off),
            ReadAt::Cut(_) | ReadAt::Untold(_) => None,
            ReadAt::Unasked(e) => return Err(e),
        })
    }

    /// WHERE A KEYED RE-POST'S RETRY AT ITS PIN LANDED, before it is published again, and
    /// whether that record is its key's winner's (`true`) or its own attempt's (`false`). First
    /// its own copy, which the attach's replay read back of its entry's words and holds for this
    /// retry ([`Bridge::read_back_for_its_retry`]: an attempt that appended past a cut that
    /// dropped its key, an older build's included), retired there as its own landing, which
    /// moves the key to it and arms an ask's deadline; then, where its lane is not `held` by an
    /// unverifiable reservation, its key's winner ([`Bridge::keyed_winner_at`]), retired there
    /// as astream's dedup would answer it (`dup=1`). Read at the winner first, the re-post
    /// would be told `dup=1` of a record that is not where it landed, while its own copy is
    /// delivered, its key pin left at the winner and an ask on the bus never expiring (the F23
    /// review).
    ///
    /// # Errors
    /// [`Bridge::keyed_winner_at`]'s.
    fn keyed_repost_landed(
        &mut self,
        pin: &PublishPin,
        held: bool,
    ) -> io::Result<Option<(u64, bool)>> {
        if let Some(off) = self.read_back_for_its_retry(pin) {
            return Ok(Some((off, false)));
        }
        if held {
            return Ok(None);
        }
        Ok(self.keyed_winner_at(pin)?.map(|off| (off, true)))
    }

    fn arm_deadline_sweep(&mut self, replace: bool, retry_overdue: bool) {
        let now = self.fx.now();
        let earliest = self.deadlines.values().map(|d| d.at).min();
        if let Some(due) = deadline_wake(
            now,
            self.fx.wall_ms(),
            earliest,
            self.roster_due,
            retry_overdue,
        ) {
            self.deadline_due = if replace {
                due
            } else {
                self.deadline_due.min(due)
            };
        }
    }

    /// Note the deadline of the ask at `off`, write what the asks journal owes (the ask's
    /// recipient [`Bridge::note_asked`] noted with it, in the same append), and say whether the
    /// journal on disk holds the deadline. A post whose deadline it does not stays queued
    /// ([`Bridge::landing_line`]): a bridge that dies before it is written leaves the next one no
    /// deadline to sweep, and the ask never expires (item F4: the sim's world found a refused
    /// write of the table lose one across a kill). The retry lands at its pin and notes it
    /// again, which writes what a refused write left behind.
    ///
    /// NOTED ONCE (F4 review): a deadline the held post's first landing noted is not noted
    /// afresh by its retry, and one settled since is not armed again. While the post was held its
    /// ask may have been settled, its answer delivered or the sweep's `expired` published, which
    /// took the deadline out of the table; armed again at `now + dl`, it would expire an ask whose
    /// answer, or whose `expired`, the asker holds once retention gave that record up. The
    /// settle is the journal's ([`StateDir::deadline_settled`], `asks::Frame::DeadlineGone`):
    /// written, it holds for a retry by the next bridge too, after a kill (C04: a bridge before
    /// it held the post's ask in memory only).
    fn note_deadline(&mut self, sid: &str, off: u64, dl: u64, to: Option<String>) -> bool {
        #[cfg(test)]
        let settled = !self.fx.mutant(crate::sim::world::Mutant::ArmOverASettle)
            && self.state.deadline_settled(off, sid);
        #[cfg(not(test))]
        let settled = self.state.deadline_settled(off, sid);
        if !settled && !self.deadlines.contains_key(&off) {
            let was_empty = self.deadlines.is_empty();
            let deadline = Deadline {
                off,
                sid: sid.to_string(),
                at: self.fx.wall_ms().saturating_add(dl),
                dl,
                to,
            };
            self.deadlines.insert(off, deadline.clone());
            self.deadlines_unwritten.insert(off, sid.to_string());
            while self.deadlines.len() > DEADLINES_KEEP {
                if let Some((trimmed, d)) = self.deadlines.pop_first() {
                    self.deadlines_unwritten.insert(trimmed, d.sid);
                }
            }
            self.note_asks([asks::Frame::Deadline(deadline)]);
            self.arm_deadline_sweep(was_empty, false);
        }
        self.deadline_on_disk(off)
    }

    /// Whether the journal on disk holds what `deadlines` does at `off`, after a write of what
    /// it owes: that write took, or no change at `off` is unwritten.
    fn deadline_on_disk(&mut self, off: u64) -> bool {
        self.persist_asks() || !self.deadlines_unwritten.contains_key(&off)
    }

    /// Take the deadline at `off` out of the table, its settle owed to the asks journal and its
    /// removal unwritten until a write takes.
    fn drop_deadline(&mut self, off: u64) {
        if let Some(d) = self.deadlines.remove(&off) {
            self.note_asks([asks::Frame::DeadlineGone {
                off,
                sid: d.sid.clone(),
            }]);
            self.deadlines_unwritten.insert(off, d.sid);
        }
    }

    /// Note who the ask at `off` went to: owed to the asks journal, written by the next write
    /// with the ask's deadline where it has one ([`Bridge::landing_line`]).
    fn note_asked(&mut self, off: u64, to: &str) {
        self.asked.insert(off, to.to_string());
        while self.asked.len() > ASKED_KEEP {
            self.asked.pop_first();
        }
        self.note_asks([asks::Frame::Asked(Asked {
            off,
            to: to.to_string(),
        })]);
    }

    /// Owe `frames` to the asks journal ([`StateDir::note_asks`]). One whose record would not
    /// read back is noted nowhere, and said: the bridge's own tables never hold one, since
    /// every asker and recipient is a principal.
    fn note_asks(&self, frames: impl IntoIterator<Item = asks::Frame>) {
        if let Err(e) = self.state.note_asks(frames) {
            stderr_line!("aterm-link: the asks journal cannot hold this: {e}");
        }
    }

    /// Whether a reply from `src` to the ask at `off` comes from where the ask went: the node it
    /// went to, or the member a key rotation made of that node since, as the hand-off says
    /// (§11 C3b), this Mac's own node for an ask of its sessions' that the node service moved
    /// across its rotation; `None` where nothing says where the ask went. A person's post another
    /// Mac carried names its sender `<person>@<node>` ([`Bridge::carried_person`]), and comes
    /// from that node, the Mac whose lane brought it, as the deadline sweep reads it off the lane
    /// (§11 C3d): an ask to a person there is answered by them.
    fn reply_from(&mut self, off: u64, src: &str) -> Option<bool> {
        #[cfg(test)]
        let carried = !PERSON_REPLY_AS_SENDER.with(std::cell::Cell::get);
        #[cfg(not(test))]
        let carried = true;
        let src = match src.split_once('@') {
            Some((person, node)) if carried && crate::person::is_person_id(person) => node,
            _ => src,
        };
        let to = self
            .asked
            .get(&off)
            .cloned()
            .or_else(|| self.deadlines.get(&off).and_then(|d| d.to.clone()))?;
        Some(
            to == src
                || (!src.is_empty() && matches!(self.member_for(&to), Ok(Some(now)) if now == src)),
        )
    }

    fn read_where_asks_went(&mut self, off: u64) {
        let waiting = self.fx.now();
        while self.reply_from(off, "").is_none()
            && self
                .conn
                .as_ref()
                .is_some_and(|link| link.owed().any(Then::waiting_post))
            && self.settle_one(waiting)
        {}
    }

    /// An answer of `sid`'s ask at `off` settles its deadline; and whether the deadline table
    /// on disk agrees: no deadline of `sid`'s at `off` stands there, written now or before.
    /// Only that one waits for the disk (F4 review): a record that settles no deadline of
    /// `sid`'s, or one whose removal is written, is delivered whatever else a refused write left
    /// unwritten, as before item F4; one whose removal a refused write left unwritten (this
    /// one, tried before, or the sweep's after its `expired`) waits until a write takes.
    ///
    /// ONE STILL TO BE ARMED IS REMEMBERED (C04): the deadline can come after this record, where
    /// an earlier bridge's write of it was refused and that bridge died, and this one reads the
    /// answer ahead of the post's retry, which settles it then
    /// ([`Bridge::settle_a_retry_answered_ahead`]). It is
    /// held in memory and costs the answer no write, so a kill before the retry forgets it: the
    /// next bridge arms the deadline and its sweep reads the answer back from the bus.
    fn settle_deadline_for(&mut self, sid: &str, off: u64) -> bool {
        if self.deadlines.get(&off).is_some_and(|d| d.sid == sid) {
            self.drop_deadline(off);
        } else if !self.deadlines.contains_key(&off) {
            self.settled_unarmed.insert(off, sid.to_string());
            while self.settled_unarmed.len() > EXPIRED_KEEP {
                self.settled_unarmed.pop_first();
            }
        }
        let unwritten = !self.deadlines.contains_key(&off)
            && self
                .deadlines_unwritten
                .get(&off)
                .is_some_and(|asker| asker == sid);
        !unwritten || self.persist_asks()
    }

    /// Write what the asks journal owes ([`StateDir::write_asks`]: one append, or a rewrite
    /// after a refused write), and say whether the journal holds everything the bridge noted
    /// now; a refusal leaves [`Bridge::deadlines_unwritten`] for the next write to take.
    fn persist_asks(&mut self) -> bool {
        match self.state.write_asks() {
            Ok(()) => {
                self.deadlines_unwritten.clear();
                true
            }
            Err(e) => {
                stderr_line!("aterm-link: could not persist the asks journal: {e}");
                false
            }
        }
    }

    fn remember_expired(&mut self, off: u64, sid: &str) {
        self.expired.insert(off, sid.to_string());
        while self.expired.len() > EXPIRED_KEEP {
            self.expired.pop_first();
        }
    }

    fn expire_deadlines(&mut self, at_once: bool) {
        if self.conn.is_none() {
            return;
        }
        let now = self.fx.wall_ms();
        let judged_at = if at_once { u64::MAX } else { now };
        let due: Vec<Deadline> = self
            .deadlines
            .values()
            .filter(|d| d.at <= judged_at)
            .take(DEADLINE_SWEEP_MAX)
            .cloned()
            .collect();
        if due.is_empty() {
            return;
        }
        let mut changed = false;
        for d in due {
            // A session that is gone gets no verdict: nobody drains its lane.
            if !self.per_sid.epochs.contains_key(&d.sid) {
                self.drop_deadline(d.off);
                changed = true;
                continue;
            }
            match self.settled_on_bus(&d.sid, d.off) {
                Err(e) => {
                    stderr_line!("aterm-link: deadline sweep stopped at {}: {e}", d.off);
                    break;
                }
                Ok(Some(kind)) => {
                    if kind == "expired" {
                        self.remember_expired(d.off, &d.sid);
                    }
                    self.drop_deadline(d.off);
                    changed = true;
                }
                Ok(None) => {
                    let subject = format!(
                        "/f/{}/in/{}/{}/{}/expired",
                        self.cfg.fleet, self.node, d.sid, self.node
                    );
                    let encoded = expired_body(now, d.off, d.dl).encode();
                    match self.publish_own_off(&subject, &encoded) {
                        Ok(at) => {
                            self.remember_expired(d.off, &d.sid);
                            self.publish_ev_for(
                                Some(&d.sid),
                                &format!("expired re={} off={at} dl={}", d.off, d.dl),
                            );
                            self.drop_deadline(d.off);
                            changed = true;
                        }
                        Err(e) => {
                            stderr_line!("aterm-link: could not record expired for {}: {e}", d.off);
                            break;
                        }
                    }
                }
            }
        }
        if changed {
            self.persist_asks();
        }
    }

    fn settled_on_bus(&mut self, sid: &str, off: u64) -> io::Result<Option<&'static str>> {
        #[cfg(test)]
        if self
            .fx
            .mutant(crate::sim::world::Mutant::SweepIgnoresAnswers)
        {
            return Ok(None);
        }
        let filter = format!("/f/{}/in/{}/{sid}/>", self.cfg.fleet, self.node);
        let mut cursor = off.saturating_add(1);
        // Read below the lane's oldest told what retention gave up ([`Bridge::lane_page`]): an
        // answer it still holds there settles the deadline, never judged gone.
        let mut told = None;
        loop {
            #[cfg(test)]
            let page = if self
                .fx
                .mutant(crate::sim::world::Mutant::SweepWithAPlainFetch)
            {
                let fx = self.fx.clone();
                let Some(conn) = self.conn_now() else {
                    return Err(broker_unreachable());
                };
                let started = fx.now();
                let answer = conn
                    .fetch(cursor, &filter, 256)
                    .map(|page| (page, None, None, Vec::new()));
                self.observe(started, answer, Some("read"))?
            } else {
                self.lane_page(&mut told, cursor, &filter, 256)?
            };
            #[cfg(not(test))]
            let page = self.lane_page(&mut told, cursor, &filter, 256)?;
            let ((rows, (next, head)), gone, _, _) = page;
            if let Some(gone) = gone {
                stderr_line!(
                    "aterm-link: the deadline of @{off} on {sid}'s lane: the broker's retention \
                     gave up {} of it; judged on what it keeps",
                    transport::gap_lost(&gone)
                );
                // What the session handled is not news: the range it is told starts past its
                // `seen` watermark, where the answer it never handled would have been, and a
                // count over a range cut short counts what it no longer names.
                let from = self
                    .state
                    .seen_off(sid)
                    .map_or(gone.from, |seen| gone.from.max(seen.saturating_add(1)));
                let records = gone.records.filter(|_| from == gone.from);
                self.tell_given_up(
                    sid,
                    None,
                    astream_broker::Gap {
                        from,
                        to: gone.to,
                        records,
                    },
                );
            }
            for (_, subj, raw) in rows {
                let kind = subj.rsplit('/').next().unwrap_or("");
                let settles = ANSWER_KINDS
                    .iter()
                    .chain(std::iter::once(&"expired"))
                    .find(|k| **k == kind)
                    .copied();
                let src = subj.split('/').nth(6).unwrap_or("");
                #[cfg(test)]
                if kind == "expired"
                    && self
                        .fx
                        .mutant(crate::sim::world::Mutant::ExpireWithoutReadingBack)
                {
                    continue;
                }
                let from_right = if kind == "expired" {
                    self.is_my_node(src)
                } else {
                    self.reply_from(off, src) != Some(false)
                };
                if let Some(k) = settles.filter(|_| from_right) {
                    if Body::decode(&raw).0.re == Some(off) {
                        return Ok(Some(k));
                    }
                }
            }
            if next >= head || next <= cursor {
                return Ok(None);
            }
            cursor = next;
        }
    }

    fn settle_landed(&mut self, seq: u64, off: u64, ours: bool) {
        let remembered = !ours || self.remember_self_ack(off).is_ok();
        let cleared = remembered
            && match self.state.clear_in_flight(seq) {
                Ok(()) => true,
                Err(e) => {
                    stderr_line!(
                        "aterm-link: the in-flight entry at sequence {seq} (@{off}) could not be \
                         cleared: {e}; the record is delivered once it is"
                    );
                    false
                }
            };
        if cleared {
            self.unsettled.retain(|_, s| *s != seq);
            self.awaiting_drain.remove(&seq);
        } else {
            self.unsettled.insert(off, seq);
        }
    }

    /// The key whose winner the attach's replay landed at `off`, told it
    /// ([`StateDir::land_key_winner`]): `sender`'s, the post's `from=`. `false` where the disk
    /// refused it, and the attach is given up: the next one reads the record back and tells it
    /// again. NEGATIVE CONTROL (`Mutant::ReplayLeavesTheKeysWinnerUntold`).
    fn land_key_winner(
        &self,
        sender: Option<&str>,
        seq: u64,
        landed: &PublishIdentity,
        off: u64,
    ) -> bool {
        #[cfg(test)]
        if self
            .fx
            .mutant(crate::sim::world::Mutant::ReplayLeavesTheKeysWinnerUntold)
        {
            return true;
        }
        // A post's `from=` is its session (`post_body`); anything that cannot name a file in
        // `keys/` names no key.
        let Some(sid) = sender.filter(|sid| {
            !sid.is_empty() && !sid.starts_with('.') && !sid.contains(['/', '@', '\0'])
        }) else {
            return true;
        };
        match self.state.land_key_winner(sid, seq, landed, off) {
            Ok(_) => true,
            Err(e) => {
                stderr_line!(
                    "aterm-link: the key whose winner the in-flight record at sequence {seq} \
                     landed at @{off} could not be told it: {e}; not attaching"
                );
                false
            }
        }
    }

    fn land_replayed(&mut self, seq: u64, off: u64, ours: bool) {
        #[cfg(test)]
        let at_once = self.deliver_replay_at_once;
        #[cfg(not(test))]
        let at_once = false;
        if at_once || self.attachment == Attachment::Observer {
            self.settle_landed(seq, off, ours);
            return;
        }
        if ours {
            let _ = self.remember_self_ack(off);
        }
        self.unsettled.insert(off, seq);
        self.awaiting_drain.insert(seq, (off, ours));
    }

    /// THE ATTACH'S DRAIN OF THE POSTS ITS REPLAY LANDED ([`Bridge::awaiting_drain`]), ahead of
    /// the refill that would deliver their records: each is retired at its pin by a publish whose
    /// dedup answers it, and behind the broker's rate limit ([`Bridge::rate`], critic item E06)
    /// the drain sends none of a held sender's. So the attach waits out each hold and drains
    /// again, while the waits add up to at most [`crate::rate_hold::REPLAY_WAIT_MAX`]: an attach
    /// that went on would meet those records in its refill and drop the link for each
    /// ([`Bridge::settled_before_delivery`]), and every attach after it would spend the bucket on
    /// its presence rows ahead of the same drain.
    fn drain_awaited(&mut self) {
        let mut waited = Duration::ZERO;
        loop {
            self.drain_outbox();
            if self.awaiting_drain.is_empty() || self.conn.is_none() {
                return;
            }
            let now = self.fx.now();
            let Some(wait) = self.rate.next_end(now).map(|end| end - now) else {
                return;
            };
            if waited + wait > crate::rate_hold::REPLAY_WAIT_MAX {
                return;
            }
            self.fx.sleep(wait);
            waited += wait;
        }
    }

    fn release_replays_by_a_peek(&mut self) {
        if self.awaiting_drain.is_empty() {
            return;
        }
        match self.peek_outbox() {
            Ok(Some(drain)) => self.release_replays(&drain),
            Ok(None) => self.release_replays(&Drain::default()),
            Err(_) => {}
        }
    }

    /// aterm's outbox as a peek lists it (`outbox earlier-receipts=1`, or an older aterm's bare
    /// `outbox`): `None` where aterm refused the listing.
    fn peek_outbox(&mut self) -> io::Result<Option<Drain>> {
        const LEGACY_OUTBOX_USAGE: &str = "ERR usage: outbox [<max>] | outbox sent <sid> <post-id> off=<n|-> [dup=1] [reason=<word>]";
        let reply = match self.ctl_request("outbox earlier-receipts=1") {
            Ok(reply) if reply.header() == LEGACY_OUTBOX_USAGE => self.ctl_request("outbox"),
            answer => answer,
        }?;
        Ok(reply.ok().then(|| parse_drain(reply.body())))
    }

    /// THE PINS OF WHAT `drain` LISTS, each with the lane of its session's outbox it holds a
    /// place in ([`crate::unverified_hold::Domain`]): a post's own pin and its key's, and a
    /// receipt's.
    fn listed_pins(&self, drain: &Drain) -> io::Result<BTreeMap<u64, OutboxLane>> {
        let mut pinned = BTreeMap::new();
        for post in &drain.posts {
            let lane = (post.sid.clone(), crate::unverified_hold::Domain::Posts);
            if let Some(pin) = self.state.post_pin(&post.sid, post.id)? {
                pinned.insert(pin.seq, lane.clone());
            }
            if let Some(key) = post.key.as_deref() {
                if let Some(pin) = self.state.key_pin(&post.sid, key)? {
                    pinned.insert(pin.seq, lane);
                }
            }
        }
        for r in &drain.receipts {
            if let Some(pin) = self.state.receipt_pin(&r.sid, r.rid)? {
                pinned.insert(
                    pin.seq,
                    (r.sid.clone(), crate::unverified_hold::Domain::Receipts),
                );
            }
        }
        Ok(pinned)
    }

    /// [`Bridge::listed_pins`] of aterm's whole outbox now, for the attach's replay (its point
    /// 5): empty where no whole listing could be read (one cut short by `more`, a refusal, a
    /// failure, or a reservation the disk would not read), which lists nothing, so the replay
    /// holds no lane, as before critic item F21.
    fn listed_pins_now(&mut self) -> BTreeMap<u64, OutboxLane> {
        match self.peek_outbox() {
            Ok(Some(drain)) if !drain.more => self.listed_pins(&drain).unwrap_or_default(),
            _ => BTreeMap::new(),
        }
    }

    fn release_replays(&mut self, drain: &Drain) {
        if self.awaiting_drain.is_empty() || drain.more {
            return;
        }
        let Ok(pinned) = self.listed_pins(drain) else {
            return;
        };
        let free: Vec<(u64, u64, bool)> = self
            .awaiting_drain
            .iter()
            .filter(|(seq, _)| !pinned.contains_key(seq))
            .map(|(&seq, &(off, ours))| (seq, off, ours))
            .collect();
        for (seq, off, ours) in free {
            self.awaiting_drain.remove(&seq);
            self.settle_landed(seq, off, ours);
        }
    }

    fn settled_before_delivery(&mut self, off: u64) -> bool {
        #[cfg(test)]
        if self
            .fx
            .mutant(crate::sim::world::Mutant::DeliverPastAStandingEntry)
        {
            return true;
        }
        let Some(&seq) = self.unsettled.get(&off) else {
            return true;
        };
        if self.awaiting_drain.contains_key(&seq) {
            return false;
        }
        self.settle_landed(seq, off, true);
        !self.unsettled.contains_key(&off)
    }

    /// Whether the own record at `off` is held for the drain that retires its post or receipt
    /// ([`Bridge::awaiting_drain`]), not for a write the state dir refused.
    fn held_for_the_drain(&self, off: u64) -> bool {
        self.unsettled
            .get(&off)
            .is_some_and(|seq| self.awaiting_drain.contains_key(seq))
    }

    fn remember_self_ack(&mut self, off: u64) -> io::Result<()> {
        self.remember_self_acks([off])
    }

    /// Read the forged-self window back from the `acked` log ([`self_window`]).
    fn load_self_window(&mut self) {
        let acked = self.state.self_acked_log();
        self.acked_entries = acked.entries;
        self.acked_appendable = acked.appendable;
        self.acked_floored = acked.floor.is_some();
        (self.self_acked, self.self_floor) = self_window(acked);
    }

    /// FLOOR A WINDOW THAT WAITS FOR THE HEAD AT THE HEAD (critic item F20). Where the `acked`
    /// file was missing, could not be read, or held neither an offset nor a floor
    /// ([`self_window`]), the attach that first reads the log's `head` floors the window there
    /// and writes it so, as a move writes its new log's ([`StateDir::forget_log_offsets`]). In
    /// both the node knows none of its own records below the head, and every record an earlier
    /// run landed lies below it, the state lock keeping runs apart. So nothing below the head
    /// is judged, and every own-lane record above it is: one this run lands enters the window,
    /// and any other is accused. The head is the strongest bound the bridge holds: the log
    /// witness's `high` is saved at an attach and on each tick, not at each landing, and the
    /// mirror's pin trails the log, so an earlier run's own records can lie above either. An
    /// offset the window held below the head vouches for nothing and is dropped. A write the
    /// disk refuses leaves the floor in memory, and the next remember replaces the file whole
    /// ([`Bridge::remember_self_acks`]); a relaunch before any write lands floors its window at
    /// the head it reads, higher, which accuses less. NEGATIVE CONTROL
    /// (`Mutant::UnreadWindowAtZero`): the floor 0 every build before F20 loaded such a file
    /// at, which accuses every own record an earlier run landed.
    fn floor_at_the_head(&mut self, head: u64) {
        if self.self_floor.is_some() {
            return;
        }
        #[cfg(test)]
        let head = if self
            .fx
            .mutant(crate::sim::world::Mutant::UnreadWindowAtZero)
        {
            0
        } else {
            head
        };
        self.self_acked.retain(|off| *off >= head);
        self.self_floor = Some(head);
        match self
            .state
            .set_self_acked(&self.self_acked, head, SELF_ACK_KEEP)
        {
            Ok(()) => {
                self.acked_entries = self.self_acked.len();
                self.acked_appendable = true;
                self.acked_floored = true;
            }
            Err(e) => {
                self.acked_appendable = false;
                stderr_line!(
                    "aterm-link: could not record the self-lane window's floor @{head}: {e}"
                );
            }
        }
    }

    /// Remember `offs` as this node's own, in the self-lane window and, past it, in the acked
    /// log: the ones it does not hold yet in one append, or the whole window rewritten where
    /// the log is due its compaction, its last write failed, or it holds offsets and names no
    /// floor, as an older build's file does (critic item E07: an ack pass's landed posts are one
    /// write, where each was one). The first append into a file that names no floor names it
    /// ahead of them ([`self_window`]). An offset below the window's floor is not added: the
    /// window accuses nothing there, so it would vouch for nothing and take a slot. Past
    /// [`SELF_ACK_KEEP`] the window drops its oldest and its floor rises to the oldest it keeps,
    /// so the dropped offset, unknown now, is not accused. While the floor waits for the log's
    /// head ([`Bridge::self_floor`]) the window holds `offs` and the file takes nothing: a file
    /// of offsets with no floor reads as an older build's, and the floor's first write puts
    /// them there ([`Bridge::floor_at_the_head`]). Nothing lands then outside a test, since a
    /// landing needs the link, and the attach that makes it reads the head first.
    fn remember_self_acks(&mut self, offs: impl IntoIterator<Item = u64>) -> io::Result<()> {
        let mut fresh = Vec::new();
        for off in offs {
            if self.self_floor.is_some_and(|floor| off < floor) {
                continue;
            }
            if !self.self_acked.insert(off) && self.acked_appendable {
                continue;
            }
            fresh.push(off);
            while self.self_acked.len() > SELF_ACK_KEEP {
                self.self_acked.pop_first();
                if let Some(&oldest) = self.self_acked.first() {
                    self.self_floor = self.self_floor.map(|floor| floor.max(oldest));
                }
            }
        }
        let Some(floor) = self.self_floor.filter(|_| !fresh.is_empty()) else {
            return Ok(());
        };
        let compact = !self.acked_appendable
            || self.acked_entries >= 2 * SELF_ACK_KEEP
            || (!self.acked_floored && self.acked_entries > 0);
        let written = if compact {
            self.state
                .set_self_acked(&self.self_acked, floor, SELF_ACK_KEEP)
                .map(|()| self.self_acked.len())
        } else {
            let floor = (!self.acked_floored).then_some(floor);
            self.state
                .append_self_acked(floor, &fresh)
                .map(|()| self.acked_entries + fresh.len())
        };
        match written {
            Ok(entries) => {
                self.acked_entries = entries;
                self.acked_appendable = true;
                self.acked_floored = true;
                Ok(())
            }
            Err(e) => {
                self.acked_appendable = false;
                let offs: Vec<String> = fresh.iter().map(|off| format!("@{off}")).collect();
                stderr_line!(
                    "aterm-link: could not record {} as this node's own: {e}",
                    offs.join(" ")
                );
                Err(e)
            }
        }
    }

    fn commit_upto(&mut self, off: u64) {
        if self.attachment == Attachment::Observer {
            return;
        }
        let Some(off) = self.commit_target(off) else {
            return;
        };
        if self
            .conn
            .as_ref()
            .is_some_and(|link| link.owed().any(|then| then.commits_through(off)))
        {
            return;
        }
        let group = subject::inbox_group(&self.cfg.fleet, &self.node);
        let waiting = self.fx.now();
        while self
            .conn
            .as_ref()
            .is_some_and(|link| link.unacked() >= PIPELINE_WINDOW)
            && self.settle_one(waiting)
        {}
        let then = Then::Commit {
            upto: off,
            retry: self.commit_refused,
        };
        let started = self.fx.now();
        let Some(link) = self.conn.as_mut() else {
            return;
        };
        if let Err((e, then)) = link.send_commit(&group, off, then, started) {
            self.judge(started, Err(&e), Some("read"));
            self.conclude(then, Err(e));
        }
    }

    fn land_inbox_commit(&mut self) {
        for _ in 0..2 {
            self.flush_inbox_commit();
            let waiting = self.fx.now();
            while self
                .conn
                .as_ref()
                .is_some_and(|link| link.owed().any(Then::is_commit))
                && self.settle_one(waiting)
            {}
            if !self.commit_refused || self.conn.is_none() {
                return;
            }
        }
    }

    fn commit_target(&self, off: u64) -> Option<u64> {
        match self.inbox_commit_floor() {
            Some(floor) if off >= floor => {
                let below = floor.checked_sub(1)?;
                (!self.committed_through(below)).then_some(below)
            }
            _ => Some(off),
        }
    }

    /// Whether this run's inbox commits already reach `off`: `false` before any has landed,
    /// at offset 0 too.
    fn committed_through(&self, off: u64) -> bool {
        self.committed.is_some_and(|through| off <= through)
    }

    /// An inbox commit through `upto` landed.
    fn note_committed(&mut self, upto: u64) {
        self.committed = Some(self.committed.map_or(upto, |through| through.max(upto)));
    }

    fn inbox_commit_floor(&self) -> Option<u64> {
        self.per_sid
            .lane_owed
            .values()
            .filter_map(|off| *off)
            .chain(self.commit_floor)
            .chain(self.notices_held.keys().next().copied())
            .min()
    }

    fn on_fleet_record(&mut self, rec: &BrokerRecord) {
        self.unyielding(|bridge| bridge.take_fleet_record(rec));
    }

    fn take_fleet_record(&mut self, rec: &BrokerRecord) {
        let (off, subject, raw) = rec;
        if self.cfg.mesh.is_some() {
            // With the mesh on a person asks their own Mac's bridge (§11 C4); an old `halt` row
            // is read only by the move, from what this node held.
            if let Some(person) = subject::parse_halt_ask(&self.cfg.fleet, subject) {
                self.take_halt_ask(*off, person, raw);
            }
            return;
        }
        let Some(row) = subject::parse_halt(&self.cfg.fleet, subject, raw) else {
            return;
        };
        let human = row.human;
        if self.halts.get(human).is_some_and(|(have, ..)| have > off) {
            return;
        }
        let on = row.on;
        let reason = halt_reason_token(row.reason.as_deref());
        self.kept_halts.remove(human);
        self.halts.insert(human.to_string(), (*off, on, reason));
        self.defer_halt_ack(*off);
        let reached = match self.standing_reason() {
            Some(reason) => self.apply_halt(true, &reason),
            None => self.apply_halt(false, ""),
        };
        self.persist_halts();
        if reached {
            self.ack_halt(*off);
        }
    }

    fn defer_halt_ack(&mut self, off: u64) {
        if self.attachment != Attachment::Observer
            && self.state.halt_acked().is_none_or(|have| have < off)
        {
            self.halt_ack_pending = Some(self.halt_ack_pending.map_or(off, |have| have.max(off)));
        }
    }

    fn ack_halt(&mut self, off: u64) {
        if self.attachment == Attachment::Observer || self.ctl.lost() {
            return;
        }
        if self.state.halt_acked().is_some_and(|have| have >= off) {
            return;
        }
        if self.conn.as_ref().is_some_and(|link| {
            link.owed()
                .any(|then| matches!(then, Then::HaltAck { off: sent } if *sent >= off))
        }) {
            return;
        }
        let ack_subject = subject::node_face(&self.cfg.fleet, &self.node, "ack");
        let state = if self.halt_applied { "held" } else { "ready" };
        let ack = format!("v=1 t={} re={off} state={state}", self.fx.wall_ms());
        self.publish_pipelined(&ack_subject, ack.as_bytes(), Then::HaltAck { off });
    }

    fn apply_halt(&mut self, on: bool, reason: &str) -> bool {
        // THE STATE IS THE PAIR. A flag-only check would swallow a second
        // halter's different reason, or the same halter re-publishing a sharper
        // one, and the agent would keep reading the first words anybody wrote.
        if !self.halt_unconfirmed && on == self.halt_applied && (!on || reason == self.halt_reason)
        {
            return true;
        }
        self.write_holds(on, reason)
    }

    fn write_holds(&mut self, on: bool, reason: &str) -> bool {
        self.unyielding(|bridge| {
            let reached = bridge.write_holds_now(on, reason);
            bridge.halt_unconfirmed = !reached;
            reached
        })
    }

    fn write_holds_now(&mut self, on: bool, reason: &str) -> bool {
        // OBSERVER MODE holds nothing, and says so once rather than being
        // refused per session: `write_hold` always names `origin=fleet`, and the
        // FLEET hold is the inherited bridge connection's alone — `hold` is
        // owner-class, but `origin=fleet` from an Owner-token connection, which
        // is what an observer holds, is `ERR denied`.
        if self.attachment == Attachment::Observer {
            self.publish_ev(&format!("hold-skipped observer state={}", u8::from(on)));
            self.halt_applied = on;
            self.halt_reason = reason.to_string();
            return true;
        }
        let roster = self.refresh_sessions();
        let mut reached = roster.is_ok();
        let sids: Vec<String> = self.locals.values().cloned().collect();
        for sid in sids {
            if !self.write_hold(&sid, on, reason) {
                reached = false;
                if self.ctl.lost() {
                    return false;
                }
            }
        }
        if let Err(e) = roster {
            stderr_line!("aterm-link: the roster could not be read while holding: {e}");
        }
        if !reached {
            self.publish_ev(&format!("hold-unconfirmed state={}", u8::from(on)));
            return false;
        }
        self.halt_applied = on;
        self.halt_reason = reason.to_string();
        true
    }

    fn write_hold(&mut self, sid: &str, on: bool, reason: &str) -> bool {
        let line = if on {
            format!("hold {sid} on reason={reason} origin=fleet")
        } else {
            format!("hold {sid} off origin=fleet")
        };
        match self.ctl_request(&line) {
            Ok(reply) if reply.ok() || reply.header() == "ERR no such session" => true,
            Ok(reply) => {
                stderr_line!("aterm-link: {line} refused: {}", reply.header());
                false
            }
            Err(e) => {
                stderr_line!("aterm-link: {line} failed: {e}");
                false
            }
        }
    }

    fn converge_hold(&mut self, sid: &str, endpoint: EndpointHold) {
        if self.attachment == Attachment::Observer || self.conn.is_none() {
            return;
        }
        let reason = self.standing_reason();
        let on = reason.is_some();
        if endpoint.fleet() == on {
            return;
        }
        let reason = reason.unwrap_or_default();
        stderr_line!(
            "aterm-link: {sid}: {}",
            if on {
                "held: fleet halt stands"
            } else {
                "released: no fleet halt"
            }
        );
        if !self.write_hold(sid, on, &reason) {
            self.halt_unconfirmed = true;
        }
    }

    fn reassert_halt(&mut self) {
        let reason = self.standing_reason();
        let reached = match reason {
            Some(reason) => self.write_holds(true, &reason),
            None if self.halt_unconfirmed => self.write_holds(false, ""),
            None => true,
        };
        if reached {
            if let Some(off) = self.halt_ack_pending {
                self.ack_halt(off);
            }
        }
    }

    /// A person's request at `off` on their halt-ask subject ([`subject::halt_ask`]; only their
    /// ring writes it, and only a person's ring has one): said on this Mac's halt subject unless
    /// it holds here already, applied, and answered once the holds reached every session.
    fn take_halt_ask(&mut self, off: u64, person: &str, raw: &[u8]) {
        if self.attachment == Attachment::Observer {
            return;
        }
        let Some(ask) = crate::halt::Ask::parse(raw) else {
            stderr_line!(
                "aterm-link: {person}'s halt request at @{off} names no halt id or no \
                 `state=on|off`; it changes nothing"
            );
            return;
        };
        if !self.said_ask(person, &ask) {
            // Not said. Where the link failed the attach that follows reads the request again;
            // otherwise it waits for the back-off, the link up.
            if self.conn.is_some() {
                self.halt_unsaid.insert(off, (person.to_owned(), ask));
            }
            return;
        }
        self.defer_halt_ack(off);
        if self.hold_for_mesh_halts() {
            self.ack_halt(off);
        }
    }

    /// Say each request [`Bridge::take_halt_ask`] could not, oldest first, and answer the newest
    /// said once the holds reached every session. `false` when one still could not be said.
    fn say_unsaid_asks(&mut self) -> bool {
        let mut said = None;
        let unsaid = std::mem::take(&mut self.halt_unsaid);
        let mut rest = unsaid.into_iter();
        for (off, (person, ask)) in rest.by_ref() {
            if !self.said_ask(&person, &ask) {
                self.halt_unsaid.insert(off, (person, ask));
                break;
            }
            said = Some(off);
        }
        self.halt_unsaid.extend(rest);
        if let Some(off) = said {
            self.defer_halt_ack(off);
            if self.hold_for_mesh_halts() {
                self.ack_halt(off);
            }
        }
        self.halt_unsaid.is_empty()
    }

    /// Say `ask`, `person`'s, on this Mac's halt subject, unless what it asks holds here
    /// already (its halt stands or was lifted, or its id was lifted for every Mac), and apply it
    /// here. A halt-off for this Mac alone ([`crate::halt::Ask::here`]) is applied here and said
    /// to no other Mac, and needed unless the id was lifted here at all. `false` when it could
    /// not be said.
    fn said_ask(&mut self, person: &str, ask: &crate::halt::Ask) -> bool {
        let needed = match ask.word {
            crate::halt::Word::On(_) => !self.mesh_halts.knows(&ask.id),
            crate::halt::Word::Off if ask.here => !self.mesh_halts.is_lifted(&ask.id),
            crate::halt::Word::Off => !self.mesh_halts.lifted_everywhere(&ask.id),
            // A person's request never asks: `Ask::parse` reads no `state=ask`.
            crate::halt::Word::Ask => false,
        };
        if !needed {
            return true;
        }
        let said = crate::halt::Said {
            id: ask.id.clone(),
            by: person.to_owned(),
            word: ask.word.clone(),
            t_ms: self.fx.wall_ms(),
        };
        let me = self.node.clone();
        if ask.here {
            let outcome = self.mesh_halts.lift_here(&ask.id, person, said.t_ms);
            self.halt_outcome(&me, &said, &outcome, " scope=here");
            return true;
        }
        if let Err(e) = self.say_halt(&said) {
            stderr_line!(
                "aterm-link: {person}'s halt {} was not said on this Mac's halt subject: {e}; the \
                 next attach says it",
                said.id
            );
            return false;
        }
        self.halt_said_at.insert(said.id.clone(), said.t_ms);
        self.apply_said(&me, &said);
        true
    }

    /// Publish `said` on this Mac's halt subject ([`subject::halt_subject`]) under its node's
    /// producer, sealed with its node key, so every Mac's broker takes the relayed copy as this
    /// node's (§4.11). The key is read for this record alone and scrubbed.
    fn say_halt(&mut self, said: &crate::halt::Said) -> io::Result<u64> {
        let Some(keys) = self.cfg.mesh.as_ref().map(|mesh| mesh.keys.clone()) else {
            return Err(io::Error::other("the mesh is off"));
        };
        let subject = subject::halt_subject(&self.cfg.fleet, &self.node);
        let seq = self.next_seq()?;
        let sealed = crate::federate::seal_as_node(&keys, &subject, seq, said.render().as_bytes())
            .and_then(|(node, body)| {
                if node == self.node {
                    Ok(body)
                } else {
                    Err(io::Error::other(format!(
                        "the node key now names {node}, not this bridge's {}: it says nothing \
                         under another node's name until it restarts",
                        self.node
                    )))
                }
            });
        let body = match sealed {
            Ok(body) => body,
            Err(e) => {
                if self.unsent == Some(seq) {
                    self.unsent = None;
                }
                return Err(e);
            }
        };
        self.publish_at(seq, &subject, &body).map(|(off, _)| off)
    }

    /// A record on a Mac's halt subject, off the halt face: applied, and the holds follow.
    fn on_halt_record(&mut self, rec: &BrokerRecord) {
        self.unyielding(|bridge| {
            if bridge.take_halt_record(rec) {
                bridge.hold_for_mesh_halts();
            }
        });
    }

    /// Apply one record on a halt subject: the origin is the node its subject names (its broker
    /// took it only under that node's producer, from that node's bridge or relayed with that
    /// node's signature), what it said is the envelope's payload. Answers whether the halts that
    /// stand changed.
    fn take_halt_record(&mut self, (off, subject, body): &BrokerRecord) -> bool {
        let Some(origin) = subject::parse_halt_origin(&self.cfg.fleet, subject) else {
            return false;
        };
        let said = astream_broker::origin::payload(body)
            .ok_or(crate::halt::Refused::Malformed)
            .and_then(crate::halt::Said::parse);
        match said {
            Ok(said) => self.apply_said(origin, &said),
            Err(why) => {
                self.halt_refused(origin, *off, why);
                false
            }
        }
    }

    /// Apply what `origin` said, keep what changed, and say it; answer a halt said again, or
    /// asked for, after a lift for every Mac with the halt-off. Answers whether the halts that
    /// stand changed.
    fn apply_said(&mut self, origin: &str, said: &crate::halt::Said) -> bool {
        let now = self.fx.wall_ms();
        let me = self.node.clone();
        match self
            .mesh_halts
            .apply(&me, origin, said, &self.halt_gone, now)
        {
            crate::halt::Outcome::Refused(why) => {
                self.halt_refused(origin, 0, why);
                false
            }
            crate::halt::Outcome::Answer { by } => {
                self.answer_halt(&said.id, &by);
                false
            }
            outcome => self.halt_outcome(origin, said, &outcome, ""),
        }
    }

    /// Keep and say what applying `said`, `origin`'s, did (`outcome`): answers whether the halts
    /// that stand changed.
    fn halt_outcome(
        &mut self,
        origin: &str,
        said: &crate::halt::Said,
        outcome: &crate::halt::Outcome,
        scope: &str,
    ) -> bool {
        if outcome.changed() {
            let state = if *outcome == crate::halt::Outcome::Halted {
                "on"
            } else {
                "off"
            };
            self.publish_ev(&format!(
                "halt id={} origin={origin} by={} state={state}{scope}",
                said.id, said.by
            ));
        }
        if outcome.kept() {
            self.persist_mesh_halts();
        }
        outcome.changed()
    }

    /// Say the halt-off for `id` again, as `by` asked it, at most once every
    /// [`crate::halt::RESAY_MS`]: its origin said the halt again, or another Mac that holds it
    /// asked for it ([`crate::halt::Word::Ask`]), after this Mac lifted it for every Mac, so that
    /// Mac never heard the halt-off (§11 C4). Best effort: that Mac says it again within
    /// [`crate::halt::RESAY_MS`], and is answered again then.
    fn answer_halt(&mut self, id: &str, by: &str) {
        let now = self.fx.wall_ms();
        let answered = self.halt_answered.get(id).copied();
        if self.attachment == Attachment::Observer
            || answered.is_some_and(|at| now < at.saturating_add(crate::halt::RESAY_MS))
        {
            return;
        }
        let said = crate::halt::Said {
            id: id.to_owned(),
            by: by.to_owned(),
            word: crate::halt::Word::Off,
            t_ms: now,
        };
        match self.say_halt(&said) {
            Ok(_) => {
                self.halt_answered.insert(id.to_owned(), now);
                let known = &self.mesh_halts;
                self.halt_answered.retain(|id, _| known.knows(id));
                self.publish_ev(&format!("halt-answered id={id} by={by}"));
            }
            Err(e) => {
                stderr_line!(
                    "aterm-link: halt {id} was said again, or asked for, after {by} lifted it on \
                     every Mac; the halt-off could not be said back ({e}), so the Mac that said it \
                     learns it when it says so again"
                );
            }
        }
    }

    /// Say what is owed (each request unsaid, each record due again) unless an earlier try
    /// failed within its back-off: a failure that stays (the node key names another node, the
    /// broker refuses the subject) costs one try a [`HALT_RETRY_MAX_MS`], not one a pass.
    fn halt_owed_now(&mut self) {
        let now = self.fx.wall_ms();
        let (at, wait) = self.halt_retry;
        if now < at {
            return;
        }
        let said = self.say_unsaid_asks() && self.resay_due_halts(now);
        self.halt_retry = if said {
            (0, 0)
        } else {
            let wait = wait
                .saturating_mul(2)
                .clamp(HALT_RETRY_MIN_MS, HALT_RETRY_MAX_MS);
            (now.saturating_add(wait), wait)
        };
    }

    /// Say again each record of this node's that is due at `now_ms` ([`crate::halt::Halts::due`]):
    /// its halts, the halt-offs its persons asked or of its halts lifted for every Mac, and an
    /// ask for each other Mac's halt whose newest saying is [`crate::halt::STALE_MS`] old, so a
    /// Mac that was away while one went round, and found it gone from every log, hears it or asks
    /// for it.
    /// `false` when one could not be said; the rest wait for the next try.
    fn resay_due_halts(&mut self, now_ms: u64) -> bool {
        if self.cfg.mesh.is_none() || self.attachment == Attachment::Observer {
            return true;
        }
        let halts = &self.mesh_halts;
        self.halt_said_at.retain(|id, _| halts.knows(id));
        for said in self.mesh_halts.due(&self.node, &self.halt_said_at, now_ms) {
            if let Err(e) = self.say_halt(&said) {
                stderr_line!(
                    "aterm-link: halt {} could not be said again ({e}); the next pass says it",
                    said.id
                );
                return false;
            }
            self.halt_said_at.insert(said.id, now_ms);
        }
        true
    }

    /// Say a refused halt record once per reason in a row.
    fn halt_refused(&mut self, origin: &str, off: u64, why: crate::halt::Refused) {
        if self.halt_refusal_told == Some(why.token()) {
            return;
        }
        self.halt_refusal_told = Some(why.token());
        let at = if off == 0 {
            String::new()
        } else {
            format!(" at @{off}")
        };
        stderr_line!(
            "aterm-link: a halt record from {origin}{at} applies to nothing ({}): only a \
             person on a current member halts or lifts",
            why.token()
        );
    }

    /// Hold or release every session as the halts that stand say: `true` once that reached
    /// every session.
    fn hold_for_mesh_halts(&mut self) -> bool {
        match self.standing_reason() {
            Some(reason) => self.apply_halt(true, &reason),
            None => self.apply_halt(false, ""),
        }
    }

    fn persist_mesh_halts(&mut self) -> bool {
        match self.state.set_mesh_halts(&self.mesh_halts.render()) {
            Ok(()) => true,
            Err(e) => {
                stderr_line!(
                    "aterm-link: the halts this node applied could not be kept ({e}); they stand \
                     until it restarts"
                );
                false
            }
        }
    }

    /// Read the node service's hand-off for the nodes gone (§4.11): where the set changed, lift
    /// every halt such a node said. A missing or stale hand-off changes nothing: what was last
    /// read stands. Answers whether a halt lifted.
    fn refresh_halt_gone(&mut self) -> bool {
        let Some(root) = self.cfg.mesh.as_ref().map(|mesh| mesh.root.clone()) else {
            return false;
        };
        let me = self.node.clone();
        let Ok(handoff) = self.handoff.read(&root, &me) else {
            return false;
        };
        let gone: BTreeSet<String> = handoff.gone.difference(&handoff.members).cloned().collect();
        if gone == self.halt_gone {
            return false;
        }
        self.halt_gone = gone;
        let lifted = self
            .mesh_halts
            .lift_gone(&me, &self.halt_gone, self.fx.wall_ms());
        // Whether the log has this Mac out is kept too, so its own halts lift only once.
        self.persist_mesh_halts();
        if lifted.is_empty() {
            return false;
        }
        let ids = lifted.join(",");
        stderr_line!(
            "aterm-link: the membership log took out the origin of halt(s) {ids}: they lift here"
        );
        self.publish_ev(&format!("halt-lifted gone ids={ids}"));
        true
    }

    /// Every record of `filter` the log keeps, oldest first, and the offset a tail of it reads
    /// on from: what is held below a retention cut included, what it gave up not.
    fn read_kept(&mut self, filter: &str) -> Option<(Vec<BrokerRecord>, u64)> {
        let mut told = None;
        let (mut cursor, mut rows) = (0u64, Vec::new());
        loop {
            let ((page, (next, head)), _, _, _) =
                self.lane_page(&mut told, cursor, filter, 256).ok()?;
            rows.extend(page);
            if next >= head || next <= cursor {
                return Some((rows, next.max(cursor)));
            }
            cursor = next;
        }
    }

    /// The halts over the mesh at an attach (§11 C4), before the fleet and halt faces are
    /// followed: the hand-off's gone nodes, every halt record the log keeps, the move of the
    /// halts held under the old subject (once), each of this node's standing halts the log lost
    /// said again, then every person's request the log keeps, each said unless it holds here.
    /// The holds follow, and the newest request below `asks_mark` (where the fleet face reads on)
    /// is answered once they reached every session. Answers where the halt face reads on;
    /// `None` when a read failed.
    fn attach_mesh_halts(&mut self, asks_mark: u64) -> Option<u64> {
        self.unyielding(|bridge| bridge.attach_mesh_halts_now(asks_mark))
    }

    fn attach_mesh_halts_now(&mut self, asks_mark: u64) -> Option<u64> {
        self.halts.clear();
        self.kept_halts.clear();
        self.halt_ack_pending = None;
        // What this node said is counted from the records the log holds now: one it lost since
        // (a log replaced, a generation moved on) is due again at once.
        self.halt_said_at.clear();
        self.halt_unsaid.clear();
        self.halt_retry = (0, 0);
        self.refresh_halt_gone();
        let (records, halt_from) = self.read_kept(&subject::halt_filter(&self.cfg.fleet))?;
        for rec in &records {
            if subject::parse_halt_origin(&self.cfg.fleet, &rec.1) == Some(self.node.as_str()) {
                if let Some(said) = astream_broker::origin::payload(&rec.2)
                    .and_then(|payload| crate::halt::Said::parse(payload).ok())
                {
                    let at = self.halt_said_at.entry(said.id).or_default();
                    *at = (*at).max(said.t_ms);
                }
            }
            self.take_halt_record(rec);
        }
        if self.attachment != Attachment::Observer {
            self.move_old_halts()?;
            // What this log lost, or holds only from long ago, is said again now.
            if !self.resay_due_halts(self.fx.wall_ms()) && self.conn.is_none() {
                return None;
            }
        }
        let (asks, _) = self.read_kept(&subject::halt_asks(&self.cfg.fleet))?;
        let mut below_mark = None;
        for (off, subject, raw) in asks {
            let Some(person) = subject::parse_halt_ask(&self.cfg.fleet, &subject) else {
                continue;
            };
            let Some(ask) = crate::halt::Ask::parse(&raw) else {
                continue;
            };
            if self.attachment != Attachment::Observer && !self.said_ask(person, &ask) {
                // A request a dropped link could not say ends the attach; while the link stays
                // up, the back-off says it again, and the attach goes on.
                self.conn.as_ref()?;
                self.halt_unsaid.insert(off, (person.to_owned(), ask));
                continue;
            }
            if off < asks_mark {
                below_mark = below_mark.max(Some(off));
            }
        }
        if let Some(off) = below_mark {
            self.defer_halt_ack(off);
        }
        let reached = match self.standing_reason() {
            Some(reason) => self.write_holds(true, &reason),
            None => self.write_holds(false, ""),
        };
        self.persist_mesh_halts();
        if let Some(off) = below_mark.filter(|_| reached) {
            self.ack_halt(off);
        }
        Some(halt_from)
    }

    /// THE OLD ROWS MOVE (§11 C4), once: every halt in force under the old subject
    /// (`/f/<F>/fleet/<h>/halt`) stands on under the id [`crate::halt::carried_id`] derives, from
    /// this Mac, asked by its person. In force is what the log's newest row for each person
    /// says, in this fleet and each the old file names, so a halt withdrawn or set while this
    /// bridge was down counts as the log has it; where the log keeps no row, what this node held.
    /// Every one was set on this Mac, whose own log held the old rows under a ring its own mint
    /// secret minted. The attach then says each on this Mac's halt subject (it is not on the log
    /// yet), so every Mac holds it and a halt-off naming its id lifts it. The move is marked done
    /// and kept, the old file forgotten, and only then are the old rows retired, so a carried halt
    /// lifted here is never carried again and a withdrawn one never is. `None` when the log could
    /// not be read: the next attach moves them.
    fn move_old_halts(&mut self) -> Option<()> {
        if self.mesh_halts.moved() {
            return Some(());
        }
        let mut rows: BTreeMap<(String, String), Option<String>> = self
            .state
            .old_standing_halts()
            .into_iter()
            .map(|(fleet, human, halt)| ((fleet, human), Some(halt.reason)))
            .collect();
        let fleet = self.cfg.fleet.clone();
        let (logged, _) = self.read_fleet_halts()?;
        take_old_halt_rows(&mut rows, &fleet, &logged);
        // Only the rows the move reads under another fleet, and the retire, need the secret.
        let mut secret = (!rows.is_empty())
            .then(|| self.root_mint_secret())
            .flatten();
        let others: BTreeSet<String> = rows
            .keys()
            .map(|(fleet, _)| fleet.clone())
            .filter(|other| *other != fleet)
            .collect();
        for other in &others {
            let Some(key) = secret.as_deref() else {
                // The file stands for that fleet, as it would with the log down for good.
                break;
            };
            let Some(logged) = self.read_old_halt_rows(key, other) else {
                if let Some(secret) = &mut secret {
                    aterm_uds::secret::scrub(secret);
                }
                return None;
            };
            take_old_halt_rows(&mut rows, other, &logged);
        }
        let now = self.fx.wall_ms();
        let me = self.node.clone();
        let mut carried = Vec::new();
        if !crate::halt::no_carry() {
            for ((fleet, human), reason) in &rows {
                let Some(reason) = reason else { continue };
                let id = crate::halt::carried_id(&me, fleet, human);
                let reason = crate::halt::reason_token(Some(reason));
                if self.mesh_halts.carry(&id, &me, human, &reason, now) {
                    carried.push(id);
                }
            }
        }
        // What was carried and the move's mark are kept before any old row goes: a start cut
        // short before then moves again from the rows the log still holds, and one after reads
        // the old rows no more.
        self.mesh_halts.set_moved();
        let kept = self.persist_mesh_halts();
        if kept {
            if let Err(e) = self.state.clear_old_standing_halts() {
                stderr_line!(
                    "aterm-link: the halts held under the old subject were carried, and their \
                     old file could not be cleared ({e}); it is read no more"
                );
            }
        }
        if let Some(mut secret) = secret {
            if kept {
                self.retire_old_halt_rows(&secret, rows.keys());
            }
            aterm_uds::secret::scrub(&mut secret);
        }
        if !kept {
            return Some(());
        }
        if !carried.is_empty() {
            let ids = carried.join(",");
            stderr_line!(
                "aterm-link: {} halt(s) in force under the old subject stand on under id(s) \
                 {ids}; `aterm fabric halt off <id>` lifts one on every Mac",
                carried.len()
            );
            self.publish_ev(&format!("halt-moved ids={ids}"));
        }
        Some(())
    }

    /// The fabric root's mint secret, which minted every ring on this Mac's log, or `None`, said,
    /// where it cannot be read: the old halt rows then stay as the old file has them, and on the
    /// log, where nothing reads them with the mesh on.
    fn root_mint_secret(&self) -> Option<Vec<u8>> {
        let root = self.cfg.mesh.as_ref()?.root.clone();
        let path = root.join(crate::enable::MINT_SECRET);
        match transport::read_mint_secret(&path.to_string_lossy()) {
            Ok(secret) => Some(secret),
            Err(e) => {
                stderr_line!(
                    "aterm-link: the old halt rows are read from the old file alone and stay on \
                     the log ({}: {e}); nothing reads them with the mesh on",
                    path.display()
                );
                None
            }
        }
    }

    /// The newest old halt row of each person in `fleet` the log keeps, read under a grant to
    /// that face alone, minted for the read with `secret`. `None` when the log could not be read.
    fn read_old_halt_rows(&mut self, secret: &[u8], fleet: &str) -> Option<Vec<BrokerRecord>> {
        let face = format!("/f/{fleet}/fleet/*/halt");
        let read = astream_cap::mint(secret, &format!("ro:{face}"))
            .map_err(|e| io::Error::other(e.to_string()))
            .and_then(|cap| {
                let (mut conn, _closer) = self.fx.dial(&self.cfg.broker, ack_deadline())?;
                conn.attach_all([(cap.filter.as_str(), cap.tag.as_slice())])?;
                conn.last_all(&face)
            });
        match read {
            Ok((rows, _)) => Some(rows),
            Err(e) => {
                stderr_line!(
                    "aterm-link: the old halt rows of {fleet} could not be read ({e}); the next \
                     attach moves them"
                );
                None
            }
        }
    }

    /// Retire each old halt row (`/f/<F>/fleet/<h>/halt`) of `rows`, as its person may: under a
    /// grant bound to that person on that one subject, minted for the retire alone with the
    /// fabric root's mint secret. Best effort: a row left behind is read by nothing with the mesh
    /// on.
    fn retire_old_halt_rows<'a>(
        &mut self,
        secret: &[u8],
        rows: impl IntoIterator<Item = &'a (String, String)>,
    ) {
        for (fleet, human) in rows {
            let row = subject::person_halt(fleet, human);
            let retired = astream_cap::mint(secret, &format!("rw,p={human}:{row}"))
                .map_err(|e| io::Error::other(e.to_string()))
                .and_then(|cap| {
                    let (mut conn, _closer) = self.fx.dial(&self.cfg.broker, ack_deadline())?;
                    conn.attach_all([(cap.filter.as_str(), cap.tag.as_slice())])?;
                    conn.retire_subject(astream_cap::producer_id_of(human), &row)
                });
            if let Err(e) = retired {
                stderr_line!("aterm-link: the old halt row {row} stays on the log: {e}");
            }
        }
    }

    fn read_fleet_halts(&mut self) -> Option<(Vec<BrokerRecord>, (u64, u64))> {
        let filter = format!("/f/{}/fleet/*/halt", self.cfg.fleet);
        self.last_all(&filter).ok()
    }

    fn broadcast_floor(&mut self, filter: &str) -> Option<(u64, u64)> {
        let owed = self
            .per_sid
            .topics
            .values()
            .flat_map(|by_topic| by_topic.values().map(|c| c.next))
            .min();
        let fx = self.fx.clone();
        let conn = self.conn_now()?;
        let started = fx.now();
        let head = conn.fetch(0, filter, 0).map(|(_, (_, head))| head);
        let head = self.observe(started, head, Some("read")).ok()?;
        Some((owed.map_or(head, |owed| owed.min(head)), head))
    }

    fn fresh_broadcast_head(&mut self) -> Option<u64> {
        let filter = subject::say_filter(&self.cfg.fleet);
        let fx = self.fx.clone();
        let conn = self.conn_now()?;
        let started = fx.now();
        let answer = conn.fetch(0, &filter, 0).map(|(_, (_, head))| head);
        let head = self.observe(started, answer, Some("read")).ok()?;
        self.say_bus_head = self.say_bus_head.max(head);
        Some(head)
    }

    fn apply_fleet_halts(&mut self, rows: Vec<BrokerRecord>, mark: u64) {
        self.unyielding(|bridge| bridge.apply_fleet_halts_now(rows, mark));
    }

    fn apply_fleet_halts_now(&mut self, rows: Vec<BrokerRecord>, mark: u64) {
        let mut held: BTreeMap<String, String> = self
            .state
            .standing_halts(&self.cfg.fleet)
            .into_iter()
            .map(|(human, halt)| (human, halt.reason))
            .collect();
        held.extend(self.in_force());
        self.halts.clear();
        self.halt_ack_pending = None;
        let mut below_mark = None;
        for (off, subject, raw) in rows {
            let Some(row) = subject::parse_halt(&self.cfg.fleet, &subject, &raw) else {
                continue;
            };
            let reason = halt_reason_token(row.reason.as_deref());
            self.halts
                .insert(row.human.to_string(), (off, row.on, reason));
            if off < mark {
                below_mark = below_mark.max(Some(off));
            }
        }
        self.kept_halts = held
            .into_iter()
            .filter(|(human, _)| !self.halts.contains_key(human))
            .collect();
        if let Some(off) = below_mark {
            self.defer_halt_ack(off);
        }
        let reached = match self.standing_reason() {
            Some(reason) => self.write_holds(true, &reason),
            None => self.write_holds(false, ""),
        };
        if !self.kept_halts.is_empty() {
            let by: Vec<&str> = self.kept_halts.keys().map(String::as_str).collect();
            let by = by.join(",");
            stderr_line!(
                "aterm-link: the bus carries no halt row for {by}, whose halt this node held: \
                 it stands until each writes a `state=off` (or `on`) row on this log"
            );
            self.publish_ev(&format!("halt-kept by={by}"));
        }
        self.persist_halts();
        if let Some(off) = below_mark.filter(|_| reached) {
            self.ack_halt(off);
        }
    }

    fn in_force(&self) -> BTreeMap<String, String> {
        let mut out: BTreeMap<String, String> = self
            .halts
            .iter()
            .filter(|(_, (_, on, _))| *on)
            .map(|(human, (_, _, reason))| (human.clone(), reason.clone()))
            .collect();
        out.extend(
            self.kept_halts
                .iter()
                .map(|(human, reason)| (human.clone(), reason.clone())),
        );
        out
    }

    fn standing_reason(&self) -> Option<String> {
        if self.cfg.mesh.is_some() {
            return self.mesh_halts.reason().map(str::to_owned);
        }
        self.in_force().into_values().next()
    }

    fn persist_halts(&mut self) {
        let mut set: BTreeMap<String, StandingHalt> = self
            .halts
            .iter()
            .filter(|(_, (_, on, _))| *on)
            .map(|(human, (_, _, reason))| {
                let halt = StandingHalt {
                    reason: reason.clone(),
                    kept: false,
                };
                (human.clone(), halt)
            })
            .collect();
        for (human, reason) in &self.kept_halts {
            let halt = StandingHalt {
                reason: reason.clone(),
                kept: true,
            };
            set.insert(human.clone(), halt);
        }
        if self.state.standing_halts(&self.cfg.fleet) == set {
            return;
        }
        if let Err(e) = self.state.set_standing_halts(&self.cfg.fleet, &set) {
            stderr_line!("aterm-link: could not record the standing halts: {e}");
        }
    }

    fn judge_log(
        &self,
        head: u64,
        ident: Option<astream_broker::LogIdentity>,
        lineage: Option<&astream_broker::Lineage>,
    ) -> Option<Replaced> {
        let high = if self.log_replaced.is_some() {
            self.bus_high
        } else {
            self.log_witness.high.max(self.bus_high)
        };
        let seen = LogWitness {
            high,
            id: self.bus_log,
        };
        let continues = seen
            .continues(ident, lineage)
            .filter(|_| ident.is_some() && ident != seen.id);
        #[cfg(test)]
        let continues = continues.or_else(|| {
            (ident.is_some()
                && ident != seen.id
                && lineage.is_some()
                && self.fx.mutant(crate::sim::world::Mutant::AnotherLogAsAMove))
            .then(|| seen.high.min(head))
        });
        seen.replaced_by(head, ident).then_some(Replaced {
            head,
            was_high: seen.high,
            was_log: seen.id,
            log: ident,
            continues,
            deadlines: 0,
            inflight: 0,
        })
    }

    fn log_identity_allows_attach(
        &mut self,
        head: u64,
        ident: Option<astream_broker::LogIdentity>,
        found: &mut Option<Replaced>,
    ) -> bool {
        let saved = match self.state.rewound_log() {
            Ok(saved) => saved,
            Err(e) => {
                stderr_line!("aterm-link: could not read the rewound log fact: {e}; not attaching");
                return false;
            }
        };
        self.rewound_log = self.rewound_log.or(saved);
        if let Some(found) = *found {
            if let Some(log) = ident {
                if found.was_log == Some(log) && head < found.was_high {
                    self.rewound_log = Some(log);
                    stderr_line!(
                        "aterm-link: log {log} rewound from head {} to {head}; \
                         its reused offsets are uncertain, so it cannot attach",
                        found.was_high
                    );
                }
            }
        }
        if let Some(log) = self.rewound_log {
            // Retry a failed durable write before allowing any later attach.
            if saved != Some(log) {
                if let Err(e) = self.state.set_rewound_log(log) {
                    stderr_line!("aterm-link: could not preserve the rewound log refusal: {e}; not attaching");
                    return false;
                }
            }
            if ident.is_none() || ident == Some(log) {
                stderr_line!(
                    "aterm-link: log {log} previously rewound; a different known log \
                     identity is required before attaching"
                );
                return false;
            }
            if let Some(found) = found.as_mut() {
                found.continues = None;
            }
        }
        true
    }

    fn note_log(&mut self, head: u64, ident: Option<astream_broker::LogIdentity>, rebase: bool) {
        if rebase {
            self.bus_high = head;
            self.bus_log = ident;
        } else {
            self.bus_high = self.bus_high.max(head);
            self.bus_log = ident.or(self.bus_log);
        }
    }

    fn forget_log_offsets(
        &mut self,
        head: u64,
        log: Option<astream_broker::LogIdentity>,
        keep_below: Option<u64>,
        lineage: Option<&astream_broker::Lineage>,
    ) -> io::Result<(usize, usize)> {
        #[cfg(test)]
        let keep_below = keep_below.or_else(|| {
            self.fx
                .mutant(crate::sim::world::Mutant::KeepFloorOnAnotherLog)
                .then(|| self.log_witness.high.max(self.bus_high))
        });
        #[cfg(test)]
        self.state.skip_publication_resets(
            self.fx
                .mutant(crate::sim::world::Mutant::ForgetPinResetsOnAnotherLog),
        );
        #[cfg(test)]
        self.state.pin_head_gone_on_another_log(
            self.fx
                .mutant(crate::sim::world::Mutant::PinHeadGoneOnAnotherLog),
        );
        let keep_below = keep_below.map(|f| f.min(head));
        let kept = |off: u64| keep_below.is_some_and(|f| off < f);
        #[cfg(test)]
        let forget_pin = !self
            .fx
            .mutant(crate::sim::world::Mutant::KeepMirrorPinAcrossMove);
        #[cfg(not(test))]
        let forget_pin = true;
        // A drop the disk refuses refuses the attach before the witness moves to the new log
        // (`save_log_witness`), so the next attach meets the move again (`mirror_forget_log`);
        // first, so that attach forgets nothing twice.
        if forget_pin {
            self.mirror_forget_log()?;
        }
        let dropped =
            self.state
                .forget_log_offsets(head, self.bus_log, log, keep_below, lineage)?;
        for (seq, face) in &dropped {
            stderr_line!(
                "aterm-link: the in-flight record at sequence {seq} on {face} was decided on \
                 another log; it is not published on this one"
            );
        }
        self.unsettled.retain(|off, _| kept(*off));
        self.awaiting_drain.retain(|_, (off, _)| kept(*off));
        let before = self.deadlines.len();
        self.load_self_window();
        self.deadlines.retain(|off, _| kept(*off));
        self.deadlines_unwritten.retain(|off, _| kept(*off));
        self.asked.retain(|off, _| kept(*off));
        self.expired.retain(|off, _| kept(*off));
        self.settled_unarmed.retain(|off, _| kept(*off));
        let open = before - self.deadlines.len();
        for cursors in self.per_sid.topics.values_mut() {
            for cursor in cursors.values_mut() {
                cursor.next = keep_below.map_or(head, |f| cursor.next.min(f));
            }
        }
        for topics in self.per_sid.unhandled.values_mut() {
            for rows in topics.values_mut() {
                rows.retain(|(_, off)| kept(*off));
            }
            topics.retain(|_, rows| !rows.is_empty());
        }
        self.per_sid
            .unhandled
            .retain(|_, topics| !topics.is_empty());
        self.per_sid.topic_floors_dirty.clear();
        for off in self.per_sid.lane_owed.values_mut() {
            *off = off.filter(|o| kept(*o));
        }
        self.per_sid
            .given_up_told
            .retain(|_, floor| keep_below.is_some_and(|f| *floor <= f));
        self.commit_floor = self.commit_floor.filter(|o| kept(*o));
        self.notices_held.retain(|off, _| kept(*off));
        self.indexed_subjects.clear();
        // Another log: a reading of the last one's budget judges nothing here ([`BudgetScope`]).
        if self.budget_scope != BudgetScope::Out {
            self.budget_scope = BudgetScope::Unread;
        }
        self.seen_nodes.clear();
        self.committed = None;
        #[cfg(test)]
        let kept = |off: u64| {
            kept(off)
                || self
                    .fx
                    .mutant(crate::sim::world::Mutant::KeepOwedCommitAcrossMove)
        };
        if !self.inbox_pending.is_some_and(kept) {
            self.inbox_pending = None;
            self.inbox_pending_records = 0;
        }
        Ok((open, dropped.len()))
    }

    fn attaches_past_an_unwritten_witness(&self) -> bool {
        #[cfg(test)]
        {
            self.fx
                .mutant(crate::sim::world::Mutant::AttachPastAnUnwrittenWitness)
        }
        #[cfg(not(test))]
        {
            false
        }
    }

    fn save_log_witness(&mut self, rebase: bool) -> bool {
        let witness = if rebase {
            LogWitness {
                high: self.bus_high,
                id: self.bus_log,
            }
        } else {
            if self.log_replaced.is_some() {
                return false;
            }
            LogWitness {
                high: self.bus_high.max(self.log_witness.high),
                id: self.log_witness.id.or(self.bus_log),
            }
        };
        if witness == self.log_witness {
            return true;
        }
        match self.state.set_log_witness(&witness) {
            Ok(()) => {
                self.log_witness = witness;
                true
            }
            Err(e) => {
                stderr_line!("aterm-link: could not record the bus log's witness: {e}");
                false
            }
        }
    }

    fn sample_local_control(&mut self) {
        if self.attachment == Attachment::Observer {
            return;
        }
        let locals: Vec<(u64, String)> = self
            .locals
            .iter()
            .map(|(local, sid)| (*local, sid.clone()))
            .collect();
        if locals.is_empty() {
            return;
        }
        let mut batch = self.batch_status_samples();
        for (local, sid) in &locals {
            let sample = match &mut batch {
                StatusBatch::Rows(rows) => match rows.remove(local) {
                    Some(BatchSample::Ready(sample)) => Some(sample),
                    Some(BatchSample::Stale) => None,
                    None => self.status_sample(sid),
                },
                StatusBatch::Unavailable => None,
            };
            self.observe_local_control(sid, sample);
        }
        for (_, sid) in locals {
            // A SESSION STILL QUEUED FOR ADMISSION IS LEFT TO THE ADMISSION,
            // which runs right after this on the same round and publishes its
            // first row (read) itself: reading and publishing it here too
            // put two identical `live` records on the log for every session
            // that existed when the bridge attached.
            if self.per_sid.pending_admit.contains(&sid) {
                continue;
            }
            if !self
                .per_sid
                .presence
                .get(&sid)
                .is_some_and(|slot| slot.sampled)
            {
                self.sample_meta(&sid);
            }
            self.publish_presence_if_due(&sid);
        }
    }

    fn topic_change(&mut self, sid: &str, words: &mut std::str::SplitWhitespace<'_>) {
        let op = words.next();
        let Some(topic) = words.next().filter(|t| subject::is_topic(t)) else {
            return;
        };
        if !self.load_persisted_topics(sid) {
            return;
        }
        match op {
            Some("add") => {
                let since = words
                    .find_map(|w| w.strip_prefix("since="))
                    .unwrap_or("head");
                let held = self
                    .per_sid
                    .topics
                    .get(sid)
                    .and_then(|by_topic| by_topic.get(topic))
                    .map(|cursor| cursor.next);
                let next = match held
                    .or_else(|| since.strip_prefix('@').and_then(crate::parse_uint::<u64>))
                {
                    Some(next) => next,
                    None => match self.fresh_broadcast_head() {
                        Some(head) => head,
                        None => return,
                    },
                };
                self.per_sid
                    .topics
                    .entry(sid.to_string())
                    .or_default()
                    .entry(topic.to_string())
                    .or_insert(TopicCursor { next });
                self.persist_topics(sid);
                self.replay_topics(false);
            }
            Some("drop") => {
                self.per_sid
                    .given_up_told
                    .remove(&(sid.to_string(), topic.to_string()));
                let Some(by_topic) = self.per_sid.topics.get_mut(sid) else {
                    return;
                };
                by_topic.remove(topic);
                if by_topic.is_empty() {
                    self.per_sid.topics.remove(sid);
                    self.per_sid.unhandled.remove(sid);
                    self.state.forget_topics(sid);
                } else {
                    self.persist_topics(sid);
                    self.forget_unhandled_topic(sid, topic);
                }
            }
            _ => {}
        }
    }

    fn load_persisted_topics(&mut self, sid: &str) -> bool {
        if self.per_sid.topics.contains_key(sid) {
            return !self.per_sid.topic_floors_dirty.contains(sid)
                || self.persist_topic_floors(sid);
        }
        let mut persisted = self.state.topics(sid);
        let floors = match self.state.topic_floors(sid) {
            Ok(floors) => floors,
            Err(e) => {
                stderr_line!(
                    "aterm-link: the broadcast refill floors of {sid} could not be read: {e}"
                );
                return false;
            }
        };
        let mut rewound = false;
        for (topic, floor) in &floors {
            let cursor = persisted.entry(topic.clone()).or_insert_with(|| {
                rewound = true;
                TopicCursor { next: *floor }
            });
            if *floor < cursor.next {
                stderr_line!(
                    "aterm-link: {sid} had not handled every broadcast on {topic} below \
                     @{}: offering them again from @{floor}",
                    cursor.next
                );
                cursor.next = *floor;
                rewound = true;
            }
        }
        if rewound {
            if let Err(e) = self.state.set_topics(sid, &persisted) {
                stderr_line!("aterm-link: the rewound broadcast cursors of {sid} could not be persisted: {e}");
                return false;
            }
        }
        if !persisted.is_empty() {
            self.per_sid.topics.insert(sid.to_string(), persisted);
        }
        floors.is_empty() || self.persist_topic_floors(sid)
    }

    fn sample_topics(&mut self, sid: &str) -> bool {
        let Ok(reply) = self.ctl_request(&format!("@{sid} topic ls")) else {
            return false;
        };
        if !reply.ok() {
            return false;
        }
        let handled = reply
            .header()
            .split_whitespace()
            .find_map(|w| w.strip_prefix("seen="))
            .and_then(crate::parse_uint::<u64>);
        if !self.load_persisted_topics(sid) {
            return false;
        }
        let have = self.per_sid.topics.get(sid).cloned().unwrap_or_default();
        let mut parsed = Vec::new();
        let mut needs_head = false;
        for row in reply.rows() {
            let mut words = row.split_whitespace();
            if words.next() != Some("topic") {
                continue;
            }
            let Some(topic) = words.next().filter(|t| subject::is_topic(t)) else {
                continue;
            };
            let since = words
                .find_map(|w| w.strip_prefix("since="))
                .unwrap_or("head");
            let offset = since.strip_prefix('@').and_then(crate::parse_uint::<u64>);
            needs_head |= !have.contains_key(topic) && offset.is_none();
            parsed.push((topic.to_string(), offset));
        }
        let head = if needs_head {
            match self.fresh_broadcast_head() {
                Some(head) => head,
                None => return false,
            }
        } else {
            self.say_bus_head
        };
        let mut wanted: BTreeMap<String, TopicCursor> = BTreeMap::new();
        for (topic, offset) in parsed {
            let next = have
                .get(&topic)
                .map_or(offset.unwrap_or(head), |held| held.next);
            wanted.insert(topic, TopicCursor { next });
        }
        if wanted != have {
            self.per_sid
                .given_up_told
                .retain(|(s, scope), _| s != sid || scope.is_empty() || wanted.contains_key(scope));
            if wanted.is_empty() {
                self.per_sid.topics.remove(sid);
                self.per_sid.unhandled.remove(sid);
                self.state.forget_topics(sid);
            } else {
                self.per_sid.topics.insert(sid.to_string(), wanted.clone());
                self.persist_topics(sid);
                for gone in have.keys().filter(|t| !wanted.contains_key(*t)) {
                    self.forget_unhandled_topic(sid, gone);
                }
            }
        }
        if let Some(seen) = handled {
            self.note_handled(sid, seen);
        }
        true
    }

    fn catch_up_topic_page(&mut self, sid: &str) -> bool {
        let Some((topic, from)) = self.per_sid.topics.get(sid).and_then(|by_topic| {
            by_topic
                .iter()
                .filter(|(_, c)| c.next < self.say_drained_to)
                .min_by_key(|(_, c)| c.next)
                .map(|(t, c)| (t.clone(), c.next))
        }) else {
            return true;
        };
        let filter = subject::say_topic_filter(&self.cfg.fleet, &topic);
        let records = self.say_replay.page;
        // A topic is one subject per sender: under rules that hold its class, each has its own
        // floor and retention can hold broadcasts below the topic's oldest, read told
        // ([`Bridge::lane_page`]) rather than skipped and told as given up.
        #[cfg(test)]
        let page = if self
            .fx
            .mutant(crate::sim::world::Mutant::TopicReplayWithAPlainFetch)
        {
            let fx = self.fx.clone();
            let Some(conn) = self.conn_now() else {
                return false;
            };
            let started = fx.now();
            let answer = conn
                .fetch(from, &filter, records)
                .map(|page| (page, None, None, Vec::new()));
            self.observe(started, answer, Some("read"))
        } else {
            self.lane_page(&mut None, from, &filter, records)
        };
        #[cfg(not(test))]
        let page = self.lane_page(&mut None, from, &filter, records);
        let Ok(((rows, (next, _)), gone, uncounted, _)) = page else {
            return false;
        };
        // AN UNCOUNTED NOTICE AHEAD OF THE PAGE, CUT AT THE FIRST BROADCAST IT DELIVERS (item
        // F4): the told page delivers what retention holds of the topic from `from` in offset
        // order, so below its first record (or below where it read, with none) the broker holds
        // nothing of the topic, and what of it lay there went. That much is told, never the
        // range of a broadcast the page delivers (aterm marks no topic range its ring holds a
        // broadcast of), and no face tells it otherwise: the broadcast face's own notice only
        // hands its cursors here ([`Bridge::on_gap`]).
        #[cfg(test)]
        let uncounted = uncounted.filter(|_| {
            !self
                .fx
                .mutant(crate::sim::world::Mutant::TopicReplayDropsAnUncountedNotice)
        });
        let below_first = uncounted.and_then(|n| {
            let to = rows.first().map_or(next, |(off, _, _)| *off).min(n.to);
            (n.from < to).then_some(astream_broker::Gap {
                from: n.from,
                to,
                records: None,
            })
        });
        let gone = match (gone, below_first) {
            (Some(g), Some(b)) => Some(g.and(b)),
            (g, b) => g.or(b),
        };
        if let Some(gone) = gone {
            stderr_line!(
                "aterm-link: the broadcast topic {topic}: the broker's retention gave up {} of \
                 it; replaying past them",
                transport::gap_lost(&gone)
            );
            #[cfg(test)]
            let tells = !self
                .fx
                .mutant(crate::sim::world::Mutant::TopicReplayTellsNoGap);
            #[cfg(not(test))]
            let tells = true;
            if tells {
                self.tell_given_up(sid, Some(&topic), gone);
            }
        }
        for (off, subject, raw) in rows {
            // STRICTLY BELOW THE DRAIN POSITION. A record at or past it is
            // one the subscription will deliver (or already has), and
            // delivering it here too would spend a `deliver` the ring is
            // only going to dedup.
            if off >= self.say_drained_to {
                break;
            }
            let Some((say, body)) = say_record(&self.cfg.fleet, &subject, &raw) else {
                continue;
            };
            if say.topic != topic || off < from {
                continue;
            }
            if self.deliver_broadcast(sid, &say, &body, off, from) != Delivery::Accounted {
                return false;
            }
        }
        let reached = next.max(from.saturating_add(1)).min(self.say_drained_to);
        let now = self
            .per_sid
            .topics
            .get(sid)
            .and_then(|m| m.get(&topic))
            .map_or(from, |c| c.next);
        self.advance_topic(sid, &topic, now, reached);
        true
    }

    fn replay_topics(&mut self, roster: bool) {
        if self.conn.is_none() || self.attachment == Attachment::Observer {
            return;
        }
        self.say_replay.refill(self.fx.now());
        if self.say_replay.pages_left == 0 {
            return;
        }
        let mut owed: BTreeSet<String> = self
            .per_sid
            .topics
            .iter()
            .filter(|(sid, _)| !self.per_sid.lane_owed.contains_key(*sid))
            .filter(|(_, by_topic)| {
                by_topic
                    .values()
                    .any(|cursor| cursor.next < self.say_drained_to)
            })
            .map(|(sid, _)| sid.clone())
            .collect();
        let mut taken = 0;
        while !owed.is_empty() {
            // A pushed opt-in is an Event, which outranks the addressed Inbox
            // in the mailbox. Without this check it could spend all four pages
            // while fresh mail was already waiting. A message arriving during
            // a Fetch waits for that page only; the next check yields to it.
            if !may_take_replay_page(&self.mailbox, roster, taken) {
                break;
            }
            let Some(sid) = self.say_replay.take(&owed) else {
                break;
            };
            taken += 1;
            if !self.catch_up_topic_page(&sid) {
                break;
            }
            if !self.per_sid.topics.get(&sid).is_some_and(|by_topic| {
                by_topic
                    .values()
                    .any(|cursor| cursor.next < self.say_drained_to)
            }) {
                owed.remove(&sid);
            }
        }
    }

    fn observe_local_control(&mut self, sid: &str, sample: Option<StatusSample>) {
        let Some(sample) = sample else {
            return;
        };
        self.status_into_presence(sid, &sample);
        self.converge_hold(sid, sample.hold);
    }

    fn status_into_presence(&mut self, sid: &str, sample: &StatusSample) {
        #[cfg(test)]
        if self
            .fx
            .mutant(crate::sim::world::Mutant::ReadMetaPerSession)
        {
            let unread = StatusSample {
                meta: None,
                ..sample.clone()
            };
            unread.apply_presence(self.per_sid.presence.entry(sid.to_string()).or_default());
            return;
        }
        sample.apply_presence(self.per_sid.presence.entry(sid.to_string()).or_default());
    }

    /// THE ATTACH'S ONE STATUS READ (critic items D03 and E05): `sessions status` once, each row
    /// that reads Ready folded into its session's presence slot, its meaning fields
    /// (`role= attention= user_title=`) with it, so the attach's rows that follow read no
    /// `@<sid> status` for a session whose row named its generation and gave its verdict, and no
    /// `@<sid> meta` for one whose row carried its meaning fields
    /// ([`Bridge::publish_session_presence`]). A session whose row said `gen=-` or
    /// `agent_deferred=1`, or left its meaning fields out, or had no usable row (stale,
    /// malformed, or a batch aterm refused), is read alone by its row's publish, as before. An
    /// observer reads none: each of its rows reads its own status. Hold convergence is the
    /// roster round's ([`Bridge::sample_local_control`]), so nothing is written here.
    fn presence_from_status_batch(&mut self) {
        if self.attachment == Attachment::Observer {
            return;
        }
        let StatusBatch::Rows(rows) = self.batch_status_samples() else {
            return;
        };
        for (local, row) in rows {
            let (BatchSample::Ready(sample), Some(sid)) = (row, self.locals.get(&local).cloned())
            else {
                continue;
            };
            self.status_into_presence(&sid, &sample);
        }
    }

    fn publish_presence_if_due(&mut self, sid: &str) {
        if self.per_sid.pending_admit.contains(sid) {
            return;
        }
        let mode = self.cfg.presence;
        if self
            .per_sid
            .presence
            .get(sid)
            .is_some_and(|slot| slot.sampled && slot.due(mode, self.fx.now()).now())
        {
            self.publish_session_presence(sid, "live");
        }
    }

    fn retire_ghost_presence(&mut self) {
        #[cfg(test)]
        if self.fx.mutant(crate::sim::world::Mutant::NoGhostSweep) {
            return;
        }
        let filter = format!("/f/{}/pub/{}/*/presence", self.cfg.fleet, self.node);
        let rows = {
            if self.conn.is_none() {
                return;
            }
            match self.last_face(&filter) {
                Ok(rows) => rows,
                Err(e) => {
                    stderr_line!("aterm-link: could not sweep the presence roster: {e}");
                    return;
                }
            }
        };
        let hosted: BTreeSet<String> = self.locals.values().cloned().collect();
        let now = self.fx.now();
        let patience = if self.fault == Fault::SweepGhostsAtOnceWhileMarked
            && self.state.root().join("sweep-ghosts-now").exists()
        {
            Duration::ZERO
        } else {
            GHOST_AFTER
        };
        let mut unhosted: BTreeSet<String> = BTreeSet::new();
        let mut retire: Vec<String> = Vec::new();
        for (_, subject, raw) in &rows {
            let segs: Vec<&str> = subject.split('/').collect();
            if segs.len() != 7 || segs[3] != "pub" || segs[6] != "presence" {
                continue;
            }
            let sid = segs[5];
            if sid == "node" || hosted.contains(sid) {
                continue;
            }
            let (body, _) = Body::decode(raw);
            if body.unknown.get("state").map(String::as_str) != Some("live") {
                continue;
            }
            let marked = self.mark.is_some()
                && body.unknown.get(crate::state::PRESENCE_MARK) == self.mark.as_ref();
            #[cfg(test)]
            let marked = marked && !self.fx.mutant(crate::sim::world::Mutant::ReadNoMark);
            let witnessed_dead = self.witnessed_dead.as_ref().and_then(|(inc, mark)| {
                (body.unknown.get(crate::state::PRESENCE_MARK) == Some(mark)).then_some(*inc)
            });
            #[cfg(test)]
            let witnessed_dead = if self
                .fx
                .mutant(crate::sim::world::Mutant::WitnessByIncarnationAlone)
            {
                self.witnessed_dead.as_ref().map(|(inc, _)| *inc)
            } else {
                witnessed_dead
            };
            let ours = adoptable(presence_inc(&body), self.inc, witnessed_dead, marked);
            #[cfg(test)]
            let ours = ours
                || (self
                    .fx
                    .mutant(crate::sim::world::Mutant::RetireEveryEarlierIncarnation)
                    && presence_inc(&body).is_some_and(|inc| inc < self.inc));
            if !ours {
                continue;
            }
            unhosted.insert(sid.to_string());
            if self.exits_owed.contains(sid) {
                continue;
            }
            let since = *self.ghosts.entry(sid.to_string()).or_insert(now);
            if now.saturating_duration_since(since) >= patience {
                retire.push(sid.to_string());
            }
        }
        self.ghosts.retain(|sid, _| unhosted.contains(sid));
        let owed: Vec<String> = std::mem::take(&mut self.exits_owed)
            .into_iter()
            .filter(|sid| unhosted.contains(sid))
            .collect();
        // Before this sweep's own `exited` rows, which it did not read: they owe the next one.
        self.reclaim_subjects(&rows);
        self.batched(|bridge| {
            for sid in owed {
                bridge.publish_session_presence(&sid, "exited");
            }
            for sid in retire {
                bridge.publish_session_presence(&sid, "exited");
                bridge.publish_ev(&format!("presence-retired sid={sid} reason=unhosted"));
                bridge.ghosts.remove(&sid);
            }
        });
    }

    /// GIVE A DEPARTED SESSION'S SUBJECTS BACK to this node's subject budget, with
    /// astream's conditional retire (`Client::retire`). `presence` is this node's
    /// presence face as the sweep read it.
    ///
    /// A session has departed when this node does not host it and its presence row on
    /// its node reads `exited` or is gone. Of what this node is charged for, it held its
    /// presence row here and each lane this node wrote to it (`/f/<F>/in/<node>/<sid>/<this
    /// node>/<kind>`, on this node or another). A subject is given back once it has stood
    /// so, its newest record unchanged, for [`reclaim_after`], and no reservation or
    /// in-flight entry names it or (on this node) its session ([`StateDir::owed_publications`]):
    /// its lanes first, then its presence row, which goes only once every lane went in the
    /// same pass. Each retire names the newest record the sweep read, so a session revived
    /// since (a successor's `live` row, under an update's handoff or the keeper's recovery)
    /// makes it moot, and a record that lands later makes a new subject. A moot retire gives
    /// nothing back and holds the row back: the next sweep reads the subject again.
    ///
    /// A sweep looks when a session of this node left, at every attach, while it tracks a
    /// subject, and while this node holds a lane to another node's session
    /// ([`Bridge::reclaim_remote`]), whose leaving is no event here.
    ///
    /// A departed sid's pin goes once nothing of it is held here: no lane this node wrote
    /// to it and, on this node, no presence row. Every sweep looks at the pins of the nodes
    /// it read, and one a session's leaving or an attach owed reads every pinned node, so a
    /// pin a killed bridge left behind its retires goes at the next attach. No address
    /// routes to a sid its node does not advertise `live` ([`Bridge::resolve_to`]).
    fn reclaim_subjects(&mut self, presence: &[BrokerRecord]) {
        let owed = std::mem::take(&mut self.reclaim_owed);
        #[cfg(test)]
        if self.fx.mutant(crate::sim::world::Mutant::NoSubjectRetire) {
            return;
        }
        let log = self
            .conn
            .as_ref()
            .filter(|c| c.supports_retire())
            .and_then(|c| c.seen_log_identity());
        let Some(log) = log.filter(|log| self.reclaim_cut != Some(*log)) else {
            return;
        };
        if self.attachment == Attachment::Observer
            || self.reclaim_unread
            || (!owed && !self.reclaim_remote && self.reclaim.is_empty())
        {
            return;
        }
        let fleet = self.cfg.fleet.clone();
        let me = self.node.clone();
        // A refusal of these reads is an answer: the link stays up.
        let lanes = match self.lanes_this_node_writes() {
            Ok(lanes) => lanes,
            Err(e) if self.conn.is_some() && refused_for_good(&e) => {
                stderr_line!(
                    "aterm-link: this node's ring does not let it read the lanes it writes ({e}): \
                     no departed session's subject is given back until the bridge attaches again"
                );
                self.reclaim_unread = true;
                return;
            }
            Err(e) => {
                if self.conn.is_some() {
                    stderr_line!(
                        "aterm-link: the reclaim could not read the lanes this node writes ({e}); \
                         the next sweep reads them again"
                    );
                }
                self.reclaim_owed |= owed;
                return;
            }
        };
        let mut rows: BTreeMap<(String, String), (u64, bool)> = BTreeMap::new();
        let read = |rows: &mut BTreeMap<(String, String), (u64, bool)>, face: &[BrokerRecord]| {
            for (off, subject, raw) in face {
                let segs: Vec<&str> = subject.split('/').collect();
                if segs.len() != 7 || segs[3] != "pub" || segs[6] != "presence" || segs[5] == "node"
                {
                    continue;
                }
                let exited =
                    Body::decode(raw).0.unknown.get("state").map(String::as_str) == Some("exited");
                rows.insert((segs[4].to_string(), segs[5].to_string()), (*off, exited));
            }
        };
        read(&mut rows, presence);
        let pins = self.state.pins();
        let mut others: BTreeSet<String> = lanes
            .iter()
            .filter_map(|(_, lane, _)| lane_addr(&fleet, &me, lane))
            .map(|(node, _)| node)
            .filter(|node| *node != me)
            .collect();
        if owed {
            others.extend(
                pins.values()
                    .filter(|n| **n != me && n.starts_with("n-") && subject::is_principal(n))
                    .cloned(),
            );
        }
        // A node whose rows this ring cannot read keeps every lane and pin to it.
        let mut read_nodes: BTreeSet<String> = BTreeSet::from([me.clone()]);
        for node in others {
            match self.last_face_judged(&format!("/f/{fleet}/pub/{node}/*/presence"), None) {
                Ok(face) => {
                    read(&mut rows, &face);
                    read_nodes.insert(node);
                }
                Err(_) if self.conn.is_some() => {}
                Err(_) => {
                    self.reclaim_owed |= owed;
                    return;
                }
            }
        }
        let departed = |hosted: &BTreeSet<String>, node: &str, sid: &str| {
            (node != me || !hosted.contains(sid))
                && read_nodes.contains(node)
                && rows
                    .get(&(node.to_string(), sid.to_string()))
                    .is_none_or(|(_, exited)| *exited)
        };
        let mut hosted: BTreeSet<String> = self.locals.values().cloned().collect();
        // What a departed session holds that this node is charged for: `(node, sid) -> [(subject, newest)]`, its lanes first.
        let mut held: BTreeMap<(String, String), Vec<(String, u64)>> = BTreeMap::new();
        for (off, lane, _) in &lanes {
            let Some(addr) = lane_addr(&fleet, &me, lane) else {
                continue;
            };
            if departed(&hosted, &addr.0, &addr.1) {
                held.entry(addr).or_default().push((lane.clone(), *off));
            }
        }
        for ((node, sid), (off, _)) in &rows {
            if *node == me && departed(&hosted, node, sid) {
                let face = subject::session_face(&fleet, node, sid, "presence");
                held.entry((node.clone(), sid.clone()))
                    .or_default()
                    .push((face, *off));
            }
        }
        let now = self.fx.now();
        let before = std::mem::take(&mut self.reclaim);
        for (subject, off) in held.values().flatten() {
            // A lane's clock starts again with each record on it; a presence row's runs
            // while it stays departed, whatever `exited` row was written again.
            let since = before
                .get(subject)
                .filter(|(was, _)| was == off || subject.ends_with("/presence"))
                .map_or(now, |(_, since)| *since);
            self.reclaim.insert(subject.clone(), (*off, since));
        }
        let due = |reclaim: &BTreeMap<String, (u64, Instant)>, subject: &str| {
            reclaim
                .get(subject)
                .is_some_and(|(_, since)| now.saturating_duration_since(*since) >= reclaim_after())
        };
        held.retain(|_, subjects| subjects.iter().any(|(s, _)| due(&self.reclaim, s)));
        // The subjects this pass gave back.
        let mut retired: BTreeSet<String> = BTreeSet::new();
        if !held.is_empty() {
            if self.refresh_sessions().is_err() {
                return;
            }
            hosted = self.locals.values().cloned().collect();
            let owed = match self.state.owed_publications() {
                Ok(owed) => owed,
                Err(e) => {
                    stderr_line!("aterm-link: the reclaim could not read what the node owes: {e}");
                    return;
                }
            };
            'groups: for ((node, sid), subjects) in held {
                if node == me && (hosted.contains(&sid) || owed.sids.contains(&sid)) {
                    continue;
                }
                let mut whole = true;
                for (subject, off) in subjects {
                    let presence = subject.ends_with("/presence");
                    if (presence && !whole)
                        || owed.subjects.contains(&subject)
                        || !due(&self.reclaim, &subject)
                    {
                        whole = false;
                        continue;
                    }
                    match self.retire_held(&subject, off, log) {
                        Some(gave) => {
                            self.reclaim.remove(&subject);
                            self.indexed_subjects.remove(&subject);
                            if gave {
                                retired.insert(subject);
                            } else {
                                // Moot: a newer record (a revived session's row, a lane
                                // written since) or none. The next sweep reads it again.
                                whole = false;
                            }
                        }
                        None if self.conn.is_none() || self.reclaim_cut.is_some() => {
                            break 'groups;
                        }
                        None => {
                            // Refused: asked again only after another whole wait.
                            self.reclaim.insert(subject, (off, now));
                            whole = false;
                        }
                    }
                }
                if whole {
                    self.lanes_opened
                        .remove(&format!("/f/{fleet}/in/{node}/{sid}/{me}"));
                }
            }
            if !retired.is_empty() {
                stderr_line!(
                    "aterm-link: gave {} subjects of departed sessions back to this node's budget",
                    retired.len()
                );
            }
        }
        let lanes_left: BTreeSet<(String, String)> = lanes
            .iter()
            .filter(|(_, lane, _)| !retired.contains(lane))
            .filter_map(|(_, lane, _)| lane_addr(&fleet, &me, lane))
            .collect();
        self.reclaim_remote = lanes_left.iter().any(|(node, _)| *node != me);
        let row_left = |sid: &str| {
            rows.contains_key(&(me.clone(), sid.to_string()))
                && !retired.contains(&subject::session_face(&fleet, &me, sid, "presence"))
        };
        let unpinned: BTreeSet<String> = pins
            .iter()
            .filter(|(sid, node)| {
                departed(&hosted, node, sid)
                    && !lanes_left.contains(&((*node).clone(), (*sid).clone()))
                    && (**node != me || !row_left(sid))
            })
            .map(|(sid, _)| sid.clone())
            .collect();
        if let Err(e) = self.state.unpin(&unpinned) {
            stderr_line!(
                "aterm-link: could not drop the pins of {} departed sessions: {e}; a later \
                 sweep drops them",
                unpinned.len()
            );
        }
    }

    /// The newest record of every lane this node writes (`/f/<F>/in/*/*/<this node>/*`), a
    /// refusal taken for an answer ([`Bridge::last_face_judged`]).
    fn lanes_this_node_writes(&mut self) -> io::Result<Vec<BrokerRecord>> {
        #[cfg(test)]
        if let Some(code) = self.reclaim_read_refused.take() {
            return Err(astream_broker::BrokerRefusal {
                code,
                msg: "refused (test)".to_string(),
            }
            .into());
        }
        let filter = format!("/f/{}/in/*/*/{}/*", self.cfg.fleet, self.node);
        self.last_face_judged(&filter, None)
    }

    /// Retire `subject` if its newest record is still the one at `off` on `log`: `Some(true)`
    /// retired, `Some(false)` moot (a newer record, or none), `None` refused or not sent.
    /// A log retention cut without carrying the index refuses every retire (`cut: `): it is
    /// said once and nothing more is asked of that log ([`Bridge::reclaim_cut`]).
    fn retire_held(
        &mut self,
        subject: &str,
        off: u64,
        log: astream_broker::LogIdentity,
    ) -> Option<bool> {
        #[cfg(test)]
        if let Some((written, body)) = self
            .written_before_retire
            .take_if(|(s, _)| s.as_str() == subject)
        {
            self.publish(&written, &body).ok()?;
        }
        let producer = self.producer_id;
        let fx = self.fx.clone();
        let conn = self.conn_now()?;
        let started = fx.now();
        #[cfg(test)]
        let answer = if fx.mutant(crate::sim::world::Mutant::RetireUnconditionally) {
            conn.retire_subject(producer, subject)
        } else {
            conn.retire(producer, subject, off, log)
        };
        #[cfg(not(test))]
        let answer = conn.retire(producer, subject, off, log);
        match self.observe(started, answer, None) {
            Ok((_, moot)) => Some(!moot),
            Err(e)
                if astream_broker::broker_refusal(&e)
                    .is_some_and(astream_broker::BrokerRefusal::is_retire_cut) =>
            {
                stderr_line!(
                    "aterm-link: this log's retention cut keeps every subject charged: no \
                     departed session's subject is given back on it ({e})"
                );
                self.reclaim_cut = Some(log);
                None
            }
            Err(e) => {
                stderr_line!("aterm-link: could not give {subject} back: {e}");
                None
            }
        }
    }

    fn retire_on_request(&mut self, sid: &str) {
        if self.locals.values().any(|s| s == sid) {
            stderr_line!("aterm-link: refusing to retire @{sid}: this instance hosts it");
            return;
        }
        match crate::fabric::retire_refusal(sid, &crate::fabric::node_hosted()) {
            Some(crate::fabric::RetireRefusal::Hosted(pid)) => {
                stderr_line!(
                    "aterm-link: refusing to retire @{sid}: the instance at pid {pid} hosts it"
                );
                self.publish_ev(&format!(
                    "presence-retire-refused sid={sid} reason=hosted pid={pid}"
                ));
                return;
            }
            Some(crate::fabric::RetireRefusal::Unaskable(why)) => {
                stderr_line!(
                    "aterm-link: refusing to retire @{sid}: the node could not be asked: {why}"
                );
                self.publish_ev(&format!(
                    "presence-retire-refused sid={sid} reason=unaskable"
                ));
                return;
            }
            None => {}
        }
        let subject = subject::session_face(&self.cfg.fleet, &self.node, sid, "presence");
        let row = {
            if self.conn.is_none() {
                return;
            }
            match self.last_face(&subject) {
                Ok(rows) => rows.into_iter().find(|(_, s, _)| *s == subject),
                Err(e) => {
                    stderr_line!("aterm-link: could not read @{sid}'s presence row: {e}");
                    return;
                }
            }
        };
        let Some((_, _, raw)) = row else {
            stderr_line!("aterm-link: @{sid} has no presence row on this node; nothing to retire");
            return;
        };
        let body = String::from_utf8_lossy(&raw).into_owned();
        if Body::decode(&raw)
            .0
            .unknown
            .get("state")
            .map(String::as_str)
            != Some("live")
        {
            stderr_line!("aterm-link: @{sid}'s presence row is not live; nothing to retire");
            return;
        }
        let retired = crate::fabric::retired_body(&body);
        match self.publish(&subject, retired.as_bytes()) {
            Ok(_) => self.publish_ev(&format!("presence-retired sid={sid} reason=operator")),
            Err(e) => stderr_line!("aterm-link: could not retire @{sid}: {e}"),
        }
    }

    fn roster_backstop(&mut self) {
        if self.cfg.mesh.is_some() {
            let lifted = self.refresh_halt_gone();
            if self.conn.is_some() {
                if lifted {
                    self.hold_for_mesh_halts();
                }
                self.halt_owed_now();
            }
        }
        self.sample_local_control();
        let before: Vec<String> = self.locals.values().cloned().collect();
        let _ = self.refresh_sessions();
        self.admit_fresh_sessions();
        let after: Vec<String> = self.locals.values().cloned().collect();
        if before != after || self.halt_unconfirmed || self.halt_ack_pending.is_some() {
            self.reassert_halt();
        }
        if self.attachment == Attachment::Inherited {
            let unwatched: Vec<String> = self
                .locals
                .iter()
                .filter(|(local, sid)| self.per_sid.watched.get(*sid) != Some(*local))
                .map(|(_, sid)| sid.clone())
                .collect();
            for sid in &unwatched {
                self.per_sid.topics_sampled.remove(sid);
            }
        }
        self.sample_unsampled_topics();
        if self.fault != Fault::SkipRosterOutboxWhileMarked
            || !self.state.root().join("skip-roster-outbox").exists()
        {
            self.drain_outbox();
        }
        self.replay_topics(true);
        self.save_log_witness(false);
    }

    fn batch_status_samples(&mut self) -> StatusBatch {
        // Keep the fault's old meaning: while it is armed, EVERY status read
        // fails and no batch response may accidentally bypass the test seam.
        if self.fault == Fault::FailStatusWhileMarked
            && self.state.root().join("fail-status").exists()
        {
            return StatusBatch::Unavailable;
        }
        let Ok(reply) = self.ctl_request("sessions status") else {
            return StatusBatch::Unavailable;
        };
        let Reply::Lines { rows, .. } = reply else {
            return StatusBatch::Unavailable;
        };
        StatusBatch::Rows(parse_batch_rows(
            rows.iter().map(String::as_str),
            &self.locals,
            &self.per_sid.epochs,
        ))
    }

    fn status_sample(&mut self, sid: &str) -> Option<StatusSample> {
        if self.fault == Fault::FailStatusWhileMarked
            && self.state.root().join("fail-status").exists()
        {
            return None;
        }
        let reply = self.ctl_request(&format!("@{sid} status")).ok()?;
        if !reply.ok() {
            return None;
        }
        let mut sample = StatusSample::default();
        for tok in reply.header().split_whitespace() {
            if let Some(v) = tok.strip_prefix("agent=") {
                sample.agent = Some(v.to_string());
            } else if let Some(v) = tok.strip_prefix("hold=") {
                sample.hold = EndpointHold::of(v == "1", None);
            } else if let Some(v) = tok.strip_prefix("detail=") {
                sample.detail = v.to_string();
            } else if let Some(v) = tok.strip_prefix("gen=") {
                // The record's last `gen=`, the screen's generation (`-` while the terminal
                // was busy, or a word this bridge cannot read: none named).
                sample.screen = ScreenGen::parse(v);
            }
        }
        Some(sample)
    }

    fn meta_header(&mut self, sid: &str) -> Option<String> {
        let reply = self.ctl_request(&format!("@{sid} meta")).ok()?;
        reply.ok().then(|| reply.header().to_string())
    }

    fn sample_meta(&mut self, sid: &str) {
        let Some(header) = self.meta_header(sid) else {
            return;
        };
        let slot = self.per_sid.presence.entry(sid.to_string()).or_default();
        slot.fields.read_meta(&header);
        slot.sampled = true;
    }

    fn refresh_meta(&mut self, sid: &str) {
        if let Some(slot) = self.per_sid.presence.get_mut(sid) {
            slot.sampled = false;
        }
        if self.per_sid.pending_admit.contains(sid) || !self.per_sid.epochs.contains_key(sid) {
            return;
        }
        self.sample_meta(sid);
        self.publish_presence_if_due(sid);
    }

    fn staged_refusal(&self, sid: &str) -> Option<io::Error> {
        if self.fault != Fault::FailPostPublishWhileMarked {
            return None;
        }
        let marked = aterm_uds::atomic_file::read_regular_text(
            &self.state.root().join("fail-post-publish"),
            64 * 1024,
        )
        .ok()?;
        marked
            .split_whitespace()
            .any(|named| named == "*" || named == sid)
            .then(|| {
                astream_broker::BrokerRefusal {
                    code: astream_broker::proto::ERR_REFUSED,
                    msg: format!("ATERM_LINK_FAULT: the publish of {sid}'s record refused"),
                }
                .into()
            })
    }

    fn park_retire_while_marked(&mut self) {
        const POLL: Duration = Duration::from_millis(10);
        let marker = self.state.root().join("park-retire");
        if !marker.exists() {
            return;
        }
        self.fault = Fault::None;
        Fault::ParkRetireWhileMarked.fired(&self.state);
        while marker.exists() {
            self.fx.sleep(POLL);
        }
    }

    /// One drain of aterm's outbox, one budget scope ([`Bridge::budget_scoped`]): the node's
    /// subject budget is read at most once in it, at the first subject it opens that is not
    /// known indexed, and once more for each the count since says is short (critic item G04).
    fn drain_outbox(&mut self) {
        if let Some(peeked) = self.budget_scoped(Self::drain_whole) {
            let _ = self.mailbox.forget_queued_drains(peeked);
        }
    }

    fn drain_whole(&mut self) -> Option<EventMark> {
        // AN OBSERVER WAS REFUSED `outbox` at attach, and is refused it for
        // life: asking again every round only repeats the refusal on stderr.
        if self.conn.is_none() || self.attachment == Attachment::Observer {
            return None;
        }
        if self.fault == Fault::DropPostEventWhileMarked
            && self.state.root().join("drop-post-event").exists()
        {
            return None;
        }
        #[cfg(test)]
        let peek_past = self.fx.mutant(crate::sim::world::Mutant::PeekPastOwedPost);
        #[cfg(not(test))]
        let peek_past = false;
        let owed_at_peek = if self.pipelines_posts() {
            // SEVERAL POSTS OF A SENDER IN FLIGHT, behind the broker's gap fence: the peek
            // goes past the posts owed. The answers already in are read first, without
            // waiting, so a post whose ack came has its `outbox sent` and is not listed
            // again; a fence a refusal set is lifted, every owed answer read first
            // (`lift_gap_fences`), so each refused post is concluded before the held senders
            // clear and the listing names it again for its retry.
            self.try_settle_acks();
            self.lift_gap_fences();
            Some(if peek_past {
                BTreeSet::new()
            } else {
                self.posts_owed()
            })
        } else {
            let waiting = self.fx.now();
            while !peek_past
                && self
                    .conn
                    .as_ref()
                    .is_some_and(|link| link.owed().any(|then| then.post().is_some()))
                && self.settle_one(waiting)
            {}
            None
        };
        self.conn.as_ref()?;
        self.held_senders.clear();
        let peeked = self.mailbox.event_mark();
        const LEGACY_OUTBOX_USAGE: &str = "ERR usage: outbox [<max>] | outbox sent <sid> <post-id> off=<n|-> [dup=1] [reason=<word>]";
        let reply = match self.ctl_request("outbox earlier-receipts=1") {
            Ok(reply) if reply.header() == LEGACY_OUTBOX_USAGE => self.ctl_request("outbox"),
            answer => answer,
        };
        let (drained, roster) = match reply {
            Ok(reply) if reply.ok() => (reply.body().to_vec(), roster_token(reply.header())),
            Ok(reply) => {
                stderr_line!("aterm-link: outbox refused: {}", reply.header());
                return None;
            }
            Err(e) => {
                stderr_line!("aterm-link: outbox failed: {e}");
                return None;
            }
        };
        let drain = parse_drain(&drained);
        let posts = u64::try_from(drain.posts.len()).unwrap_or(u64::MAX);
        self.waits.count(crate::wait_tally::Row::DrainPosts, posts);
        self.release_replays(&drain);
        // A whole listing names every lane still queued: a lane it no longer names has nothing
        // left to hold ([`Bridge::unverified`]).
        if !drain.more {
            let listed: BTreeSet<(&str, crate::unverified_hold::Domain)> = drain
                .posts
                .iter()
                .map(|p| (p.sid.as_str(), crate::unverified_hold::Domain::Posts))
                .chain(
                    drain
                        .receipts
                        .iter()
                        .map(|r| (r.sid.as_str(), crate::unverified_hold::Domain::Receipts)),
                )
                .collect();
            self.unverified
                .retain_listed(|sid, domain| listed.contains(&(sid, domain)));
        }
        for (sid, off) in drain.fetches {
            if self.aterm_gone() {
                return None;
            }
            self.answer_fetch(&sid, off);
        }
        let whole = (!drain.more).then_some(peeked);
        if drain.receipts.is_empty() && drain.posts.is_empty() {
            return whole;
        }
        // A DOOR THAT COMMITS MUST LOOK (69979209f1): a session that left before this listing
        // must not be resolved as hosted. aterm read the count it names after the listing, and
        // every departure moves it, so the count this roster was read at is that look; any other
        // count, or none from an older aterm, reads the roster.
        if roster.is_none() || roster != self.roster_seen {
            if let Err(e) = self.refresh_sessions() {
                stderr_line!("aterm-link: the roster could not be re-read before a drain: {e}");
                return None;
            }
        }
        let mut stalled: BTreeSet<String> = BTreeSet::new();
        for receipt in drain.receipts {
            if self.aterm_gone() {
                return None;
            }
            // A session's receipts go in aterm's order, none past one that stalled or that an
            // unverifiable reservation holds ([`Bridge::unverified`]), but one whose record the
            // attach's replay read back, retired where it landed
            // ([`Bridge::landed_behind_an_unverifiable_hold`]).
            let held = stalled.contains(&receipt.sid)
                || self.unverified.holds(
                    &receipt.sid,
                    crate::unverified_hold::Domain::Receipts,
                    self.fx.now(),
                );
            if held {
                stalled.insert(receipt.sid.clone());
                if !self.landed_behind_an_unverifiable_hold(
                    &receipt.sid,
                    crate::unverified_hold::Domain::Receipts,
                    || self.state.receipt_pin(&receipt.sid, receipt.rid),
                ) {
                    continue;
                }
            }
            if let Err(e) = self.send_receipt(&receipt) {
                // An unverifiable answer was said where it held the lane, at its first and at
                // each power of two after ([`Bridge::hold_unverifiable`]).
                if !(unverifiable(&e) && self.conn.is_some()) {
                    stderr_line!(
                        "aterm-link: could not publish the receipt for {} ({}): {e}",
                        receipt.off,
                        receipt.sid
                    );
                }
                self.conn.as_ref()?;
                stalled.insert(receipt.sid.clone());
            }
        }
        let looked = std::mem::replace(&mut self.roster_looked, true);
        let finished =
            self.batched(|bridge| bridge.drain_posts(drain.posts, owed_at_peek.as_ref()));
        self.roster_looked = looked;
        (finished && stalled.is_empty() && self.held_senders.is_empty())
            .then_some(whole)
            .flatten()
    }

    /// Every post owed an answer now, by `(sid, post id)`.
    fn posts_owed(&self) -> BTreeSet<(String, u64)> {
        self.conn.as_ref().map_or_else(BTreeSet::new, |link| {
            link.owed()
                .filter_map(|then| then.post().map(|post| (post.sid.clone(), post.id)))
                .collect()
        })
    }

    /// Send `posts`, as one listing gave them. With `owed_at_peek` (several posts of a sender
    /// in flight, [`Bridge::pipelines_posts`]) a listed post that was owed an answer at the
    /// peek is skipped: it is listed again because its `outbox sent` has not gone yet. The
    /// skip reads the set taken at the peek, never what is owed when the loop reaches the
    /// post: any blocking broker call between (`conn_now`'s settle, before a receipt, an
    /// `ev`, a fetch's answer, a new subject's budget read, a keyed or retried post; a send's
    /// window or `settle_above`) can conclude a listed post, its pin cleared, and its stale
    /// row would take a fresh sequence and go out a second time (C12's review). Before each
    /// post the fences a refusal set are lifted ([`Bridge::lift_gap_fences`]), which reads
    /// every owed answer first, so a sender whose post was refused is held before its next
    /// is looked at. Without it, a sender's post waits for the answer to its last
    /// ([`Bridge::settle_sender`]). The look at aterm before each post is not the last word:
    /// the same blocking calls can conclude an earlier post whose `outbox sent` meets aterm
    /// gone, so each send looks again after the last of them ([`Bridge::unsendable`]; a retry
    /// or a keyed post after the settle before its read back, in [`Bridge::drain_post`]).
    fn drain_posts(
        &mut self,
        posts: Vec<QueuedPost>,
        owed_at_peek: Option<&BTreeSet<(String, u64)>>,
    ) -> bool {
        for post in posts {
            self.service_urgent();
            if self.aterm_gone() {
                return false;
            }
            // Behind the broker's rate limit ([`Bridge::rate`]), or an unverifiable reservation
            // of its sender's ([`Bridge::unverified`]): no pin is made, nothing is sent and no
            // answer waited on; the loop drains again once the hold is over. Only a post whose
            // record the attach's replay read back goes on, to be retired where it landed
            // ([`Bridge::landed_behind_an_unverifiable_hold`]).
            let now = self.fx.now();
            let rate_held = self.rate.holds_sender(&post.sid, now);
            if rate_held
                || self
                    .unverified
                    .holds(&post.sid, crate::unverified_hold::Domain::Posts, now)
            {
                self.held_senders.insert(post.sid.clone());
                if rate_held || !self.post_landed_behind_an_unverifiable_hold(&post) {
                    continue;
                }
            }
            match owed_at_peek {
                Some(owed) => {
                    self.lift_gap_fences();
                    if self.conn.is_none() || self.aterm_gone() {
                        return false;
                    }
                    #[cfg(test)]
                    let owed_now = self
                        .fx
                        .mutant(crate::sim::world::Mutant::LiveOwedSkip)
                        .then(|| self.posts_owed());
                    #[cfg(test)]
                    let owed = owed_now.as_ref().unwrap_or(owed);
                    if owed.contains(&(post.sid.clone(), post.id)) {
                        continue;
                    }
                }
                None => {
                    if !self.settle_sender(&post.sid) || self.aterm_gone() {
                        return false;
                    }
                }
            }
            if self.held_senders.contains(&post.sid)
                && !self.post_landed_behind_an_unverifiable_hold(&post)
            {
                continue;
            }
            if !self.drain_post(post) {
                return false;
            }
            // This fault parks the first retirement before another post can be sent,
            // independent of how many budget reads this drain needs.
            if self.fault == Fault::ParkRetireWhileMarked {
                self.settle_acks();
            }
        }
        true
    }

    /// A POST OR RECEIPT OF A LANE AN UNVERIFIABLE RESERVATION HOLDS ([`Bridge::unverified`])
    /// WHOSE RECORD THE ATTACH'S REPLAY READ BACK ON THE BUS (critic item F21): `sid`'s, in
    /// `domain`, at the pin `pin` reads. Held for its retry ([`Bridge::awaiting_drain`]), the
    /// record is not delivered until the drain retires it, and every delivery that meets it
    /// drops the link ([`Bridge::settled_before_delivery`]). So the drain retires it at the
    /// offset the replay read back for its sequence ([`Bridge::landed_at_its_pin`]), whatever
    /// holds its lane: a post, keyed or not, and an owed receipt alike. Nothing is sent, since
    /// it landed already, and held back it would hold no order, only every delivery behind it,
    /// for as long as the hold lasts. NEGATIVE CONTROL (`Mutant::HeldRecordWaitsForTheHold`):
    /// it waits for its lane's hold, and each delivery of its record drops the link.
    fn landed_behind_an_unverifiable_hold(
        &self,
        sid: &str,
        domain: crate::unverified_hold::Domain,
        pin: impl FnOnce() -> io::Result<Option<PublishPin>>,
    ) -> bool {
        #[cfg(test)]
        if self
            .fx
            .mutant(crate::sim::world::Mutant::HeldRecordWaitsForTheHold)
        {
            return false;
        }
        if !self.unverified.holds(sid, domain, self.fx.now()) {
            return false;
        }
        let Ok(Some(pin)) = pin() else {
            return false;
        };
        self.read_back_for_its_retry(&pin).is_some()
    }

    /// [`Bridge::landed_behind_an_unverifiable_hold`] for a queued post.
    fn post_landed_behind_an_unverifiable_hold(&self, post: &QueuedPost) -> bool {
        self.landed_behind_an_unverifiable_hold(
            &post.sid,
            crate::unverified_hold::Domain::Posts,
            || self.state.post_pin(&post.sid, post.id),
        )
    }

    /// WHERE THE ATTACH'S REPLAY READ BACK `pin`'S RECORD, held for this retry: its entry awaits
    /// the drain there ([`Bridge::awaiting_drain`]) and the offset holds its sequence's claim
    /// ([`Bridge::claims`]). A keyed re-post shares its sequence with its key's earlier holder,
    /// so for its pin (`via_key`) the entry standing there must also be its own attempt's
    /// words, which the replay read back: the record is then the copy that attempt appended
    /// once retention dropped the key, as astream's dedup at the pin would answer it.
    fn read_back_for_its_retry(&self, pin: &PublishPin) -> Option<u64> {
        let &(off, _) = self.awaiting_drain.get(&pin.seq)?;
        if self.claims_now()?.get(&off) != Some(&pin.seq) {
            return None;
        }
        if pin.via_key {
            let entry = self
                .state
                .in_flight()
                .ok()?
                .into_iter()
                .find_map(|(seq, entry)| (seq == pin.seq).then_some(entry))?;
            if pin.identity.as_ref() != Some(&publication_identity(&entry.subject, &entry.body)) {
                return None;
            }
        }
        Some(off)
    }

    fn drain_post(&mut self, mut post: QueuedPost) -> bool {
        let routed = self
            .resolve_to(&post.to, Some(&post.sid))
            .and_then(|routed| {
                if let (Route::To(prefix), Some(re)) = (&routed, post.re) {
                    post.re = Some(self.wire_re(prefix, re)?);
                }
                Ok(routed)
            });
        let routed = match routed {
            Ok(routed) => routed,
            Err(e) => {
                stderr_line!(
                    "aterm-link: could not route post {} to {}: {e}",
                    post.id,
                    post.to
                );
                if self.conn.is_none() {
                    return false;
                }
                self.held_senders.insert(post.sid.clone());
                return true;
            }
        };
        match routed {
            Route::To(_) | Route::Exact(_) => {
                let (subject, in_body) = match routed {
                    Route::To(prefix) => (format!("{prefix}/{}", post.kind), None),
                    Route::Exact(subject) => (subject, Some(post.kind.clone())),
                    _ => unreachable!("matched above"),
                };
                let encoded = post_body(self.fx.wall_ms(), &post, in_body.clone()).encode();
                let opens = if in_body.is_some() {
                    Opens::Topic
                } else {
                    Opens::Lane
                };
                let existing = (|| -> io::Result<_> {
                    let pin = self.state.post_pin(&post.sid, post.id)?;
                    let keyed = post
                        .key
                        .as_deref()
                        .map(|key| self.state.key_pin(&post.sid, key))
                        .transpose()?
                        .flatten();
                    Ok((pin, keyed))
                })();
                let (existing_pin, keyed) = match existing {
                    Ok(existing) => existing,
                    Err(e) => {
                        stderr_line!(
                            "aterm-link: post {} reservation unreadable, left queued: {e}",
                            post.id
                        );
                        self.held_senders.insert(post.sid.clone());
                        return true;
                    }
                };
                #[cfg(test)]
                let existing_pin = existing_pin
                    .filter(|_| !self.fx.mutant(crate::sim::world::Mutant::FreshSeqOnRetry));
                if existing_pin.is_none() && keyed.is_none() {
                    if !self.settle_same_words(&post.sid, &publication_identity(&subject, &encoded))
                        || !self.settle_unjournaled_ahead(&post.sid, &subject)
                    {
                        return false;
                    }
                    if self.held_senders.contains(&post.sid) {
                        return true;
                    }
                }
                if existing_pin.is_none()
                    && keyed.is_none()
                    && !self.budget_allows(&subject, opens, 0)
                {
                    self.retire_at_the_door(&post, DeadReason::SubjectBudget);
                    return true;
                }
                self.mirror_drained_post(&subject);
                let fresh = existing_pin.is_none() && post.key.is_none();
                let reserved = (|| -> io::Result<PublishPin> {
                    if let Some(mut pin) = existing_pin {
                        if pin.via_key {
                            if let Some(keyed) = keyed.as_ref() {
                                if keyed.seq == pin.seq
                                    && keyed.owner == Some(post.id)
                                    && pin.identity.is_some()
                                    && keyed.identity == pin.identity
                                {
                                    pin.via_key = false;
                                    pin.original = None;
                                    pin.original_off = None;
                                    pin.owner = Some(post.id);
                                    self.state.set_post_pin(&post.sid, post.id, &pin)?;
                                } else if keyed.seq == pin.seq
                                    && pin.original_off.is_none()
                                    && keyed.identity == pin.original
                                    && keyed.original_off.is_some()
                                {
                                    pin.original_off = keyed.original_off;
                                    self.state.set_post_pin(&post.sid, post.id, &pin)?;
                                }
                            }
                        }
                        return Ok(pin);
                    }
                    let pin = match keyed {
                        Some(keyed) if keyed.owner == Some(post.id) => PublishPin {
                            seq: keyed.seq,
                            via_key: false,
                            identity: keyed.identity,
                            original: None,
                            original_off: None,
                            owner: Some(post.id),
                            at: keyed.at,
                        },
                        Some(keyed) => PublishPin {
                            seq: keyed.seq,
                            via_key: true,
                            identity: Some(publication_identity(&subject, &encoded)),
                            original: keyed.identity,
                            original_off: keyed.original_off,
                            owner: keyed.owner,
                            at: keyed.at,
                        },
                        None => {
                            let pin = PublishPin {
                                seq: self.next_seq()?,
                                via_key: false,
                                identity: Some(publication_identity(&subject, &encoded)),
                                original: None,
                                original_off: None,
                                owner: Some(post.id),
                                at: Some(self.bus_high),
                            };
                            if let Some(key) = post.key.as_deref() {
                                self.state.set_key_pin(&post.sid, key, &pin)?;
                            }
                            pin
                        }
                    };
                    self.state.set_post_pin(&post.sid, post.id, &pin)?;
                    Ok(pin)
                })();
                let pin = match reserved {
                    Ok(pin) => pin,
                    Err(e) => {
                        stderr_line!(
                            "aterm-link: post {} reservation refused, left queued: {e}",
                            post.id
                        );
                        self.held_senders.insert(post.sid.clone());
                        return true;
                    }
                };
                let staged = self.staged_refusal(&post.sid);
                if fresh && staged.is_none() {
                    return self.send_post(post, pin, subject, encoded);
                }
                // A retry, or a keyed post, goes alone, every owed answer read first (its
                // publish would read them), and not past one that held its sender or an aterm
                // gone meanwhile ([`Bridge::unsendable`]).
                self.settle_acks();
                self.lift_gap_fences();
                if self.conn.is_none() || self.aterm_gone() {
                    return false;
                }
                // Past a hold only to be retired where the replay read it back, never sent
                // ([`Bridge::landed_behind_an_unverifiable_hold`]).
                let unsendable = self.unsendable(&post.sid).is_some();
                if unsendable
                    && (staged.is_some() || !self.post_landed_behind_an_unverifiable_hold(&post))
                {
                    return true;
                }
                // A retry at a pin an earlier attempt took, read back first: past a cut
                // astream's dedup no longer answers the pin with the record that attempt
                // landed, which a reader can still be handed (carried below the base); the
                // attach's replay may have landed it so (`Bridge::replay_in_flight`). A keyed
                // re-post is read back by [`Bridge::keyed_repost_landed`]: at its own copy the
                // replay read back, held or not, and otherwise, unheld, at its key's winner.
                if staged.is_none() {
                    let read_back = if pin.via_key {
                        self.keyed_repost_landed(&pin, unsendable)
                    } else {
                        self.landed_at_its_pin(&pin, &subject, &encoded)
                            .map(|found| found.map(|off| (off, false)))
                    };
                    match read_back {
                        Ok(Some((off, winners))) => {
                            stderr_line!(
                                "aterm-link: post {} of {} is on the bus at @{off}, read back; \
                                 it is not published again",
                                post.id,
                                post.sid
                            );
                            // The key's winner is its key's record, not this attempt's: a keyed
                            // re-post claims nothing, as its dedup answer claims nothing.
                            let (deduped, actual) = if winners {
                                (true, pin.original.clone())
                            } else {
                                self.claim(off, pin.seq);
                                (false, pin.identity.clone())
                            };
                            self.settle_a_retry_answered_ahead(&post, off);
                            let sid = post.sid.clone();
                            // The entry an earlier attempt left standing, its record held for
                            // this retry (`Bridge::awaiting_drain`): aterm's answer here ends it
                            // ([`Bridge::land_group`]), so the refill after this drain delivers
                            // the record rather than meeting it held.
                            let entry = self.awaiting_drain.get(&pin.seq).map(|&(_, ours)| ours);
                            let next = self.land_alone(Landing {
                                of: Outboxed::Post(post),
                                subject,
                                pin,
                                off,
                                deduped,
                                actual,
                                entry,
                            });
                            return self.follow(&sid, next);
                        }
                        Ok(None) => {}
                        Err(e) => {
                            let next = self.post_refused(&post, &subject, pin.seq, e);
                            return self.follow(&post.sid, next);
                        }
                    }
                }
                if unsendable {
                    return true;
                }
                // Its entry, where its publish journals one, stands until the post left aterm's
                // outbox ([`Bridge::land_group`]).
                let published = match staged {
                    Some(refused) => Err(refused),
                    None => self.publish_journaled_entry(&pin, &subject, &encoded, None),
                };
                let sid = post.sid.clone();
                let next = match published {
                    Ok((off, deduped, actual, entry)) => {
                        if !pin.via_key {
                            self.claim(off, pin.seq);
                        }
                        self.settle_a_retry_answered_ahead(&post, off);
                        self.land_alone(Landing {
                            of: Outboxed::Post(post),
                            subject,
                            pin,
                            off,
                            deduped,
                            actual,
                            entry,
                        })
                    }
                    Err(e) => self.post_refused(&post, &subject, pin.seq, e),
                };
                if !self.follow(&sid, next) {
                    return false;
                }
            }
            route => self.retire_at_the_door(&post, route.reason()),
        }
        true
    }

    /// Before a post's first pin is made, read the answer to every post of its sender in the
    /// SAME WORDS still owed (its publication `identity`): the pin's head (`PublishPin::at`) is
    /// where a later read back starts ([`Bridge::landed_since`]), which takes the first record
    /// of those words above it for this post's, so each earlier copy of them must lie below
    /// it. A refused one holds the sender, and this post waits for a later drain, after that
    /// one's retry. Several posts in other words go on in flight together. `false` once the
    /// connection is lost.
    fn settle_same_words(&mut self, sid: &str, identity: &PublishIdentity) -> bool {
        #[cfg(test)]
        if self
            .fx
            .mutant(crate::sim::world::Mutant::PinAheadOfSameWords)
        {
            return self.conn.is_some();
        }
        let waiting = self.fx.now();
        while self
            .conn
            .as_ref()
            .is_some_and(|link| link.owed().any(|then| then.posted_in(sid, identity)))
            && self.settle_one(waiting)
        {}
        self.conn.is_some()
    }

    /// Before the first pin of a post to `subject` on one of this node's own lanes, read the
    /// answer to every post of its sender still owed that no in-flight entry stands for: one to
    /// another lane ([`Bridge::publish_journaled`]). Sent behind it, this post's entry would
    /// outlive a kill that left that one's answer unread, and the attach's replay would land it
    /// ahead of that one's retry, which only the drain sends ([`Bridge::settle_in_flight`]'s 4).
    /// A refused one holds the sender, and this post waits for a later drain. Posts with an entry
    /// go on in flight together, and so does one to another lane behind them, which no replay
    /// publishes. `false` once the connection is lost.
    fn settle_unjournaled_ahead(&mut self, sid: &str, subject: &str) -> bool {
        #[cfg(test)]
        if self
            .fx
            .mutant(crate::sim::world::Mutant::EntryAheadOfAPostWithNone)
        {
            return self.conn.is_some();
        }
        if self.on_own_lane(subject) {
            let waiting = self.fx.now();
            while self
                .conn
                .as_ref()
                .is_some_and(|link| link.owed().any(|then| then.unjournaled_post_of(sid)))
                && self.settle_one(waiting)
            {}
        }
        self.conn.is_some()
    }

    /// Why a post of `sid`'s is not sent now, though the drain reached it: an answer read since
    /// (a blocking call's settle, a full window's, a lifted fence's) refused an earlier post of
    /// `sid`'s and held the sender, so sent now this one would land ahead of that one's retry;
    /// or the answer it concluded took long enough for aterm to go (its retirement waits on
    /// aterm), and a bridge whose aterm is gone publishes nothing more. It stays queued at its
    /// pin for a later drain, its in-flight entry dropped whatever became of the link
    /// ([`Unsent`]), so no attach on the state dir, a successor's included, publishes it.
    fn unsendable(&self, sid: &str) -> Option<io::Error> {
        if self.aterm_gone() {
            return Some(io::Error::other("aterm is gone"));
        }
        #[cfg(test)]
        if self
            .fx
            .mutant(crate::sim::world::Mutant::SendPastAHeldSender)
        {
            return None;
        }
        self.held_senders
            .contains(sid)
            .then(|| io::Error::other(format!("held behind a refused post of {sid}'s")))
    }

    fn settle_sender(&mut self, sid: &str) -> bool {
        let waiting = self.fx.now();
        while self
            .conn
            .as_ref()
            .is_some_and(|link| link.owed().any(|then| then.posted_by(sid)))
            && self.settle_one(waiting)
        {}
        self.conn.is_some()
    }

    fn send_post(
        &mut self,
        post: QueuedPost,
        pin: PublishPin,
        subject: String,
        encoded: Vec<u8>,
    ) -> bool {
        debug_assert!(
            self.pipelines_posts()
                || !self
                    .conn
                    .as_ref()
                    .is_some_and(|link| link.owed().any(|then| then.posted_by(&post.sid))),
            "one pipelined post in flight per sender against a broker that does not gap-fence \
             ({})",
            post.sid
        );
        let journaled = self.on_own_lane(&subject);
        if journaled && self.conn.is_none() {
            let next = self.post_refused(&post, &subject, pin.seq, broker_unreachable());
            return self.follow(&post.sid, next);
        }
        if journaled {
            let entry = InFlight {
                pin: pin.clone(),
                subject: subject.clone(),
                body: encoded.clone(),
                log: self.conn.as_ref().and_then(|conn| conn.seen_log_identity()),
                at: Some(self.bus_high),
            };
            if let Err(e) = self.state.set_in_flight(&entry) {
                let next = self.post_refused(&post, &subject, pin.seq, e);
                return self.follow(&post.sid, next);
            }
        }
        let (seq, face) = (pin.seq, subject.clone());
        let then = Then::Post(Box::new(SentPost {
            post,
            subject,
            pin,
            journaled,
        }));
        self.send_pipelined(seq, &face, &encoded, then);
        if self.post_event.is_some() {
            self.waits
                .note(crate::wait_tally::Row::PostPublish, self.post_event);
        }
        true
    }

    fn follow(&mut self, sid: &str, next: PostNext) -> bool {
        match next {
            PostNext::Moves => true,
            PostNext::Holds => {
                self.held_senders.insert(sid.to_string());
                true
            }
            PostNext::Ends => false,
        }
    }

    fn conclude_post(&mut self, sent: SentPost, outcome: io::Result<u64>) {
        let SentPost {
            post,
            subject,
            pin,
            journaled,
        } = sent;
        let e = match outcome {
            Ok(off) => {
                // LANDED: its bookkeeping waits for its pass's group (critic item E07).
                let actual = pin.identity.clone();
                self.landings.push(Landing {
                    of: Outboxed::Post(post),
                    subject,
                    pin,
                    off,
                    deduped: false,
                    actual,
                    entry: journaled.then_some(true),
                });
                if !self.grouping {
                    self.flush_landings();
                }
                return;
            }
            Err(e) => e,
        };
        let unsent = self.unsent.take();
        let record = self.group_record.take();
        if journaled && !self.fate_unknown(&e) {
            self.drop_behind_a_refused_post(&post.sid, pin.seq, &e);
            self.drop_in_flight(pin.seq);
        }
        let next = self.post_refused(&post, &subject, pin.seq, e);
        if next != PostNext::Moves {
            self.held_senders.insert(post.sid.clone());
        }
        self.unsent = unsent;
        self.group_record = record;
    }

    /// THE LATER POSTS OF A REFUSED POST'S SENDER LEAVE THE IN-FLIGHT JOURNAL WITH IT, ahead of
    /// its own entry, on a link that gap-fences ([`Link::gap_fences`]): the broker refused the
    /// post at `seq`, so its fence refuses every write of this producer above `seq` that the link
    /// sent after it (`gap-fenced=`), and the fence is lifted only once their answers are read
    /// ([`Bridge::lift_gap_fences`]): none of them lands. Their answers, still owed, conclude
    /// them as ever. Left in the journal, an entry whose answer a kill or a lost link leaves
    /// unread would be published by the attach's replay ([`Bridge::settle_in_flight`]) ahead of
    /// the refused post's retry, which the drain sends first (C12s1's review). Against any other
    /// broker a sender has one post in flight, and none is owed after it.
    fn drop_behind_a_refused_post(&mut self, sid: &str, seq: u64, e: &io::Error) {
        if astream_broker::broker_refusal(e).is_none() {
            return;
        }
        let Some(link) = self.conn.as_ref().filter(|link| link.gap_fences()) else {
            return;
        };
        let behind: Vec<u64> = link
            .owed()
            .filter_map(|then| match then {
                Then::Post(sent)
                    if sent.journaled && sent.post.sid == sid && sent.pin.seq > seq =>
                {
                    Some(sent.pin.seq)
                }
                _ => None,
            })
            .collect();
        for later in behind {
            self.drop_in_flight(later);
        }
    }

    /// A held post's retry landed at `off`: where this bridge already delivered that ask's answer
    /// with no deadline standing, the settle is owed, so the landing arms nothing
    /// ([`Bridge::settle_deadline_for`]). Only a retry: an ask's first landing is armed whatever
    /// overtook its ack, for the sweep to read back.
    fn settle_a_retry_answered_ahead(&mut self, post: &QueuedPost, off: u64) {
        if post.dl.is_some() && self.settled_unarmed.get(&off) == Some(&post.sid) {
            self.note_asks([asks::Frame::DeadlineGone {
                off,
                sid: post.sid.clone(),
            }]);
        }
    }

    /// A post a retry, or a read back, landed, or a receipt ([`Bridge::receipt_landed`]),
    /// settled alone ([`Bridge::land_group`]), every landing a pass left ahead of it settled
    /// first: its `entry` is the in-flight entry its publish, or an earlier attempt's, left
    /// standing, if one journaled one ([`Bridge::publish_journaled_entry`]).
    fn land_alone(&mut self, landing: Landing) -> PostNext {
        self.flush_landings();
        let grouping = std::mem::replace(&mut self.grouping, false);
        let next = self
            .land_group(vec![landing])
            .pop()
            .map_or(PostNext::Moves, |(_, next)| next);
        self.grouping = grouping;
        next
    }

    /// Settle the landed posts a pass has read ([`Bridge::landings`]), one group after another,
    /// each read under the last one's settle included, every one of them alone in the order it
    /// was read: a group's settle runs nothing that groups ([`Bridge::grouped`]).
    fn flush_landings(&mut self) {
        while !self.landings.is_empty() {
            let group = std::mem::take(&mut self.landings);
            let grouping = std::mem::replace(&mut self.grouping, false);
            let unsent = self.unsent.take();
            let record = self.group_record.take();
            for (sid, next) in self.land_group(group) {
                if next != PostNext::Moves {
                    self.held_senders.insert(sid);
                }
            }
            self.unsent = unsent;
            self.group_record = record;
            self.grouping = grouping;
        }
    }

    /// LANDED POSTS SETTLED AS ONE GROUP (critic item E07): for every post of `group`, in the
    /// order of its acks,
    ///
    /// 1. its offset in the self-lane log where its record is this node's own
    ///    ([`Landing::remembers`]), the group's in one append ([`Bridge::remember_self_acks`]);
    /// 2. its key's identity, its ask's recipient and its deadline, each post's own
    ///    ([`Bridge::landing_line`]);
    /// 3. aterm's `outbox sent`, the group's back to back on the verb lane
    ///    ([`Bridge::retire_on_the_lane`]);
    /// 4. its in-flight entry's end and its reservation's, the group's in one journal write
    ///    ([`StateDir::settle_posts`]).
    ///
    /// THE SELF-LANE LOG TAKES THIS NODE'S OWN RECORDS ONLY (critic item F17): step 1 writes no
    /// offset of a post or a receipt onto another node's lane or a person's. The log answers one
    /// question, whether a record on this node's own lane under its own `<src>` is one it
    /// published ([`forged_self`], read where a record is delivered and where `inbox get
    /// @<off>` fetches one, [`Bridge::fetch_record`]), from a window of its newest
    /// [`SELF_ACK_KEEP`] offsets over a floor of its own, which accuses nothing below it and
    /// rises only where the window drops its oldest ([`self_window`], critic item F17's review).
    /// An offset on another lane is never an own-lane record's (a log's offsets are its
    /// records', and a move replaces the set, [`StateDir::forget_log_offsets`]), so it decides
    /// no verdict by membership; but it takes a slot, and a full window drops an own offset for
    /// it and raises its floor past that record. Every build before F17 wrote every landed
    /// post's offset (each post's alone from the bridge's first build, E07's group in one
    /// append), and every other record's only on its own lanes
    /// ([`Bridge::publish_journaled_entry`]): a node whose posts go mostly to people and other
    /// nodes vouched for its last 4,096 posts rather than its last 4,096 own records, so a
    /// forged own record behind that edge, fetched by its offset or offered by a refill, went
    /// unaccused, and each group of them cost an append and brought the log's compaction
    /// nearer. Those builds also read the floor off the window's oldest offset, so a fresh
    /// window's first landed post, to a person or not, raised the floor of the empty window,
    /// which accused every offset at or above it; with the floor kept apart
    /// (`Mutant::FloorOffTheOldest`, `tests::a_fresh_windows_first_own_record_moves_no_floor`)
    /// the own-only window accuses every offset the wide one did. A fresh window's floor is the
    /// log's head as its first attach read it, written then (critic item F20,
    /// [`Bridge::floor_at_the_head`]); every build before F20 floored one at 0. A window loaded
    /// from an older build's `acked` file, which names no floor, keeps that build's entries,
    /// every landed post's, over the floor that build read off its oldest: it accuses at least
    /// what that build's window did, and those entries age out as the window takes
    /// [`SELF_ACK_KEEP`] own records.
    ///
    /// Nothing else reads the log: a reservation of a build before publication identities took
    /// an offset there for evidence of its dedup answer until critic item F20, and is left
    /// queued now where its read back cannot verify it ([`Bridge::publish_reserved`]'s
    /// `legacy`). NEGATIVE CONTROL: `Mutant::WideAckedSet` writes every landed post's offset,
    /// as E07 did, and in a full window a post to a person raises the floor past a forged own
    /// record, which goes unaccused
    /// (`tests::a_post_onto_a_persons_lane_or_another_nodes_takes_no_slot_of_the_self_lane_window`).
    ///
    /// AN OWED RECEIPT IS SETTLED BY THE SAME STEPS (critic item F15, a group of one,
    /// [`Bridge::receipt_landed`]): its retirement is `deliver <sid> receipt=<rid> off=<off>`
    /// where a post's is `outbox sent`, it has no step 2, and its reservation's end follows the
    /// journal's write. So every rule below holds for it as for a post. Before F15 a
    /// receipt's entry ended at its ack, and the hole E06 recorded for posts stayed open for
    /// receipts: a bridge killed before the retirement left the next one the receipt's record to
    /// deliver while aterm still owed the receipt, and the drain's retry at the pin, past a cut
    /// that gave that record up, landed the answer a second time (the sim's `receipt:` law,
    /// `answered 2 times`, at seed 515 under a rate limit that binds).
    ///
    /// AN OWN RECORD'S ENTRY STANDS UNTIL ATERM ANSWERED ITS POST'S `outbox sent`. While it
    /// stands the record is not delivered: a kill there leaves the next bridge the entry, whose
    /// replay holds the record back until the drain retires the post at its pin
    /// ([`Bridge::settle_in_flight`], [`Bridge::awaiting_drain`]), so a cut that gives up the
    /// record before that retry ("A CUT VERIFIES NOTHING", [`Bridge::publish_reserved`])
    /// gives up a copy no reader was handed. A build that ended the entry at the ack (as every
    /// build before E07 did) left the next bridge the record to deliver while the post stood
    /// queued, and a retry past such a cut appended a second copy (`dup delivery`, the sim's
    /// seed 78 under a rate limit of ten records a second and a burst of four, recorded at E06).
    /// The entry ends once the post left aterm's outbox ([`left_the_outbox`]): its `OK`
    /// retired it, or aterm said it holds no such post or session. A post still queued leaves
    /// the entry standing and its record held, as the attach's replay leaves one, for the
    /// drain's retry at its pin to read back and retire ([`Bridge::drain_outbox`]), or for a
    /// later drain that no longer lists it to release ([`Bridge::release_replays`]): one aterm
    /// did not answer (its lane gone), one it answered with anything else (`ERR busy
    /// update-committing` while a seamless update commits), and one held for its retry (a key
    /// or a deadline the disk refused), which sends no `outbox sent`. An entry ended there, as
    /// main's ended every entry at the ack, let the record be delivered while the post stood
    /// queued, and the retry past a cut appended a second copy.
    ///
    /// The self-lane log's offset comes first: an entry ended before it would leave the next
    /// bridge an own record that is neither in flight nor known as its own, which it refuses as
    /// forged. The reservation's end comes after aterm's `OK`: one ended before it would leave
    /// the next bridge a post that aterm still lists and that no pin holds, sent again at a
    /// fresh sequence.
    ///
    /// Answers each post's sender and what it does next.
    fn land_group(&mut self, group: Vec<Landing>) -> Vec<(String, PostNext)> {
        #[cfg(test)]
        let (ends_first, ends_at_the_ack, retires_first, wide) = (
            self.fx.mutant(crate::sim::world::Mutant::LandedBeforeAcked),
            self.fx
                .mutant(crate::sim::world::Mutant::LandedBeforeOutboxSent),
            self.fx
                .mutant(crate::sim::world::Mutant::RetireBeforeOutboxSent),
            self.fx.mutant(crate::sim::world::Mutant::WideAckedSet),
        );
        #[cfg(not(test))]
        let (ends_first, ends_at_the_ack, retires_first, wide) = (false, false, false, false);
        let every_entry = |group: &[Landing], remembered: bool| -> Vec<Option<u64>> {
            group
                .iter()
                .map(|l| {
                    l.entry
                        .filter(|&ours| remembered || !ours)
                        .map(|_| l.pin.seq)
                })
                .collect()
        };
        if ends_first {
            let ends = every_entry(&group, true);
            self.settle_group(&group, &ends, &[]);
        }
        let own: Vec<u64> = group
            .iter()
            .filter(|l| {
                l.remembers(|face| self.on_own_lane(face))
                    || (wide && matches!(l.of, Outboxed::Post(_)))
            })
            .map(|l| l.off)
            .collect();
        let remembered = self.remember_self_acks(own).is_ok();
        if ends_at_the_ack {
            let ends = every_entry(&group, remembered);
            self.settle_group(&group, &ends, &[]);
        }
        let mut nexts = Vec::with_capacity(group.len());
        let mut lines = Vec::new();
        for (i, landing) in group.iter().enumerate() {
            match self.landing_line(landing) {
                Ok(line) => {
                    lines.push((i, line));
                    nexts.push(PostNext::Moves);
                }
                Err(next) => nexts.push(next),
            }
        }
        if retires_first {
            let retired: Vec<bool> = (0..group.len())
                .map(|i| lines.iter().any(|(at, _)| *at == i))
                .collect();
            self.settle_group(&group, &vec![None; group.len()], &retired);
        }
        if self.fault == Fault::ParkRetireWhileMarked && !lines.is_empty() {
            self.park_retire_while_marked();
        }
        let sent: Vec<&str> = lines.iter().map(|(_, line)| line.as_str()).collect();
        let replies = self.retire_on_the_lane(&sent);
        // By place: whether aterm's answer retired the post (a receipt's reservation goes once
        // it left, as it always went), and whether it left the outbox.
        let (mut retired, mut left) = (vec![false; group.len()], vec![false; group.len()]);
        #[cfg(test)]
        let mut answered = vec![false; group.len()];
        for ((i, line), reply) in lines.iter().zip(replies) {
            let gone = match &reply {
                Ok(reply) => left_the_outbox(reply),
                Err(_) => false,
            };
            if !gone {
                let answer = match &reply {
                    Ok(reply) => reply.header().to_string(),
                    Err(e) => e.to_string(),
                };
                stderr_line!("aterm-link: `{line}` left it queued at its pin: {answer}");
            }
            if let Ok(reply) = reply {
                retired[*i] = match group[*i].of {
                    Outboxed::Post(_) => reply.ok(),
                    Outboxed::Receipt(_) => gone,
                };
                left[*i] = gone;
                #[cfg(test)]
                {
                    answered[*i] = true;
                }
            }
        }
        // The control: an entry ended at any answer and at a hold, as E07's first cut ended it.
        #[cfg(test)]
        if self
            .fx
            .mutant(crate::sim::world::Mutant::LandedWhileThePostStaysQueued)
        {
            for ((left, answered), next) in left.iter_mut().zip(&answered).zip(&nexts) {
                *left |= *answered || *next != PostNext::Moves;
            }
        }
        let ends: Vec<Option<u64>> = every_entry(&group, remembered)
            .into_iter()
            .zip(&left)
            .map(|(end, left)| end.filter(|_| *left))
            .collect();
        self.settle_group(&group, &ends, &retired);
        for ((landing, end), left) in group.iter().zip(&ends).zip(&left) {
            let (Some(ours), None, false) = (landing.entry, *end, ends_first || ends_at_the_ack)
            else {
                continue;
            };
            let seq = landing.pin.seq;
            // Delivered once a later settle ends its entry, as `Bridge::settle_landed` leaves
            // one whose self-lane offset the disk refused; and while the post may still be
            // queued, held as the attach's replay holds one, for the drain's retry to retire or
            // a later drain to release.
            self.unsettled.insert(landing.off, seq);
            if !left {
                self.awaiting_drain.insert(seq, (landing.off, ours));
            }
        }
        group
            .into_iter()
            .zip(nexts)
            .map(|(landing, next)| (landing.of.sid().to_string(), next))
            .collect()
    }

    /// Write `group`'s in-flight ends (`ends`, a sequence where one ends) and its posts'
    /// reservations' ends (`retired`, by place; a shorter slice retires the rest of none) in one
    /// journal write ([`StateDir::settle_posts`]), and let each ended entry's record be
    /// delivered; one the write did not end is delivered once a later settle ends it, as
    /// [`Bridge::settle_landed`] leaves one. A retired receipt's reservation, a file of its own,
    /// is forgotten after that write ([`StateDir::clear_receipt_seq`]), as a post's `Retire`
    /// frame follows its `Landed` one.
    fn settle_group(&mut self, group: &[Landing], ends: &[Option<u64>], retired: &[bool]) {
        let retires = |i: usize| retired.get(i).copied().unwrap_or(false);
        let settles: Vec<crate::state::PostSettle<'_>> = group
            .iter()
            .enumerate()
            .map(|(i, landing)| crate::state::PostSettle {
                landed: ends.get(i).copied().flatten(),
                retired: match &landing.of {
                    Outboxed::Post(post) if retires(i) => Some((post.sid.as_str(), post.id)),
                    _ => None,
                },
            })
            .collect();
        if settles
            .iter()
            .any(|s| s.landed.is_some() || s.retired.is_some())
        {
            let written = self.state.settle_posts(&settles);
            for (landing, end) in group.iter().zip(ends) {
                let Some(seq) = *end else {
                    continue;
                };
                match &written {
                    Ok(()) => {
                        self.unsettled.retain(|_, s| *s != seq);
                        self.awaiting_drain.remove(&seq);
                    }
                    Err(e) => {
                        stderr_line!(
                            "aterm-link: the in-flight entry at sequence {seq} (@{}) could not \
                             be cleared: {e}; the record is delivered once it is",
                            landing.off
                        );
                        self.unsettled.insert(landing.off, seq);
                    }
                }
            }
        }
        for (i, landing) in group.iter().enumerate() {
            if let Outboxed::Receipt(r) = &landing.of {
                if retires(i) {
                    self.state.clear_receipt_seq(&r.sid, r.rid);
                }
            }
        }
    }

    /// One landed post's own steps ahead of its `outbox sent` ([`Bridge::land_group`]): its key's
    /// identity, its ask's recipient and its deadline. Answers the `outbox sent` line, or that
    /// the post is held for its retry. A receipt has none of those steps: its line is its
    /// `deliver <sid> receipt=<rid> off=<off>` (critic item F15).
    fn landing_line(&mut self, landing: &Landing) -> Result<String, PostNext> {
        let Landing {
            of,
            subject,
            pin,
            off,
            deduped,
            actual,
            entry: _,
        } = landing;
        let (off, deduped) = (*off, *deduped);
        let post = match of {
            Outboxed::Post(post) => post,
            Outboxed::Receipt(r) => {
                return Ok(format!("deliver {} receipt={} off={off}", r.sid, r.rid));
            }
        };
        let (seq, via_key) = (pin.seq, pin.via_key);
        let own_attempt = if !deduped || !via_key {
            Some(true)
        } else {
            match (
                actual.as_ref(),
                pin.original.as_ref(),
                pin.identity.as_ref(),
            ) {
                (Some(actual), Some(original), Some(attempt)) if original != attempt => {
                    // Verified different semantic identities decide ownership without an offset.
                    Some(actual == attempt)
                }
                _ => pin.original_off.map(|original_off| off != original_off),
            }
        };
        if (actual.is_none() || own_attempt.is_none())
            && WAITING_KINDS.contains(&post.kind.as_str())
        {
            stderr_line!(
                "aterm-link: post {} has no verifiable original reply route or key offset; left queued",
                post.id
            );
            return Err(PostNext::Holds);
        }
        let appended = own_attempt.unwrap_or(false) && actual.is_some();
        if let (Some(key), Some(actual)) = (post.key.as_deref(), actual.as_ref()) {
            if let Err(e) = self.state.reconcile_key(
                &post.sid,
                key,
                seq,
                actual,
                off,
                appended.then_some(post.id),
            ) {
                stderr_line!(
                    "aterm-link: post {} key identity could not persist, left queued: {e}",
                    post.id
                );
                return Err(PostNext::Holds);
            }
        }
        self.indexed_subjects.insert(subject.clone());
        let to = actual
            .as_ref()
            .and_then(|identity| replier_of(&identity.subject));
        // THE ASK'S RECIPIENT AND ITS DEADLINE ARE ONE APPEND to the asks journal (C04), where a
        // build before it replaced two whole tables here, ahead of the `outbox sent` that
        // releases the sender.
        let waiting = appended && WAITING_KINDS.contains(&post.kind.as_str());
        if waiting {
            if let Some(to) = &to {
                self.note_asked(off, to);
            }
        }
        if waiting && post.dl.is_none() && to.is_some() {
            // The recipient alone: a refused write is said and rewritten at the next, as the
            // asked table's was.
            self.persist_asks();
        }
        if let Some(dl) = post.dl.filter(|_| waiting) {
            let noted = self.note_deadline(&post.sid, off, dl, to);
            #[cfg(test)]
            let noted = noted
                || self
                    .fx
                    .mutant(crate::sim::world::Mutant::RetireOverAnUnwrittenDeadline);
            if !noted {
                stderr_line!(
                    "aterm-link: post {}'s deadline could not persist, left queued",
                    post.id
                );
                return Err(PostNext::Holds);
            }
        }
        let dup = if via_key && deduped && !appended {
            " dup=1"
        } else {
            ""
        };
        Ok(format!(
            "outbox sent {} {} off={off}{dup}",
            post.sid, post.id
        ))
    }

    /// A GROUP'S `outbox sent` LINES, BACK TO BACK (critic item E07): in as few writes as
    /// [`Ctl::pipeline`] keeps whole, the `deliver` lines an inbox run queued sent ahead of them
    /// ([`Bridge::send_rows`]), where aterm is this bridge's own build on its own lane
    /// ([`Bridge::lane_pipelines`]); one request each anywhere else. Answers each line's reply,
    /// in order.
    fn retire_on_the_lane(&mut self, lines: &[&str]) -> Vec<io::Result<Reply>> {
        if lines.len() < 2 || !self.lane_pipelines() {
            return lines.iter().map(|line| self.ctl_request(line)).collect();
        }
        self.send_rows();
        let mut started = self.waits.starts();
        let (ctl, waits) = (&mut self.ctl, &mut self.waits);
        let replies = self.fx.lane(|| {
            ctl.pipeline(lines, |_| {
                if started.is_some() {
                    waits.note(crate::wait_tally::Row::LaneSent, started);
                    started = waits.starts();
                }
            })
        });
        note_ctl_loss(
            &self.ctl,
            &self.mailbox,
            replies.iter().find_map(|reply| reply.as_ref().err()),
        );
        replies
    }

    fn post_refused(
        &mut self,
        post: &QueuedPost,
        subject: &str,
        seq: u64,
        e: io::Error,
    ) -> PostNext {
        // UNVERIFIABLE (critic item F21): its sender is held, on a back-off, and the drain goes
        // on with every other sender's posts; under the control the link went, as it did before.
        if unverifiable(&e) && self.conn.is_some() {
            #[cfg(test)]
            if self
                .fx
                .mutant(crate::sim::world::Mutant::UnverifiableHoldsNothing)
            {
                stderr_line!("aterm-link: publish of post {} failed: {e}", post.id);
                return PostNext::Moves;
            }
            let what = format!("post {}", post.id);
            self.hold_unverifiable(
                &post.sid,
                crate::unverified_hold::Domain::Posts,
                seq,
                &what,
                &e,
            );
            return PostNext::Holds;
        }
        stderr_line!("aterm-link: publish of post {} failed: {e}", post.id);
        if e.kind() == io::ErrorKind::AlreadyExists {
            self.state.clear_post_seq(&post.sid, post.id);
            if let Some(k) = post.key.as_deref() {
                if let Err(e) = self.state.forget_key_seq(&post.sid, k, seq) {
                    stderr_line!("aterm-link: could not forget key {k:?}: {e}");
                }
            }
            return PostNext::Ends;
        }
        // RATE-LIMITED: nothing was appended, and the same publish lands once the bucket has
        // refilled. Its sender waits the whole wait the broker named, its post at its pin, and
        // goes on in its order after it (the drain sends nothing of a held sender's).
        if let Some(wait) = crate::rate_hold::wait_of(&e) {
            #[cfg(test)]
            if self
                .fx
                .mutant(crate::sim::world::Mutant::RateLimitedIsForGood)
            {
                self.retire_at_the_door(post, DeadReason::Refused);
                return PostNext::Moves;
            }
            #[cfg(test)]
            let at_once = self
                .fx
                .mutant(crate::sim::world::Mutant::RetryRateLimitedAtOnce)
                || self
                    .fx
                    .mutant(crate::sim::world::Mutant::SenderWaitsOnlyTheProducerHold);
            #[cfg(not(test))]
            let at_once = false;
            if !at_once {
                let now = self.fx.now();
                self.rate.hold_sender(&post.sid, now, wait);
            }
        }
        if refused_for_good(&e) {
            self.retire_at_the_door(post, DeadReason::Refused);
            PostNext::Moves
        } else if past_the_budget(&e) && self.writes_alone(subject) {
            self.indexed_subjects.remove(subject);
            self.retire_at_the_door(post, DeadReason::SubjectBudget);
            PostNext::Moves
        } else if self.conn.is_none() {
            PostNext::Ends
        } else {
            PostNext::Holds
        }
    }

    fn retire_at_the_door(&mut self, post: &QueuedPost, why: DeadReason) {
        let why = why.word();
        let retired = self.ctl_request(&format!(
            "outbox sent {} {} off=- reason={why}",
            post.sid, post.id
        ));
        if retired.is_ok_and(|r| r.ok()) {
            self.state.clear_post_seq(&post.sid, post.id);
        }
        match self.unrouted.take() {
            Some(no) => self.publish_ev(&format!(
                "undeliverable to={} reason={why} why={}",
                post.to,
                no.word()
            )),
            None => self.publish_ev(&format!("undeliverable to={} reason={why}", post.to)),
        }
    }

    fn writes_alone(&self, subject: &str) -> bool {
        let segs: Vec<&str> = subject.split('/').collect();
        let fleet = segs.get(2) == Some(&self.cfg.fleet.as_str());
        match segs.get(3) {
            Some(&"in") => fleet && segs.len() == 8 && segs[6] == self.node,
            Some(&"pub") => fleet && segs.get(4) == Some(&self.node.as_str()),
            _ => false,
        }
    }

    /// Run `step` as one budget scope ([`BudgetScope`]): a drain, or a mirror pass. The outermost
    /// scope starts with nothing read and forgets its reading as it ends; a scope run inside
    /// another judges by the outer one's reading, which counts what both send.
    ///
    /// NEGATIVE CONTROL (`Mutant::BudgetReadOutlivesItsScope`): a scope's reading is kept for
    /// the next, so what another connection under this producer spent between them is not read.
    fn budget_scoped<R>(&mut self, step: impl FnOnce(&mut Self) -> R) -> R {
        let outermost = self.budget_scope == BudgetScope::Out;
        if outermost {
            self.budget_scope = BudgetScope::Unread;
        }
        let r = step(self);
        if outermost {
            #[cfg(test)]
            let kept = self
                .fx
                .mutant(crate::sim::world::Mutant::BudgetReadOutlivesItsScope)
                && matches!(self.budget_scope, BudgetScope::Read { .. });
            #[cfg(not(test))]
            let kept = false;
            if !kept {
                self.budget_scope = BudgetScope::Out;
            }
        }
        r
    }

    /// A publish or a will to `subject` is being written: one more subject the running scope's
    /// reading counts as perhaps opened, unless the bridge knows it indexed ([`BudgetScope`]).
    fn count_opening(&mut self, subject: &str) {
        if let BudgetScope::Read { opened, .. } = &mut self.budget_scope {
            if !self.indexed_subjects.contains(subject) {
                *opened = opened.saturating_add(1);
            }
        }
    }

    /// Whether a publish to `subject` may go out under the node's subject budget: an indexed
    /// subject always may, and so may any while the budget cannot be read; a new one only
    /// while it leaves the reserve `opens` keeps whole, counting `opening` new subjects the
    /// caller already allowed ahead of it, which the broker's count does not hold until they
    /// land.
    ///
    /// The budget is read once a scope ([`BudgetScope`], critic item G04): a later subject the
    /// reading less what the bridge sent since leaves room for goes without a request, and one
    /// it does not is judged as every subject was before, by a fresh reading (which the scope
    /// keeps) and the subject's own `last`. NEGATIVE CONTROLS: `Mutant::BudgetReadPerSubject`
    /// reads the budget for every subject (the bridge before G04),
    /// `Mutant::BudgetCountTakenAsUnlimited` lets every subject after a scope's first read
    /// open, and `Mutant::BudgetShortOnTheCount` refuses where the count says short without
    /// the fresh reading.
    fn budget_allows(&mut self, subject: &str, opens: Opens, opening: u64) -> bool {
        if self.indexed_subjects.contains(subject) {
            return true;
        }
        if let BudgetScope::Read { budget, opened } = self.budget_scope {
            #[cfg(test)]
            if self
                .fx
                .mutant(crate::sim::world::Mutant::BudgetCountTakenAsUnlimited)
            {
                return true;
            }
            #[cfg(test)]
            let per_subject = self
                .fx
                .mutant(crate::sim::world::Mutant::BudgetReadPerSubject);
            #[cfg(not(test))]
            let per_subject = false;
            let left = budget.remaining().saturating_sub(opened);
            if !per_subject && left > opens.reserve(budget.max).saturating_add(opening) {
                return true;
            }
            #[cfg(test)]
            if !per_subject
                && self
                    .fx
                    .mutant(crate::sim::world::Mutant::BudgetShortOnTheCount)
            {
                return false;
            }
        }
        let producer = self.producer_id;
        #[cfg(test)]
        {
            self.budget_queries += 1;
        }
        let fx = self.fx.clone();
        let Some(conn) = self.conn_now() else {
            return true;
        };
        let started = fx.now();
        let answer = conn.subject_budget(producer);
        let budget =
            match self.observe_as(crate::wait_tally::Row::BudgetRequest, started, answer, None) {
                Ok(Some(budget)) => budget,
                Ok(None) | Err(_) => return true,
            };
        if self.budget_scope != BudgetScope::Out {
            self.budget_scope = BudgetScope::Read { budget, opened: 0 };
        }
        let reserve = opens.reserve(budget.max);
        if budget.remaining() > reserve.saturating_add(opening) {
            return true;
        }
        let fx = self.fx.clone();
        let Some(conn) = self.conn_now() else {
            return true;
        };
        let started = fx.now();
        let answer = conn.last(subject, "", 1);
        let indexed = match self.observe(started, answer, None) {
            Ok((rows, _)) => rows.iter().any(|(_, s, _)| s == subject),
            Err(_) => return true,
        };
        if indexed {
            self.indexed_subjects.insert(subject.to_string());
            return true;
        }
        let ahead = if opening == 0 {
            String::new()
        } else {
            format!(" ({opening} of them already allowed to new subjects ahead of it)")
        };
        stderr_line!(
            "aterm-link: the post to {subject} would open a subject with {} of this \
             node's {} left{ahead}: the last {reserve} are kept for {}",
            budget.remaining(),
            budget.max,
            opens.kept_for()
        );
        false
    }

    fn node_seen(&mut self, node: &str) -> io::Result<bool> {
        if node == self.node || self.seen_nodes.contains(node) {
            return Ok(true);
        }
        let face = subject::node_face(&self.cfg.fleet, node, "presence");
        let fx = self.fx.clone();
        let conn = self.conn_now().ok_or_else(|| {
            io::Error::new(io::ErrorKind::NotConnected, "the broker is unreachable")
        })?;
        let started = fx.now();
        let answer = conn.last(&face, "", 1);
        let (rows, _) = self.observe(started, answer, None)?;
        let seen = rows.iter().any(|(_, s, _)| *s == face);
        if seen {
            self.seen_nodes.insert(node.to_string());
        }
        Ok(seen)
    }

    fn resolve_to(&mut self, to: &str, from_sid: Option<&str>) -> io::Result<Route> {
        self.unrouted = None;
        let to = crate::pct::decode(to);
        let fleet = self.cfg.fleet.clone();
        if to == "say" || to.starts_with("say:") {
            let topic = to.strip_prefix("say:").unwrap_or("say");
            if !subject::is_topic(topic) {
                return Ok(Route::Unroutable);
            }
            return Ok(match from_sid.filter(|s| subject::is_principal(s)) {
                Some(sid) => {
                    Route::Exact(format!("/f/{fleet}/pub/{}/{sid}/say/{topic}", self.node))
                }
                None => Route::Unroutable,
            });
        }
        if let Some(rest) = to.strip_prefix('@') {
            let (sid, node) = match rest.split_once('@') {
                Some((sid, node)) => (sid.to_string(), Some(node.to_string())),
                None => (rest.to_string(), None),
            };
            if let (true, Some(node)) = (crate::person::is_person_id(&sid), node.as_deref()) {
                return Ok(self.person_post_route(&sid, node));
            }
            if !subject::is_principal(&sid) || !sid.starts_with("s-") {
                return Ok(Route::Unroutable);
            }
            let mut hosted = self.per_sid.epochs.contains_key(&sid);
            if !hosted && !self.roster_looked {
                let _ = self.refresh_sessions();
                hosted = self.per_sid.epochs.contains_key(&sid);
            }
            let node = match (node, hosted) {
                (Some(n), _) if self.cfg.mesh.is_some() && n != self.node => {
                    // With the mesh on (§11 C3) another Mac's sessions are not on this bus: the
                    // node service's hand-off names the members, and the push to the one named
                    // carries the post there, or holds it here until that Mac reads it.
                    match self.member_for(&n) {
                        Ok(Some(member)) => member,
                        Ok(None) => return Ok(Route::Unroutable),
                        Err(no) => {
                            self.unrouted = Some(no);
                            return Ok(Route::Unroutable);
                        }
                    }
                }
                (Some(n), _) => {
                    if !subject::is_principal(&n) || !n.starts_with("n-") {
                        return Ok(Route::Unroutable);
                    }
                    match self.state.pins().get(&sid).cloned() {
                        Some(pinned) if pinned != n => return Ok(Route::Ambiguous),
                        Some(_) => {}
                        None if self.node_seen(&n)? => {}
                        None => return Ok(Route::Unroutable),
                    }
                    // The named node must advertise the sid `live`. Its row reading `exited`
                    // (the session ended or a quit withdrew it) or gone (its node gave the
                    // row back to its budget, `Bridge::reclaim_subjects`, or never wrote
                    // one) is no route: the post would land on a lane nobody reads.
                    if !(hosted && n == self.node) && !self.advertisers(&sid)?.contains(&n) {
                        return Ok(Route::Unroutable);
                    }
                    n
                }
                (None, true) => self.node.clone(),
                (None, false) => match self.pinned_node_for(&sid)? {
                    Ok(node) => node,
                    Err(route) => return Ok(route),
                },
            };
            return Ok(Route::To(format!(
                "/f/{fleet}/in/{node}/{sid}/{}",
                self.node
            )));
        }
        if let Some((person, node)) = to.split_once('@') {
            if crate::person::is_person_id(person) {
                return Ok(self.person_post_route(person, node));
            }
        }
        Ok(if subject::is_principal(&to) {
            Route::To(subject::person_lane(&fleet, &to, &self.node))
        } else {
            Route::Unroutable
        })
    }

    /// The route of a post to `person` on the Mac `node` names, `<person>@<node>` as a carried
    /// post shows them (§11 C3d, [`Bridge::person_route`]).
    fn person_post_route(&mut self, person: &str, node: &str) -> Route {
        match self.person_route(person, node) {
            Ok((lane, _)) => Route::To(lane),
            Err(NoLane::Handoff(no)) => {
                self.unrouted = Some(no);
                Route::Unroutable
            }
            Err(_) => Route::Unroutable,
        }
    }

    /// The member `node` names now, with the mesh on (§11 C3): this Mac's own node, a current
    /// member, or the member a key rotation made of it, as the node service's hand-off says;
    /// `None` for any other node, and with the mesh off.
    ///
    /// # Errors
    /// Another Mac's node while the hand-off is missing, stale (it names another node as this
    /// Mac's) or unreadable: routing to another Mac fails closed, and stderr says why.
    fn member_for(&mut self, node: &str) -> Result<Option<String>, NoHandoff> {
        if node == self.node {
            return Ok(Some(node.to_owned()));
        }
        let Some(root) = self.cfg.mesh.as_ref().map(|mesh| mesh.root.clone()) else {
            return Ok(None);
        };
        let me = self.node.clone();
        let handoff = self.handoff.read(&root, &me)?;
        Ok(handoff.current(node).map(str::to_owned))
    }

    /// Read again which nodes this Mac held before ([`Bridge::old_nodes`]) and the reads of their
    /// mail lanes ([`Bridge::old_reads`], [`Bridge::read_olds`]): with the mesh on, the
    /// hand-off's retired nodes whose seat is this node, and of the node service's reader's ring
    /// each read of one of theirs, at most `federate::RING_OLDS_MAX` (the most this bridge's
    /// reader attaches beside its node ring); none while either cannot be read. The ring names
    /// the nodes the node service moves the unread mail of, whichever it chose, so this bridge
    /// reads back the ones it moved. The hand-off is read again only where its file changed, and
    /// the ring only where this Mac has retired nodes.
    fn refresh_old_nodes(&mut self) {
        self.read_olds.clear();
        let Some(root) = self.cfg.mesh.as_ref().map(|mesh| mesh.root.clone()) else {
            self.old_nodes.clear();
            self.old_reads.clear();
            return;
        };
        let me = self.node.clone();
        self.old_nodes = self
            .handoff
            .read(&root, &me)
            .map(|handoff| handoff.retired(&me))
            .unwrap_or_default();
        if self.old_nodes.is_empty() {
            self.old_reads.clear();
            return;
        }
        let wanted: BTreeMap<String, &String> = self
            .old_nodes
            .iter()
            .map(|old| (crate::federate::old_read(old), old))
            .collect();
        let file = root.join(crate::federate::OLD_READS_FILE);
        let mut reads = Vec::new();
        for cap in read_cap_file(&file.to_string_lossy()).unwrap_or_default() {
            let Some(old) = wanted.get(&cap.grant) else {
                continue;
            };
            if self.read_olds.len() < crate::federate::RING_OLDS_MAX
                && self.read_olds.insert((*old).clone())
            {
                reads.push(cap);
            }
        }
        self.old_reads = reads;
    }

    /// Whether `node` is this Mac's: its node, or one it held before a key rotation.
    fn is_my_node(&self, node: &str) -> bool {
        node == self.node || self.old_nodes.contains(node)
    }

    /// The record at `off` of `filter` on this bus, and the origin it was published under
    /// (`None` where the broker stamps none), read on a connection of its own, which holds
    /// `reads` beside the ring: `None` where none lies at `off`, retention gave it up, or no
    /// grant reads it (`reads` that do not attach, as a ring minted under a secret the root no
    /// longer holds, among them). One `Fetch`: the connection asked for origins as it was made
    /// ([`Bridge::connect`]), so its page names each record's. A read refused below the
    /// filter's oldest is read again told, on the same connection, as [`Bridge::read_at`]
    /// reads one: the oldest is the greatest floor of the filter's subjects, and retention can
    /// still hold the record below it (finding F3); on a broker that does not tell fetches the
    /// refusal reads as none.
    fn held_record(
        &self,
        off: u64,
        filter: &str,
        reads: &[Cap],
    ) -> io::Result<Option<(BrokerRecord, crate::mailbox::Origin)>> {
        #[cfg(test)]
        ORIGIN_READS.with(|count| count.set(count.get() + 1));
        let (mut reader, _closer) = self.connect()?;
        if !reads.is_empty() && transport::attach_ring(&mut reader, reads).is_err() {
            return Ok(None);
        }
        let rows = match reader.fetch(off, filter, 1) {
            Ok((rows, _)) => rows,
            Err(e) if transport::retained_past(&e, off).is_some() => {
                #[cfg(test)]
                if self
                    .fx
                    .mutant(crate::sim::world::Mutant::ReadAtWithoutTheHeld)
                {
                    return Ok(None);
                }
                match told_read_at(&mut reader, off, filter) {
                    Ok(rows) => rows.unwrap_or_default(),
                    Err(e) if astream_broker::broker_refusal(&e).is_some() => return Ok(None),
                    Err(e) => return Err(e),
                }
            }
            Err(e) if astream_broker::broker_refusal(&e).is_some() => return Ok(None),
            Err(e) => return Err(e),
        };
        let Some(row) = rows.into_iter().next().filter(|row| row.0 == off) else {
            return Ok(None);
        };
        let origin = reader.page_origins().first().copied().flatten();
        Ok(Some((row, origin)))
    }

    /// The record at `off` on the inbox of a node this Mac held before a key rotation (§11 C3b),
    /// on `sid`'s lanes there where given, with that node and the record's origin: mail a bridge
    /// of that node delivered stays where it was, read under the node service's reader's ring
    /// ([`Bridge::old_reads`]). `None` with the mesh off and where none of them holds a record at
    /// `off` the ring reaches.
    ///
    /// # Errors
    /// A broker that cannot be read.
    fn old_inbox_record(
        &mut self,
        off: u64,
        sid: Option<&str>,
    ) -> io::Result<Option<(String, BrokerRecord, crate::mailbox::Origin)>> {
        #[cfg(test)]
        if OWN_LANES_ONLY.with(std::cell::Cell::get) {
            return Ok(None);
        }
        if self.cfg.mesh.is_none() {
            return Ok(None);
        }
        self.refresh_old_nodes();
        for old in &self.read_olds {
            let filter = match sid {
                Some(sid) => format!("/f/{}/in/{old}/{sid}/>", self.cfg.fleet),
                None => subject::inbox_filter(&self.cfg.fleet, old),
            };
            if let Some((row, origin)) = self.held_record(off, &filter, &self.old_reads)? {
                return Ok(Some((old.clone(), row, origin)));
            }
        }
        Ok(None)
    }

    /// The `re=` a record bound for the lane `prefix` carries (§11 C3). An offset on this bus
    /// means nothing on another Mac's, so with the mesh on, an answer or receipt to another Mac
    /// names the record it answers, when a push brought that record here from its sender, by the
    /// record's sequence number, which the push set to the offset the record has on the sender's
    /// bus: under the sender's producer, or, for mail it wrote under a node it held before a key
    /// rotation, under the producer of the node its seat holds now (§11 C3b). A record the node
    /// service moved here across this Mac's own rotation (`federate::Lane::Inbox`: under this
    /// node's producer, or an earlier node's of this Mac for one moved again at a later rotation,
    /// numbered by the offset it had on this log, incarnation 0, below every sequence a bridge
    /// numbers) answers as the record it was first moved from, read under the node service's
    /// reader's ring ([`Bridge::old_reads`]), one rotation at a time: by that one's sender's
    /// sequence, or by its offset here where this Mac wrote it, as an answer to this Mac's own
    /// session does, whose asker waits on the offset its ask has here. A record a bridge of a
    /// node this Mac held before delivered, which stays on that node's lanes, is read there under
    /// the same ring ([`Bridge::old_inbox_record`]) and answers as that node's record does. Any
    /// other record keeps `off`: one this Mac wrote, one retention gave up, and one moved from a
    /// record none of the reads reaches, which another Mac's answer names by `off` and this
    /// Mac's by the offset it was moved from.
    ///
    /// # Errors
    /// A broker that cannot be read.
    fn wire_re(&mut self, prefix: &str, off: u64) -> io::Result<u64> {
        let to = prefix
            .strip_prefix(&format!("/f/{}/in/", self.cfg.fleet))
            .and_then(|rest| rest.split('/').next())
            .unwrap_or_default();
        if self.cfg.mesh.is_none() || !to.starts_with("n-") {
            return Ok(off);
        }
        let local = to == self.node;
        if local {
            // Only a Mac that rotated its key holds moved mail of its own sessions.
            self.refresh_old_nodes();
            if self.old_nodes.is_empty() {
                return Ok(off);
            }
        }
        let fleet = self.cfg.fleet.clone();
        let inbox = format!("/f/{fleet}/in/{}/>", self.node);
        let found = match self.held_record(off, &inbox, &[])? {
            Some((row, origin)) => Some((self.node.clone(), row, origin)),
            None => self.old_inbox_record(off, None)?,
        };
        let Some((node, (_, subject, _), origin)) = found else {
            return Ok(off);
        };
        let Ok(addr) = subject::parse_in(&fleet, &node, &subject) else {
            return Ok(off);
        };
        let (off, origin) = match self.first_of_moves(&addr, off, origin)? {
            Ok(first) => first,
            Err((at, seq)) => return Ok(if local { seq } else { at }),
        };
        if local || self.is_my_node(&addr.src) || !addr.src.starts_with("n-") {
            return Ok(off);
        }
        Ok(self.sender_seq(&addr.src, origin).unwrap_or(off))
    }

    /// Where the record at `off` on `addr`'s lane here, read with `origin`, was before the node
    /// service moved it across this Mac's own key rotations (§11 C3b, `federate::Lane::Inbox`):
    /// followed back one rotation at a time, under the node service's reader's ring
    /// ([`Bridge::moved_origin`]), to the offset and origin of the record no move put there
    /// (itself, for one no move put here). `Err((at, seq))` where a hop further back cannot be
    /// read (a ring that does not reach that node's lane, or more than `RING_OLDS_MAX` moves):
    /// `at` the earliest offset reached here, `seq` the one its record was moved from.
    ///
    /// # Errors
    /// A broker that cannot be read.
    fn first_of_moves(
        &mut self,
        addr: &subject::InAddr,
        off: u64,
        origin: crate::mailbox::Origin,
    ) -> io::Result<Result<(u64, crate::mailbox::Origin), (u64, u64)>> {
        let (mut off, mut origin) = (off, origin);
        if self.moved_seq(origin).is_some() {
            // The hand-off and the reader's ring as the node service wrote them for the move:
            // this attach may have read them before it wrote them.
            self.refresh_old_nodes();
        }
        let mut hops = 0;
        while let Some(seq) = self.moved_seq(origin) {
            hops += 1;
            #[cfg(test)]
            let followed = ONE_HOP.with(std::cell::Cell::get).then_some(1);
            #[cfg(not(test))]
            let followed = None;
            if followed.is_some_and(|most| hops > most) {
                break;
            }
            let found = if hops > crate::federate::RING_OLDS_MAX {
                None
            } else {
                self.moved_origin(seq, addr)?
            };
            let Some(was) = found else {
                return Ok(Err((off, seq)));
            };
            (off, origin) = (seq, was);
        }
        Ok(Ok((off, origin)))
    }

    /// The offset on its sender's bus of a record a push brought here from `src`, read from the
    /// `origin` its read stamped on it ([`Bridge::wire_re`]): its sequence, where it came under
    /// `src`'s producer or under the producer of the member a key rotation made of `src` since
    /// (§11 C3b), below the sequences a receipt is numbered by. `None` for any other origin.
    fn sender_seq(&mut self, src: &str, origin: crate::mailbox::Origin) -> Option<u64> {
        let now = match self.member_for(src) {
            Ok(Some(now)) => Some(astream_cap::producer_id_of(&now)),
            _ => None,
        };
        match origin {
            Some((pid, seq))
                if (pid == astream_cap::producer_id_of(src) || Some(pid) == now)
                    && seq < astream_broker::ACK_SEQ_BASE =>
            {
                Some(seq)
            }
            _ => None,
        }
    }

    /// The offset a record whose origin is `origin` was moved from, where the node service moved
    /// it across a key rotation of this Mac ([`Bridge::wire_re`]): under the producer of this
    /// node or of one it held before, at incarnation 0, which no bridge numbers.
    fn moved_seq(&self, origin: Option<(u64, u64)>) -> Option<u64> {
        let (pid, seq) = origin?;
        let mine = std::iter::once(&self.node)
            .chain(&self.old_nodes)
            .any(|node| astream_cap::producer_id_of(node) == pid);
        (mine && astream_broker::incarnation_of(seq) == Some(0)).then_some(seq)
    }

    /// Where on this log a copy the node service moved across this Mac's own key rotations
    /// (`federate::Lane::Inbox`) was first: the offset of the record the first move took, read
    /// from the `origin` the read that brought the copy stamped on it (a move numbers its copy by
    /// the offset it moved, under this node's producer at incarnation 0, [`Bridge::moved_seq`])
    /// and followed back one rotation at a time, as [`Bridge::wire_re`] follows a receipt's,
    /// under the node service's reader's ring ([`Bridge::old_reads`]), on `addr`'s lane. Where
    /// a hop further back cannot be read (a broker that cannot be read, a ring that does not
    /// reach that node's lane, or more than `RING_OLDS_MAX` moves), the earliest offset reached,
    /// the same mail. `None` for a record no move put here, with the mesh off, and where aterm's
    /// lane predates `was=` ([`aterm_types::control_verbs::LINK_MOVED_LANE`]).
    ///
    /// The endpoint answers a delivery naming it from its memory of that offset
    /// ([`aterm_mesh_core::inbox_move::shows_anew`]), so mail a bridge of an older node
    /// delivered and left uncommitted (killed, or its commit held below a record it could not
    /// deliver) is not shown again from the move's copy (§11 C3b). Only a moved copy reads the
    /// bus here: a record no move put here is known for one by its origin alone.
    fn moved_from(
        &mut self,
        addr: &subject::InAddr,
        origin: crate::mailbox::Origin,
    ) -> Option<u64> {
        if self.cfg.mesh.is_none()
            || self.endpoint_lane < aterm_types::control_verbs::LINK_MOVED_LANE
        {
            return None;
        }
        let mut first = self.moved_seq(origin)?;
        // The hand-off and the reader's ring as the node service wrote them for the move: this
        // bridge may not have read them since, or at all yet (an attach's refill reads before it).
        self.refresh_old_nodes();
        for _ in 0..crate::federate::RING_OLDS_MAX {
            match self.moved_origin(first, addr) {
                Ok(Some(was)) => match self.moved_seq(was) {
                    Some(seq) => first = seq,
                    None => break,
                },
                _ => break,
            }
        }
        Some(first)
    }

    /// The origin of the record a move put at `addr`'s lane from offset `at`: the same lane of
    /// one of this Mac's old nodes, read under the reader's ring. `None` where none of them holds
    /// a record at `at` its reads reach; `Some(None)` for one the broker stamps no origin on.
    ///
    /// # Errors
    /// A broker that cannot be read.
    fn moved_origin(
        &self,
        at: u64,
        addr: &subject::InAddr,
    ) -> io::Result<Option<Option<(u64, u64)>>> {
        for old in &self.read_olds {
            let lane = format!(
                "/f/{}/in/{old}/{}/{}/{}",
                self.cfg.fleet,
                addr.sid,
                addr.src,
                addr.kind.as_str()
            );
            if let Some((_, was)) = self.held_record(at, &lane, &self.old_reads)? {
                return Ok(Some(was));
            }
        }
        Ok(None)
    }

    fn pinned_node_for(&mut self, sid: &str) -> io::Result<Result<String, Route>> {
        let pinned = self.state.pins().get(sid).cloned();
        if let Some(pinned) = pinned
            .as_deref()
            .filter(|pinned| self.cfg.mesh.is_some() && *pinned != self.node)
        {
            // With the mesh on (§11 C3) no other Mac's presence is on this bus: the pin routes,
            // to the member it names now.
            return Ok(match self.member_for(pinned) {
                Ok(Some(member)) => Ok(member),
                Ok(None) => Err(Route::Unroutable),
                Err(no) => {
                    self.unrouted = Some(no);
                    Err(Route::Unroutable)
                }
            });
        }
        #[cfg(test)]
        let settle = pinned.is_some() && !self.route_on_a_pin_that_reads;
        #[cfg(not(test))]
        let settle = pinned.is_some();
        if settle {
            self.state.settle_pins()?;
        }
        if let Some(node) = self.route_from_view(sid, pinned.as_deref())? {
            return match pinned {
                Some(_) => Ok(Ok(node)),
                None => self.state.pin(sid, &node).map(Ok),
            };
        }
        let advertisers = self.advertisers(sid);
        self.want_presence_view();
        let advertisers = advertisers?;
        Ok(match (&pinned, advertisers.len()) {
            (Some(node), _) if advertisers.iter().any(|n| n != node) => Err(Route::Ambiguous),
            // The pinned node no longer advertises the sid: its row reads `exited`, or is
            // gone once its node gave it back to its budget.
            (Some(_), 0) => Err(Route::Unroutable),
            (Some(node), _) => Ok(node.clone()),
            (None, 0) => Err(Route::Unroutable),
            (None, 1) => {
                let node = advertisers.into_iter().next().unwrap_or_default();
                return self.state.pin(sid, &node).map(Ok);
            }
            (None, _) => Err(Route::Ambiguous),
        })
    }

    fn route_from_view(&mut self, sid: &str, pinned: Option<&str>) -> io::Result<Option<String>> {
        let Some(view) = self.presence_view.snapshot(sid) else {
            return Ok(None);
        };
        let mut nodes = view.nodes.into_iter();
        let Some(node) = nodes.next() else {
            return Ok(None);
        };
        if nodes.next().is_some() || pinned.is_some_and(|p| p != node) {
            return Ok(None);
        }
        self.settle_acks();
        let heard = view.head.max(self.bus_high);
        if heard.saturating_sub(view.exact_to) > CONFIRM_SCAN_MAX {
            return Ok(None);
        }
        let filter = crate::presence_view::presence_filter(&self.cfg.fleet);
        let fx = self.fx.clone();
        let conn = self.conn_now().ok_or_else(|| {
            io::Error::new(io::ErrorKind::NotConnected, "the broker is unreachable")
        })?;
        let started = fx.now();
        let answer = transport::fetch_kept(conn, view.exact_to, &filter, CONFIRM_ROWS);
        let ((found, mark), floor) = self.observe(started, answer, Some("read"))?;
        if floor.is_some() {
            return Ok(None);
        }
        let confirmed =
            crate::presence_view::confirmation(&self.cfg.fleet, sid, view.exact_to, &found, mark);
        self.presence_view
            .advance(view.generation, confirmed.exact_to, mark.1);
        Ok(confirmed.routes.then_some(node))
    }

    fn want_presence_view(&mut self) {
        use crate::presence_view::Phase;
        if !self.fx.presence_view()
            || matches!(self.presence_view.phase(), Phase::Seeding | Phase::Live)
            || self.conn.is_none()
            || self
                .presence_view_retry_at
                .is_some_and(|at| self.fx.now() < at)
        {
            return;
        }
        self.presence_view_retry_at = Some(self.fx.now() + PRESENCE_VIEW_REOPEN);
        let broker = self.cfg.broker.clone();
        let ring = self.caps.clone();
        let judged = self.bus_log;
        let dial = move || {
            let (mut c, closer) = transport::connect(&broker, ack_deadline())?;
            transport::attach_ring(&mut c, &ring)?;
            same_log(judged, &c)?;
            Ok((c, closer))
        };
        let generation = self.presence_view.open();
        let spawned = crate::presence_view::spawn_reader(
            dial,
            &self.cfg.fleet,
            Arc::clone(&self.presence_view),
            generation,
        );
        if let Err(e) = spawned {
            stderr_line!("aterm-link: the presence view's reader could not start: {e}");
            self.presence_view.close();
        }
    }

    fn advertisers(&mut self, sid: &str) -> io::Result<BTreeSet<String>> {
        let filter = format!("/f/{}/pub/*/{sid}/presence", self.cfg.fleet);
        let rows = self.last_face(&filter)?;
        let mut live = BTreeSet::new();
        for (_, subject, raw) in &rows {
            let segs: Vec<&str> = subject.split('/').collect();
            if segs.len() != 7 || segs.get(5) != Some(&sid) {
                continue;
            }
            let (body, _) = Body::decode(raw);
            if body.unknown.get("observer").is_some_and(|v| v == "1") {
                continue;
            }
            let Some(node) = segs.get(4).filter(|n| subject::is_principal(n)) else {
                continue;
            };
            if body.unknown.get("state").map(String::as_str) == Some("live") {
                live.insert((*node).to_string());
            }
        }
        Ok(live)
    }

    fn on_event(&mut self, line: &str) {
        let mut toks = line.split_whitespace();
        match toks.next() {
            Some("EVENT") => self.mirror_woken_by(line.split_whitespace().nth(1)),
            Some("GAP") => {
                let rest = line.strip_prefix("GAP").unwrap_or("").trim();
                self.publish_ev(&format!("gap {rest}"));
                self.mirror_wake();
                for slot in self.per_sid.presence.values_mut() {
                    slot.sampled = false;
                }
                self.roster_backstop();
                self.resample_topics();
                self.roster_due = self.fx.now() + roster_refresh();
                return;
            }
            Some("sub") => {
                let Some((local, sid)) = watch_ack(line) else {
                    return;
                };
                self.note_watch_ack(local, sid);
                if self.locals.get(&local).is_some_and(|listed| listed == sid)
                    && self.sample_topics(sid)
                {
                    self.per_sid.topics_sampled.insert(sid.to_string());
                }
                // THE ACK CHECKS A SAMPLED SLOT, AND NEVER ROLLS IT BACK (critic item E05). aterm
                // read the ack's meaning fields in one meta hold with the watch's start, so each
                // change after them is an `EVENT <local> meta` behind this ack. The read that
                // sampled the slot cannot be ordered against that start: one before it (a later
                // subscribe's) may lack a change no event will bring, and one after it (the
                // attach's batch, which `start()`'s subscribe precedes) is newer than the ack,
                // so folding the ack would put an older field back. An ack whose three the slot
                // already says asks nothing; one that differs, or says none, has the session's
                // own `@<sid> meta` read, which is later than the watch's start. A slot no read
                // sampled is left to the round or the admission.
                let checked = self.per_sid.presence.get(sid).and_then(|slot| {
                    slot.sampled.then(|| {
                        watch_ack_meta(line).is_some_and(|said| slot.fields.says_meta(&said))
                    })
                });
                #[cfg(test)]
                let checked = checked.map(|agrees| {
                    agrees
                        && !self
                            .fx
                            .mutant(crate::sim::world::Mutant::ReadMetaPerSession)
                });
                if checked == Some(false) {
                    self.refresh_meta(sid);
                }
                self.replay_topics(false);
                return;
            }
            _ => return,
        }
        let Some(target) = toks.next() else { return };
        let Some(kind) = toks.next() else { return };
        match kind {
            "post" => {
                if self.fault == Fault::DropPostEventWhileMarked
                    && self.state.root().join("drop-post-event").exists()
                {
                    self.fault.fired(&self.state);
                    return;
                }
                self.post_event = self.taken_at;
                self.drain_outbox();
                self.post_event = None;
            }
            "fetch" => {
                if self.fault == Fault::SkipRosterOutboxWhileMarked {
                    self.fault.fired(&self.state);
                }
                self.drain_outbox();
            }
            "inbox-seen" => {
                let Some(sid) = crate::parse_uint::<u64>(target).and_then(|l| self.locals.get(&l))
                else {
                    return;
                };
                let sid = sid.clone();
                let named: Option<u64> = toks.next().and_then(crate::parse_uint);
                let mut off: Option<u64> = None;
                let mut floor: Option<Option<u64>> = None;
                let mut watermark: Option<u64> = None;
                let mut decided = false;
                for t in toks {
                    if let Some(n) = t.strip_prefix("off=") {
                        off = crate::parse_uint(n);
                    } else if let Some(f) = t.strip_prefix("floor=") {
                        floor = Some(crate::parse_uint(f));
                    } else if let Some(w) = t.strip_prefix("seen=") {
                        watermark = crate::parse_uint(w);
                    } else if t.starts_with("verdict=") {
                        decided = true;
                    }
                }
                if let Some(refill_after) = seen_watermark(off, floor) {
                    let _ = self.state.set_seen_off(&sid, refill_after);
                }
                if let Some(seen) = watermark.or(named) {
                    self.note_handled(&sid, seen);
                }
                if decided {
                    self.drain_outbox();
                }
            }
            "session-created" => {
                let _ = self.refresh_sessions();
                self.admit_fresh_sessions();
                self.reassert_halt();
                self.sample_unsampled_topics();
                self.replay_topics(false);
            }
            "hold" => {
                let Some(sid) = crate::parse_uint::<u64>(target).and_then(|l| self.locals.get(&l))
                else {
                    return;
                };
                let sid = sid.clone();
                let held = toks.next() == Some("1");
                let (mut origin, mut standing) = (None, None);
                for tok in toks {
                    if let Some(v) = tok.strip_prefix("origin=") {
                        origin = Some(v);
                    } else if let Some(v) = tok.strip_prefix("hold_origin=") {
                        standing = Some(v);
                    }
                }
                self.converge_hold(&sid, EndpointHold::of(held, standing.or(origin)));
            }
            "topic" => {
                let Some(sid) = crate::parse_uint::<u64>(target).and_then(|l| self.locals.get(&l))
                else {
                    return;
                };
                let sid = sid.clone();
                self.topic_change(&sid, &mut toks);
            }
            "agent" => {
                let Some(sid) = crate::parse_uint::<u64>(target).and_then(|l| self.locals.get(&l))
                else {
                    return;
                };
                let sid = sid.clone();
                let word = toks.next();
                // The verdict names the screen it was judged on (`gen=`), so the row it owes
                // says that one, read off no screen.
                let screen = toks
                    .find_map(|t| t.strip_prefix("gen="))
                    .and_then(ScreenGen::parse);
                let slot = self.per_sid.presence.entry(sid.clone()).or_default();
                slot.saw_agent(word);
                slot.saw_screen(screen);
                self.publish_presence_if_due(&sid);
            }
            "meta" => {
                // See [`Fault::DropMetaEventWhileMarked`].
                if self.fault == Fault::DropMetaEventWhileMarked
                    && self.state.root().join("drop-meta-event").exists()
                {
                    self.fault.fired(&self.state);
                    return;
                }
                let Some(sid) = crate::parse_uint::<u64>(target).and_then(|l| self.locals.get(&l))
                else {
                    return;
                };
                let sid = sid.clone();
                self.refresh_meta(&sid);
            }
            "fabric-retire" => {
                let Some(sid) = toks.next().filter(|s| subject::is_principal(s)) else {
                    return;
                };
                self.retire_on_request(sid);
            }
            "bye" => {
                if let Some(bye) = toks.next().and_then(Bye::parse) {
                    self.take_bye(bye);
                }
            }
            "session-exited" => {
                // THE LINE THAT NEVER ARRIVES — see
                // [`Fault::DropSessionExitedWhileMarked`]. Everything that
                // follows the drop is the shipped path: the departure is then
                // discovered by LOOKING, in `refresh_sessions`.
                if self.fault == Fault::DropSessionExitedWhileMarked
                    && self.state.root().join("drop-session-exited").exists()
                {
                    return;
                }
                let sid = toks.next().unwrap_or(target).to_string();
                #[cfg(test)]
                let say = !self.fx.mutant(crate::sim::world::Mutant::SkipExitedRow);
                #[cfg(not(test))]
                let say = true;
                if say {
                    self.publish_session_presence(&sid, "exited");
                }
                self.per_sid.epochs.remove(&sid);
                self.roster_seen = None;
                let _ = self.refresh_sessions();
            }
            _ => {}
        }
    }

    fn take_bye(&mut self, bye: Bye) {
        // The count carries into a successor whose local ids can differ
        // (`continue_roster_seq`), so past a bye no count stands for a roster.
        self.roster_seen = None;
        if self.attachment != Attachment::Inherited || self.bye == Some(Bye::Quit) {
            return;
        }
        match self.ctl_request(&format!("link bye {}", bye.word())) {
            Ok(reply) if !reply.ok() => {
                stderr_line!("aterm-link: `link bye` refused: {}", reply.header());
            }
            Ok(_) => {}
            Err(e) => stderr_line!("aterm-link: `link bye` failed: {e}"),
        }
        self.bye = Some(bye);
        match bye {
            Bye::Quit => {
                let _ = self.refresh_sessions();
                self.withdraw();
            }
            Bye::Handoff => self.publish_ev("bye reason=handoff"),
        }
    }

    /// A QUIT'S `state=exited` ROW, the last the bridge publishes for its session, at one
    /// sequence: a refusal for the broker's rate limit, or the bridge's hold behind one
    /// ([`Bridge::rate`]), is waited out and the row sent again (critic item E06's review),
    /// while the quit's waits add up to at most [`crate::rate_hold::WITHDRAW_WAIT_MAX`] from its
    /// first row ([`Bridge::withdraw_waits_until`]). Past it, the refusal: the row stays `live`
    /// behind the node's will, read `away`, as at a quit the broker does not answer.
    ///
    /// # Errors
    /// The publish's, the last refusal for the rate among them.
    fn publish_withdrawal(&mut self, subject: &str, body: &[u8]) -> io::Result<u64> {
        let seq = self.next_seq()?;
        let now = self.fx.now();
        let until = *self
            .withdraw_waits_until
            .get_or_insert(now + crate::rate_hold::WITHDRAW_WAIT_MAX);
        #[cfg(test)]
        let until = if self
            .fx
            .mutant(crate::sim::world::Mutant::WithdrawalWaitsNothing)
        {
            now
        } else {
            until
        };
        let fx = self.fx.clone();
        crate::rate_hold::waiting_out(
            || self.publish_at(seq, subject, body),
            until.saturating_duration_since(now),
            |wait| fx.sleep(wait),
        )
        .map(|(off, _)| off)
    }

    fn withdraw(&mut self) {
        if self.bye != Some(Bye::Quit) {
            return;
        }
        if self.conn.is_none() {
            stderr_line!(
                "aterm-link: aterm is quitting and the broker is unreachable: its sessions are \
                 withdrawn once the link is back, or read `away` until then"
            );
            return;
        }
        let owed: Vec<String> = self
            .per_sid
            .epochs
            .keys()
            .filter(|sid| !self.withdrawn.contains(*sid))
            .cloned()
            .collect();
        let mut published = false;
        for sid in owed {
            if !self.publish_session_presence(&sid, "exited") {
                if self.conn.is_none() {
                    return;
                }
                continue;
            }
            self.withdrawn.insert(sid);
            published = true;
        }
        if published || !self.withdrawal_told {
            self.withdrawal_told = true;
            let n = self.withdrawn.len();
            self.publish_ev(&format!("bye reason=quit withdrawn={n}"));
        }
    }

    fn on_aterm_closed(&mut self) {
        if self.attachment != Attachment::Inherited {
            return;
        }
        match self.mailbox.bye() {
            Some(Bye::Quit) => {
                self.bye = Some(Bye::Quit);
                self.withdraw();
            }
            Some(Bye::Handoff) if self.bye.is_none() => {
                self.bye = Some(Bye::Handoff);
                self.publish_ev("bye reason=handoff");
            }
            _ => {}
        }
    }

    fn attach_broker(&mut self) -> bool {
        self.attaching = true;
        self.attach_rtt = None;
        let attached = self.attach_broker_inner();
        self.attaching = false;
        if attached {
            // THE FIRST `link up`, now that the link is one the bridge can
            // do its job on: the halt read, the presence publish and every
            // subscription answered. The round trip is the attach's last
            // ack — a real exchange with this broker, never a guess.
            if let Some(rtt) = self.attach_rtt.take() {
                self.note_ack(rtt);
            }
            self.mirror_attached();
        } else {
            self.attach_rtt = None;
        }
        attached
    }

    fn attach_broker_inner(&mut self) -> bool {
        self.close_subscriptions();
        self.mailbox.reset_broker_sources();
        self.say_drained_to = 0;
        self.say_bus_head = 0;
        if !self.run_names_this_boot() {
            return false;
        }
        self.reload_caps();
        let (conn, _pub_closer) = match self.connect() {
            Ok(c) => c,
            Err(e) => {
                self.dial_failed(&e);
                return false;
            }
        };
        self.conn = Some(Link::from(conn));
        self.commit_refused = false;
        // A new link may reach another broker: the running scope reads its budget again.
        if self.budget_scope != BudgetScope::Out {
            self.budget_scope = BudgetScope::Unread;
        }
        self.settle_refused = false;
        let Some((halts, (fleet_from, head))) = self.read_fleet_halts() else {
            stderr_line!("aterm-link: the standing fleet halt could not be read; not attaching");
            self.conn = None;
            return false;
        };
        if self.aterm_gone() {
            self.conn = None;
            return false;
        }
        let Ok(ident) = self.read_log_identity() else {
            self.conn = None;
            return false;
        };
        if !self.ask_gap_fences() {
            self.conn = None;
            return false;
        }
        self.note_broker_retention();
        let lineage = self.conn_now().and_then(|c| c.log_lineage().cloned());
        #[cfg(test)]
        let lineage = lineage.filter(|_| !self.ignore_lineage);
        let mut found = self.judge_log(head, ident, lineage.as_ref());
        if !self.log_identity_allows_attach(head, ident, &mut found) {
            self.conn = None;
            return false;
        }
        if let Some(ident) = ident {
            let continues = found.and_then(|f| Some((f.was_log?, f.continues?.min(head))));
            if !self.tell_endpoint_log(ident, continues) {
                self.conn = None;
                return false;
            }
        }
        if let Some(mut found) = found {
            match self.forget_log_offsets(head, ident, found.continues, lineage.as_ref()) {
                Ok((open, inflight)) => {
                    found.deadlines = open;
                    found.inflight = inflight;
                    self.log_replaced = Some(found);
                }
                Err(e) => {
                    stderr_line!(
                        "aterm-link: the bus log is not the one this node last read ({}), and \
                         its offsets could not be forgotten: {e}; not attaching",
                        found.why()
                    );
                    self.link_down("error");
                    self.conn = None;
                    return false;
                }
            }
        }
        self.floor_at_the_head(head);
        self.note_log(head, ident, found.is_some());
        if let Err(e) = self.bring_presence_up() {
            stderr_line!("aterm-link: presence could not come up: {e}");
            if e.kind() == io::ErrorKind::Other {
                self.link_down(link_reason(&e, "attach"));
            }
            self.conn = None;
            return false;
        }
        if let Some((seq, off)) = self.collided {
            let alarm = format!("cap-compromised seq={seq} off={off} reason=deduped");
            match self.try_publish_ev_for(None, &alarm) {
                Ok(_) => self.collided = None,
                Err(e) => stderr_line!("aterm-link: could not publish ev {alarm:?}: {e}"),
            }
            if self.conn.is_none() {
                return false;
            }
        }
        let halt_from = if self.cfg.mesh.is_some() {
            drop(halts);
            let Some(from) = self.attach_mesh_halts(fleet_from) else {
                stderr_line!(
                    "aterm-link: the halts over the mesh could not be read; not attaching"
                );
                self.conn = None;
                return false;
            };
            Some(from)
        } else {
            self.apply_fleet_halts(halts, fleet_from);
            None
        };
        if let Some(found) = self.log_replaced {
            match found.continues {
                Some(fork) => stderr_line!(
                    "aterm-link: the bus log moved to its next generation: {} (a power loss \
                     under a weak tier, a torn batch cut, or a repair); every offset this node \
                     held at or above {fork} is forgotten and its cursors resume there — {} \
                     open deadline(s) and {} in-flight record(s) named a record the move took \
                     — the halt watermark is cleared so a standing halt is answered again, and \
                     every halt this node held stands until its human writes a row here",
                    found.why(),
                    found.deadlines,
                    found.inflight
                ),
                None => stderr_line!(
                    "aterm-link: the bus log is not the one this node last read: {} (deleted, \
                     moved aside, repaired, restored, or another broker's); every offset this \
                     node held there is forgotten — the seen and topic cursors, the halt and \
                     self-ack watermarks, {} open deadline(s) no reply here can settle, and {} \
                     in-flight record(s) decided there — and every halt this node held stands \
                     until its human writes a row here",
                    found.why(),
                    found.deadlines,
                    found.inflight
                ),
            }
            let mut ev = format!(
                "log-replaced head={} was={} deadlines={} inflight={}",
                found.head, found.was_high, found.deadlines, found.inflight
            );
            if let Some(fork) = found.continues {
                ev.push_str(&format!(" continues={fork}"));
            }
            if let Some(log) = found.log {
                ev.push_str(&format!(" log={log}"));
            }
            if let Some(was) = found.was_log {
                ev.push_str(&format!(" was_log={was}"));
            }
            self.publish_ev(&ev);
        }
        if self.save_log_witness(self.log_replaced.is_some()) {
            self.log_replaced = None;
            if self.rewound_log.is_some() {
                if let Err(e) = self.state.clear_rewound_log(&self.log_witness) {
                    stderr_line!(
                        "aterm-link: could not retire the rewound log refusal: {e}; not attaching"
                    );
                    self.conn = None;
                    return false;
                }
                self.rewound_log = None;
            }
        } else if self.rewound_log.is_some() {
            self.conn = None;
            return false;
        } else if self.bus_log.is_some()
            && self.log_witness.id != self.bus_log
            && !self.attaches_past_an_unwritten_witness()
        {
            // NOR IS ANYTHING DELIVERED ON A LOG THE STATE DIR'S WITNESS DOES
            // NOT NAME (sim item S7): a re-base whose witness could not be
            // written, or a first witness — none on the dir, or one a power
            // cut tore — that could not. The endpoint was told this log; the
            // state dir names an older one, or none. A record delivered now,
            // and then a kill or a later move of the log, has the next attach
            // tell the endpoint the move from that older witness — the fork
            // lowest over the chain, at or below every offset delivered here
            // — or, with none, a bare identity, and the endpoint forgets them:
            // each one delivered again, a second row. Not attaching keeps a
            // re-base owed ([`Bridge::log_replaced`]), and the next attach
            // finishes it.
            if self.link.down_wants_report("error") {
                stderr_line!(
                    "aterm-link: the bus log's witness could not be written; not attaching \
                     until it is (a record delivered on this log meanwhile would be offered \
                     again as new)"
                );
            }
            self.link_down("error");
            self.conn = None;
            return false;
        }
        if !self.subscribe_fleet(fleet_from) {
            return false;
        }
        if let Some(from) = halt_from {
            if !self.subscribe_face(Source::Halt, from, &subject::halt_filter(&self.cfg.fleet)) {
                return false;
            }
        }
        let sids: Vec<String> = self.locals.values().cloned().collect();
        // A first attach knows no session's status, so one batch read stands in for a status
        // read per row; a re-attach whose slots all know theirs reads none.
        if sids.iter().any(|sid| {
            !self
                .per_sid
                .presence
                .get(sid)
                .is_some_and(Slot::knows_status)
        }) {
            self.presence_from_status_batch();
        }
        self.batched(|bridge| {
            for sid in sids {
                bridge.publish_session_presence(&sid, "live");
            }
        });
        self.per_sid
            .pending_admit
            .retain(|sid| self.per_sid.epochs.contains_key(sid));
        if !self.settle_in_flight() {
            self.conn = None;
            return false;
        }
        if !self.awaiting_drain.is_empty() {
            self.drain_awaited();
            if self.conn.is_none() {
                return false;
            }
        }
        if !self.refill_pending_admissions(false) {
            return false;
        }
        // This node's alone: one a bridge of a node this Mac held before left is that node's
        // group's ([`StateDir::inbox_owed`]), and stays for the node service, which makes it
        // before its move of that node's unread mail follows that group (§11 C3b).
        let handed_over = self
            .state
            .inbox_owed(&self.node)
            .filter(|_| self.attachment != Attachment::Observer);
        if let Some(owed) = handed_over {
            self.inbox_pending = Some(self.inbox_pending.map_or(owed, |have| have.max(owed)));
        }
        self.land_inbox_commit();
        if self.conn.is_none() {
            return false;
        }
        if self.inbox_pending.is_none() && handed_over.is_some() {
            if let Err(e) = self.state.clear_inbox_owed(&self.node) {
                stderr_line!("aterm-link: could not clear the inbox commit handed over: {e}");
            }
        }
        self.refresh_old_nodes();
        if !self.subscribe_inbox() {
            return false;
        }
        for sid in self.locals.values().cloned().collect::<Vec<_>>() {
            if !self.load_persisted_topics(&sid) {
                self.conn = None;
                return false;
            }
        }
        if !self.sync_say_face() {
            return false;
        }
        // Every attach looks for what a departed session holds, an earlier run's too, and
        // asks a ring that refused the lanes' read again: the attach may carry another.
        self.reclaim_owed = true;
        self.reclaim_unread = false;
        self.retire_ghost_presence();
        self.ghost_due = self.fx.now() + ghost_sweep();
        self.resample_topics();
        true
    }

    fn subscribe_fleet(&mut self, from: u64) -> bool {
        let fleet_filter = subject::fleet_filter(&self.cfg.fleet);
        self.subscribe_face(Source::Fleet, from, &fleet_filter)
    }

    fn subscribe_face(&mut self, source: Source, from: u64, filter: &str) -> bool {
        let (client, closer) = match self.connect_on_log() {
            Ok(c) => c,
            Err(e) => {
                self.dial_failed(&e);
                return false;
            }
        };
        let Ok(sub) = client.subscribe(from, filter) else {
            self.link_down("subscribe");
            return false;
        };
        self.start_reader(sub, closer, source)
    }

    fn resume_face(&mut self, source: Source, from: u64) {
        let filter = match source {
            Source::Fleet => subject::fleet_filter(&self.cfg.fleet),
            Source::Halt => subject::halt_filter(&self.cfg.fleet),
            Source::Inbox => subject::inbox_filter(&self.cfg.fleet, &self.node),
            Source::Say => subject::say_filter(&self.cfg.fleet),
            Source::Aterm => return,
        };
        if source == Source::Inbox {
            self.flush_inbox_commit();
        }
        self.close_face(source);
        if !self.subscribe_face(source, from, &filter) {
            self.conn = None;
        }
    }

    fn say_face_held(&self) -> bool {
        self.closers.iter().any(|(face, _)| *face == Source::Say)
    }

    fn sync_say_face(&mut self) -> bool {
        let wanted = self.attachment != Attachment::Observer && !self.per_sid.topics.is_empty();
        self.say_held_topics();
        match (wanted, self.say_face_held()) {
            (true, false) => self.open_say_face(),
            (false, true) => {
                self.close_face(Source::Say);
                self.mailbox.reset_source(Source::Say);
                true
            }
            _ => true,
        }
    }

    /// With the mesh on (§11 C3), name the topics this Mac's sessions hold on its node's topics
    /// face ([`crate::federate::topics_face`]) where they changed since this attach last did: the
    /// node service reads it into this Mac's presence row, and the other Macs push their topic
    /// broadcasts here while it names one. A refused write is tried at the next sync and said
    /// once until one lands.
    fn say_held_topics(&mut self) {
        if self.cfg.mesh.is_none() || self.attachment == Attachment::Observer || self.conn.is_none()
        {
            return;
        }
        let held: BTreeSet<String> = self
            .per_sid
            .topics
            .values()
            .flat_map(|topics| topics.keys().cloned())
            .collect();
        if self.topics_said.as_ref() == Some(&held) {
            return;
        }
        let body = crate::federate::topics_body(self.fx.wall_ms(), &held);
        match self.publish(&crate::federate::topics_face(&self.node), body.as_bytes()) {
            Ok(_) => {
                self.topics_said = Some(held);
                self.topics_refused = false;
            }
            Err(e) => {
                if !std::mem::replace(&mut self.topics_refused, true) {
                    stderr_line!("aterm-link: the topics this Mac holds were not said: {e}");
                }
            }
        }
    }

    fn open_say_face(&mut self) -> bool {
        let say_filter = subject::say_filter(&self.cfg.fleet);
        let Some((say_from, say_head)) = self.broadcast_floor(&say_filter) else {
            self.link_down("attach");
            return false;
        };
        self.say_drained_to = say_from;
        self.say_bus_head = say_head;
        self.subscribe_face(Source::Say, say_from, &say_filter)
    }

    /// THE `face` SUBSCRIPTION'S READ ENDED (`end`): the link goes down under the face's cut
    /// reason when the broker still answers a read, and the bridge reconnects either way. The
    /// line it prints names the end its reader saw ([`End`]): only an end of file is the
    /// broker's.
    fn subscription_lost(&mut self, source: Source, end: End) {
        let what = end.said(face_name(source));
        if self.conn.is_none() {
            stderr_line!("aterm-link: {what}; reconnecting");
            return;
        }
        let answers = self.unyielding(|bridge| {
            let filter = subject::fleet_filter(&bridge.cfg.fleet);
            let fx = bridge.fx.clone();
            let Some(conn) = bridge.conn_now() else {
                return false;
            };
            let started = fx.now();
            let head = conn.fetch(0, &filter, 0);
            bridge.observe(started, head, Some("read")).is_ok()
        });
        if answers {
            let reason = cut_reason(source);
            stderr_line!(
                "aterm-link: {what}, and the broker still answers ({reason}); reconnecting \
                 every subscription"
            );
            self.link_down(reason);
        } else {
            stderr_line!("aterm-link: {what}, and the broker does not answer; reconnecting");
        }
        self.conn = None;
    }

    fn subscription_refused(&mut self, source: Source, reason: &'static str) {
        stderr_line!(
            "aterm-link: the broker refused the {} subscription ({reason}); reconnecting",
            face_name(source)
        );
        self.link_down(reason);
        self.conn = None;
    }

    fn subscribe_inbox(&mut self) -> bool {
        let inbox_filter = subject::inbox_filter(&self.cfg.fleet, &self.node);
        let observer_from = if self.attachment == Attachment::Observer {
            let fx = self.fx.clone();
            let Some(conn) = self.conn_now() else {
                return false;
            };
            let started = fx.now();
            let answer = conn.fetch(0, &inbox_filter, 0).map(|(_, (_, head))| head);
            let Ok(head) = self.observe(started, answer, Some("read")) else {
                return false;
            };
            Some(head)
        } else {
            None
        };
        let (client, closer) = match self.connect_on_log() {
            Ok(c) => c,
            Err(e) => {
                self.dial_failed(&e);
                return false;
            }
        };
        let sub = match observer_from {
            Some(head) => client.subscribe(head, &inbox_filter),
            None => {
                let group = subject::inbox_group(&self.cfg.fleet, &self.node);
                client.subscribe_group(&group, &inbox_filter)
            }
        };
        let Ok(sub) = sub else {
            self.link_down("subscribe");
            return false;
        };
        self.start_reader(sub, closer, Source::Inbox)
    }

    fn start_reader(
        &mut self,
        sub: astream_broker::AnySubscription,
        closer: BusCloser,
        source: Source,
    ) -> bool {
        let _ = closer.set_read_timeout(None);
        let closer = Arc::new(closer);
        let reader = self.fx.start_reader(
            sub,
            Arc::clone(&closer),
            live_deadline(),
            source,
            self.mailbox.clone(),
        );
        if let Err(e) = reader {
            stderr_line!("aterm-link: the {source:?} reader thread could not start: {e}");
            closer.close();
            self.link_down("subscribe");
            return false;
        }
        self.closers.push((source, closer));
        true
    }

    fn resample_topics(&mut self) {
        self.per_sid.topics_sampled.clear();
        self.sample_unsampled_topics();
        self.replay_topics(false);
    }

    fn sample_unsampled_topics(&mut self) {
        let fresh: Vec<String> = self
            .locals
            .values()
            .filter(|sid| !self.per_sid.topics_sampled.contains(*sid))
            .cloned()
            .collect();
        for sid in fresh {
            if self.sample_topics(&sid) {
                self.per_sid.topics_sampled.insert(sid);
            }
        }
    }

    fn note_watch_ack(&mut self, local: u64, sid: &str) {
        self.per_sid.watched.insert(sid.to_string(), local);
        self.per_sid.topics_sampled.remove(sid);
    }

    fn park_detached(&mut self, backoff: Duration) -> bool {
        self.close_subscriptions();
        let mailbox = Arc::clone(&self.mailbox);
        let fx = self.fx.clone();
        wait_for_retry_or_aterm_close(&fx, &mailbox, backoff, |line| {
            if let Some((local, sid)) = watch_ack(line) {
                self.note_watch_ack(local, sid);
            }
            if let Some(bye) = bye_of(line) {
                self.take_bye(bye);
            }
        })
    }

    fn close_subscriptions(&mut self) {
        self.land_inbox_commit();
        for (_, closer) in self.closers.drain(..) {
            closer.close();
        }
        self.presence_view.close();
        self.presence_view_retry_at = None;
    }

    fn set_aside_torn_state(&mut self) -> bool {
        #[cfg(test)]
        if self
            .fx
            .mutant(crate::sim::world::Mutant::RefuseTornAfterPowerCut)
        {
            return true;
        }
        let swept = self.state.set_aside_torn();
        match swept {
            Ok(moved) => {
                for m in &moved {
                    stderr_line!(
                        "aterm-link: {} did not read whole after the last bridge stopped \
                         uncleanly in an earlier boot (a power cut tears what was not yet on \
                         disk); moved aside to {} in the state dir — it served sessions that \
                         boot ended, and nothing reads it again",
                        m.from,
                        m.to
                    );
                }
                self.set_aside += moved.len() as u64;
                true
            }
            Err(e) if crate::state::a_clean_disk_heals(&e) => {
                stderr_line!(
                    "aterm-link: the files a power cut may have torn could not all be set \
                     aside: {e}; tried again before the run record is written"
                );
                false
            }
            Err(e) => {
                stderr_line!(
                    "aterm-link: the files a power cut may have torn could not all be set \
                     aside: {e}; what is left is refused where it is read, until a person \
                     repairs the state dir"
                );
                true
            }
        }
    }

    fn retire_in_flight_state(&mut self) -> bool {
        #[cfg(test)]
        if self
            .fx
            .mutant(crate::sim::world::Mutant::RepublishInFlightOfAnEarlierBoot)
        {
            return true;
        }
        match self.state.retire_in_flight_of_an_earlier_boot() {
            Ok(retired) => {
                for m in &retired {
                    stderr_line!(
                        "aterm-link: {} was in flight when the last bridge stopped uncleanly in \
                         an earlier boot; retired to {} in the state dir and never published \
                         again — it served sessions that boot ended, and a power cut may have \
                         brought it back after its record landed",
                        m.from,
                        m.to
                    );
                }
                true
            }
            Err(e) if crate::state::a_clean_disk_heals(&e) => {
                stderr_line!(
                    "aterm-link: the in-flight entries of an earlier boot could not all be \
                     retired: {e}; tried again before the run record is written"
                );
                false
            }
            Err(e) => {
                stderr_line!(
                    "aterm-link: the in-flight entries of an earlier boot could not all be \
                     retired: {e}; what is left is published again at the attach"
                );
                true
            }
        }
    }

    fn reconcile_logpin_state(&mut self) -> bool {
        #[cfg(test)]
        let rules = crate::state::PinRules {
            set_aside_torn: !self.fx.mutant(crate::sim::world::Mutant::KeepTornPin),
            total_from_floor: !self
                .fx
                .mutant(crate::sim::world::Mutant::TotalInThePinAlone),
            need_key: !self.fx.mutant(crate::sim::world::Mutant::KeyInThePinAlone),
        };
        #[cfg(test)]
        let done = if rules == crate::state::PinRules::ALL {
            self.state.reconcile_logpin()
        } else {
            self.state.reconcile_logpin_with(rules)
        };
        #[cfg(not(test))]
        let done = self.state.reconcile_logpin();
        let owed = done.as_ref().is_err_and(crate::state::a_clean_disk_heals);
        if done.is_ok() {
            self.pin_unreconciled = None;
        }
        match done {
            Ok(crate::state::Reconciled::Kept) => {}
            Ok(crate::state::Reconciled::Raised { from, to }) => stderr_line!(
                "aterm-link: mirror: the last bridge stopped uncleanly in an earlier boot, and \
                 nothing tells logpin from an older version a power cut brought back, so its \
                 total ({from} bytes) is raised to its floor ({to}): what the floor leased \
                 ahead, at most {} bytes and the batch in flight, is counted as mirrored",
                crate::mirror::FLOOR_LEASE_BYTES
            ),
            Ok(crate::state::Reconciled::SetAside(m)) => {
                stderr_line!(
                    "aterm-link: {} did not read whole after the last bridge stopped uncleanly \
                     in an earlier boot (a power cut tears what was not yet on disk); moved \
                     aside to {} in the state dir, and a whole logpin written from logkey and \
                     logfloor in its place — the mirror resumes under the same alias key, its \
                     total at the floor",
                    m.from,
                    m.to
                );
                self.set_aside += 1;
            }
            Ok(crate::state::Reconciled::Restored) => stderr_line!(
                "aterm-link: mirror: no logpin after the last bridge stopped uncleanly in an \
                 earlier boot; a whole one written from logkey and logfloor — the same alias \
                 key, its total at the floor"
            ),
            Err(e) => {
                let then = if owed {
                    "tried again before the run record is written; the mirror judges no pin \
                     until it is"
                } else {
                    "a pin that does not read stops the mirror, as before, and the stop names \
                     the file to repair"
                };
                stderr_line!(
                    "aterm-link: the mirror's pin could not be reconciled after an unclean stop: \
                     {e}; {then}"
                );
                self.pin_unreconciled = Some(e.to_string());
            }
        }
        !owed
    }

    fn recover(&mut self) -> bool {
        #[cfg(test)]
        self.state
            .copy_on_every_try(self.fx.mutant(crate::sim::world::Mutant::CopyOnEveryTry));
        #[cfg(test)]
        self.state.one_quarantine_memory_per_source(
            self.fx
                .mutant(crate::sim::world::Mutant::OneQuarantineMemoryPerSource),
        );
        let swept = self.set_aside_torn_state();
        let retired = self.retire_in_flight_state();
        let reconciled = self.reconcile_logpin_state();
        #[cfg(test)]
        if self
            .fx
            .mutant(crate::sim::world::Mutant::BeginRunPastAFailedRecovery)
        {
            return true;
        }
        let owed = !(swept && retired && reconciled)
            && matches!(self.state.torn_files_possible(), Ok(true));
        if owed && !self.recovery_owed {
            stderr_line!(
                "aterm-link: the recovery the last bridge's unclean stop in an earlier boot owes \
                 is not complete: the run record is not written, no attach goes on and the \
                 mirror judges no pin until it is; each attach tries it again"
            );
        }
        self.recovery_owed = owed;
        !owed
    }

    fn begin_run(&self) {
        let began = self.fx.now();
        match self.state.begin_run() {
            Ok(flushed) if flushed.full + flushed.ordered > 0 => stderr_line!(
                "aterm-link: the run record says a bridge runs in this boot: {} barriers and {} \
                 full flushes in {} us{}",
                flushed.ordered,
                flushed.full,
                self.fx.now().saturating_duration_since(began).as_micros(),
                aterm_durable::durable_flushes_note()
            ),
            Ok(flushed) if flushed.proven => stderr_line!(
                "aterm-link: the run record says a bridge runs in this boot, on the media as its \
                 proof ({}) shows: nothing written, no flush, in {} us",
                crate::state::RUN_PROOF,
                self.fx.now().saturating_duration_since(began).as_micros()
            ),
            Ok(_) => {}
            Err(e) => stderr_line!(
                "aterm-link: the state dir's run record could not be written: {e}; no marker \
                 is removed to force legacy recovery; unavailable recovery stays fail-closed"
            ),
        }
    }

    fn run_names_this_boot(&mut self) -> bool {
        #[cfg(test)]
        if self.attach_past_an_earlier_boots_run {
            return true;
        }
        if matches!(self.state.torn_files_possible(), Ok(true)) {
            let owed = self.recovery_owed;
            #[cfg(test)]
            let old_wait = self
                .fx
                .mutant(crate::sim::world::Mutant::BeginRunPastAFailedRecovery);
            #[cfg(not(test))]
            let old_wait = false;
            if !old_wait && !self.recover() {
                if self.link.down_wants_report("error") {
                    stderr_line!(
                        "aterm-link: the recovery an unclean stop in an earlier boot owes could \
                         not be completed; not attaching until it is (a torn file left now \
                         would be refused at every attach of this boot)"
                    );
                }
                self.link_down("error");
                return false;
            }
            if owed {
                self.tell_set_aside();
                self.tell_quarantine();
                self.mirror_after_recovery();
            }
        }
        match self.state.begin_run() {
            Ok(_) => true,
            Err(e) => {
                if self.link.down_wants_report("error") {
                    stderr_line!(
                        "aterm-link: the state dir's OPEN run record could not be admitted: \
                         {e}; not attaching until its required publication succeeds"
                    );
                }
                self.link_down("error");
                false
            }
        }
    }

    fn end_run(&self) {
        self.hand_asks_to_files();
        if self.bye == Some(Bye::Handoff) {
            self.hand_posts_to_files();
            stderr_line!(
                "aterm-link: handed off: the run goes on in the successor's bridge, so nothing \
                 is flushed and the run record stays open"
            );
            return;
        }
        #[cfg(test)]
        let owed = self.recovery_owed
            && !self
                .fx
                .mutant(crate::sim::world::Mutant::CleanStopOverAnOwedRecovery);
        #[cfg(not(test))]
        let owed = self.recovery_owed;
        if owed {
            stderr_line!(
                "aterm-link: no clean stop: the recovery the last bridge's unclean stop in an \
                 earlier boot owes is not complete, so the run record stays as it is and the \
                 next start does it"
            );
            return;
        }
        if !self.hand_posts_to_files() {
            stderr_line!(
                "aterm-link: no clean stop: the post journal holds records an older build would \
                 not find, so the run record stays open and the next bridge of this build reads \
                 them from the journal"
            );
            return;
        }
        let began = self.fx.now();
        match self.state.end_run_clean() {
            Ok(flushed) => stderr_line!(
                "aterm-link: the run stopped cleanly: {} state files and {} directories \
                 flushed, then the run record: {} barriers and {} full flushes in {} us{}",
                flushed.files,
                flushed.dirs,
                flushed.ordered,
                flushed.full,
                self.fx.now().saturating_duration_since(began).as_micros(),
                aterm_durable::durable_flushes_note()
            ),
            Err(e) => stderr_line!(
                "aterm-link: this run's state files could not all be flushed: {e}; no marker \
                 is removed to force legacy recovery; unavailable recovery stays fail-closed"
            ),
        }
    }

    /// At a stop, the asks journal's tables written as the files an older build reads
    /// ([`StateDir::hand_asks_to_files`]), for whichever bridge runs next. A refusal is said and
    /// stops nothing: a bridge of this build reads the journal, and a rollback reads the tables
    /// the last hand-off that took wrote.
    fn hand_asks_to_files(&self) {
        if let Err(e) = self.state.hand_asks_to_files() {
            stderr_line!(
                "aterm-link: the asks journal's tables could not be written as the files a \
                 bridge of an older build reads: {e}; a bridge of this build reads the journal"
            );
        }
    }

    /// At a stop, the post journal's records written as the files an older build reads
    /// ([`StateDir::hand_posts_to_files`]), for whichever bridge runs next. Whether they were.
    fn hand_posts_to_files(&self) -> bool {
        #[cfg(test)]
        self.state.hand_off_without_heads(
            self.fx
                .mutant(crate::sim::world::Mutant::HandOffWithoutTheHeads),
        );
        match self.state.hand_posts_to_files() {
            Ok(0) => true,
            Ok(n) => {
                stderr_line!(
                    "aterm-link: {n} record(s) of the post journal written as the files a bridge \
                     of an older build reads, for whichever bridge runs next"
                );
                true
            }
            Err(e) => {
                stderr_line!(
                    "aterm-link: the post journal's records could not all be written as the \
                     files a bridge of an older build reads: {e}; a bridge of this build reads \
                     every one from the journal"
                );
                false
            }
        }
    }

    fn tell_set_aside(&mut self) {
        let files = std::mem::take(&mut self.set_aside);
        #[cfg(test)]
        {
            self.moved_at_start = files;
        }
        if files == 0
            || self.attachment != Attachment::Inherited
            || self.endpoint_lane < LINK_SET_ASIDE_LANE
        {
            return;
        }
        match self.ctl_request(&format!("link set-aside files={files}")) {
            Ok(reply) if reply.ok() => {}
            Ok(reply) => stderr_line!(
                "aterm-link: aterm did not take `link set-aside files={files}`: {}",
                reply.header()
            ),
            Err(e) => stderr_line!("aterm-link: `link set-aside` failed: {e}"),
        }
    }

    fn tell_quarantine(&mut self) {
        if self.attachment != Attachment::Inherited || self.endpoint_lane < LINK_QUARANTINE_LANE {
            return;
        }
        let held = match self.state.quarantined_torn() {
            Ok(held) => held.unwrap_or(0),
            Err(e) => {
                if !self.quarantine_unread {
                    stderr_line!(
                        "aterm-link: the state dir's quarantine could not be read ({e}); aterm \
                         is not told whether the torn files set aside are still there"
                    );
                }
                self.quarantine_unread = true;
                return;
            }
        };
        self.quarantine_unread = false;
        #[cfg(test)]
        let held = if self.fx.mutant(crate::sim::world::Mutant::TellMovedNotHeld) {
            self.moved_at_start
        } else {
            held
        };
        if self.quarantine_said == Some(held) {
            return;
        }
        match self.ctl_request(&format!("link quarantine torn={held}")) {
            Ok(reply) if reply.ok() => {}
            Ok(reply) => stderr_line!(
                "aterm-link: aterm did not take `link quarantine torn={held}`: {}",
                reply.header()
            ),
            Err(e) => stderr_line!("aterm-link: `link quarantine` failed: {e}"),
        }
        self.quarantine_said = Some(held);
    }

    fn watches_quarantine(&self) -> bool {
        self.quarantine_said.is_some_and(|held| held > 0)
    }

    fn introduce(&mut self) {
        if self.attachment != Attachment::Inherited {
            return;
        }
        let own = aterm_stamp::build_id();
        let line = format!(
            "hello lane={} build={own} {HELLO_REFILL}{}",
            aterm_types::control_verbs::BRIDGE_LANE,
            self.mirror_hello_token()
        );
        let reply = match self.ctl_request(&line) {
            Ok(reply) => reply,
            Err(e) => {
                stderr_line!("aterm-link: `hello` failed: {e}");
                return;
            }
        };
        let header = reply.header();
        if !reply.ok() {
            self.endpoint_lane = 0;
            self.endpoint_build = None;
            if !header.starts_with("ERR unknown verb") {
                stderr_line!("aterm-link: aterm refused `hello` ({header})");
            }
            stderr_line!(
                "aterm-link: treating aterm as lane 0 (no lane handshake); this bridge is {own}, \
                 lane {}",
                aterm_types::control_verbs::BRIDGE_LANE
            );
            return;
        }
        let field = |key: &str| {
            header
                .split_whitespace()
                .find_map(|tok| tok.strip_prefix(key)?.strip_prefix('='))
        };
        self.endpoint_lane = field("lane").and_then(crate::parse_uint).unwrap_or(0);
        self.endpoint_build = field("build").map(str::to_string);
        let header = header.to_string();
        self.mirror_hello_answered(&header);
        if self.endpoint_build.as_deref() != Some(own.as_str())
            || self.endpoint_lane != aterm_types::control_verbs::BRIDGE_LANE
        {
            stderr_line!(
                "aterm-link: this bridge is {own} (lane {}) under aterm {} (lane {}): \
                 [fabric] command names another build than this instance's; the two speak \
                 lane {}",
                aterm_types::control_verbs::BRIDGE_LANE,
                self.endpoint_build.as_deref().unwrap_or("-"),
                self.endpoint_lane,
                self.endpoint_lane
                    .min(aterm_types::control_verbs::BRIDGE_LANE)
            );
        }
    }

    fn first_roster(&mut self) -> bool {
        match self.refresh_sessions() {
            Err(e) if e.kind() == io::ErrorKind::Unsupported => {
                stderr_line!(
                    "aterm-link: {e}: this aterm does not list its sessions in the grammar this \
                     bridge reads; attaching nothing until it goes away"
                );
                self.link_down("endpoint-grammar");
                false
            }
            Err(_) => true,
            Ok(_) => {
                if std::mem::take(&mut self.earlier_boot) {
                    self.sweep_sessions_of_an_earlier_boot();
                }
                true
            }
        }
    }

    /// Unlink the files an earlier boot's sessions left in the state dir and retire their post
    /// reservations, keeping those of every session the roster just read
    /// ([`StateDir::sweep_sessions_of_an_earlier_boot`]), so the subjects they named go back
    /// to the budget at the reclaim's next due pass ([`Bridge::reclaim_subjects`]); once, at
    /// the start, after its recovery tried the sweep of torn files and before the attach
    /// publishes again what is in flight. A roster that did not read sweeps nothing, and what
    /// is left goes at the next boot's start.
    fn sweep_sessions_of_an_earlier_boot(&mut self) {
        let live: BTreeSet<String> = self.locals.values().cloned().collect();
        #[cfg(test)]
        self.state.keep_earlier_boot_reservations(
            self.fx
                .mutant(crate::sim::world::Mutant::KeepEarlierBootReservations),
        );
        match self.state.sweep_sessions_of_an_earlier_boot(&live) {
            Ok(0) => {}
            Ok(n) => stderr_line!(
                "aterm-link: the last bridge ran in an earlier boot: {n} files and post \
                 reservations of the sessions that boot ended (keys/, acks/, unhandled/, seen/, \
                 topics/ and the post journal's reservations) removed from the state dir"
            ),
            Err(e) => stderr_line!(
                "aterm-link: the files and post reservations of the sessions an earlier boot \
                 ended could not all be removed from the state dir: {e}; what is left goes at \
                 the next boot's start"
            ),
        }
    }

    fn close_face(&mut self, source: Source) {
        self.closers.retain(|(face, closer)| {
            if *face == source {
                closer.close();
            }
            *face != source
        });
    }

    /// Runs the bridge event loop until the `aterm` endpoint disconnects.
    pub fn run(mut self) -> io::Result<()> {
        if let Some(ended) = self.start() {
            return ended;
        }
        let mut backoff = Backoff::default();
        loop {
            if let Some(ended) = self.pass(&mut backoff) {
                return ended;
            }
        }
    }

    /// Performs pre-loop setup (push reader, handshake, initial roster, and deadline arming).
    pub(crate) fn start(&mut self) -> Option<io::Result<()>> {
        // The push lane: `subscribe @* events,sessions` on the SECOND inherited
        // descriptor. It is a separate connection because a `subscribe` holds its
        // stream forever, and the verb connection must stay answerable. Adopted
        // in the constructor; a bridge the probe made an observer keeps holding
        // it, unread, exactly as it held the bare number before.
        if self.attachment == Attachment::Inherited {
            if let Some(push) = self.push.take() {
                spawn_event_reader(push, self.mailbox.clone());
            }
        }
        self.earlier_boot = self.state.last_ran_in_an_earlier_boot();
        if self.recover() {
            self.begin_run();
        }
        self.introduce();
        self.tell_set_aside();
        self.tell_quarantine();
        if !self.first_roster() {
            while !self.park_detached(UNSERVED_PARK) {}
            stderr_line!("aterm-link: aterm closed the bridge connection; exiting");
            self.end_run();
            return Some(Ok(()));
        }
        self.arm_deadline_sweep(true, false);
        None
    }

    /// Executes a single iteration of the bridge loop ([`Bridge::run`]).
    pub(crate) fn pass(&mut self, backoff: &mut Backoff) -> Option<io::Result<()>> {
        // A BRIDGE WHOSE ATERM IS GONE DOES NOTHING MORE ([`Bridge::aterm_gone`]):
        // no redial, no roster round, no sweep. The closure is taken at the
        // `take` below, but a round that ended with the link dropped — a
        // roster read or a refill that met the lost lane drops it on
        // purpose — would otherwise redial FIRST: a fresh incarnation in
        // the shared `inc`, a will and a `live` row for an aterm that is
        // not there, while the successor's bridge waits on the lock.
        if self.aterm_gone() {
            self.on_aterm_closed();
            stderr_line!("aterm-link: aterm closed the bridge connection; exiting");
            self.wind_down();
            return Some(Ok(()));
        }
        if self.conn.is_none() {
            // THE OWNER EMPTYING THE QUARANTINE IS LOOKED FOR WITH NO BROKER
            // TOO, once a redial while it holds what aterm was told of: the
            // look asks aterm, not the broker, and the round's backstop below
            // is never reached while an attach fails. So the entry ends
            // within a redial's back-off ([`crate::transport::RECONNECT_MAX`])
            // of the owner emptying it while the bus is away.
            if self.watches_quarantine() {
                self.tell_quarantine();
            }
            // A redial reads the ring anyway, so a ring widened under this bridge is said here.
            if let Some(wait) = backoff.lost_with(self.fx.now(), self.fx.jitter()) {
                if self.park_detached(wait) {
                    self.on_aterm_closed();
                    stderr_line!("aterm-link: aterm closed the bridge connection; exiting");
                    self.wind_down();
                    return Some(Ok(()));
                }
            }
            if self.attach_broker() {
                backoff.attached(self.fx.now());
                self.withdraw();
                self.drain_outbox();
                self.mirror_wake();
            } else {
                // A REFUSAL IS NOT A RETRY: the node is another bridge's
                // (`Bridge::refused`), and asking again later would only
                // run as it once that bridge goes.
                if let Some(e) = self.refused.take() {
                    return Some(Err(e));
                }
                self.conn = None;
                let wait = backoff.failed_with(self.fx.jitter());
                if self.park_detached(wait) {
                    self.on_aterm_closed();
                    stderr_line!("aterm-link: aterm closed the bridge connection; exiting");
                    self.wind_down();
                    return Some(Ok(()));
                }
                return None;
            }
        }
        self.after_holds();
        if self.fx.now() >= self.roster_due {
            self.roster_backstop();
            self.roster_due = self.fx.now() + roster_refresh();
            self.arm_deadline_sweep(true, false);
            self.mirror_wake();
            if self.watches_quarantine() {
                self.tell_quarantine();
            }
        }
        let sweep_now = self.fault == Fault::SweepGhostsAtOnceWhileMarked
            && self.state.root().join("sweep-ghosts-now").exists();
        if sweep_now || (self.sweep_owed() && self.fx.now() >= self.ghost_due) {
            self.retire_ghost_presence();
            self.ghost_due = self.fx.now() + ghost_sweep();
        }
        let expire_now = self.fault == Fault::ExpireDeadlinesAtOnceWhileMarked
            && self.state.root().join("expire-deadlines-now").exists();
        if !self.deadlines.is_empty() && (expire_now || self.fx.now() >= self.deadline_due) {
            self.expire_deadlines(expire_now);
            self.arm_deadline_sweep(true, true);
        }
        if self.conn.is_some() && !self.sync_say_face() {
            self.conn = None;
        }
        self.conn.as_ref()?;
        if !self.mirror_then_round() {
            return Some(Ok(()));
        }
        None
    }

    fn mirror_then_round(&mut self) -> bool {
        if !self.mirror_engaged() {
            let wait = self.duty_wait(self.fx.now());
            return self.round(wait);
        }
        self.mirror_pass_then_settle();
        let wait = self.park_wait(self.fx.now());
        self.park_and_take(wait)
    }

    /// A ROUND'S MIRROR PASS, THEN ITS SETTLE, THE INBOX COMMIT HELD FROM THE ROUND'S START
    /// (critic item E04's review): the pass runs ahead of the settle, so a commit the delivery
    /// before it owes is due when the pass begins, and the settle holds it from then
    /// ([`Bridge::hold_inbox_commit`]). A pass that takes makes a `log-take` ctl round trip, and
    /// a take can come back empty (a snapshot the endpoint holds back to its rate, the take
    /// behind a delivery that E04's hold put there, [`Bridge::mirror_rides_nothing`]); dated
    /// from the round's start, the commit's [`IDLE_ACK_GRACE`] runs as it would with no take in
    /// the round, the round trip inside it rather than ahead of it. A take that comes back with
    /// slots carries the commit in its burst ([`Bridge::take_commit_for_the_mirror`]). Ahead of
    /// the pass, the pin's settle's answer is read if it is in ([`Bridge::read_the_pins_settle`]).
    fn mirror_pass_then_settle(&mut self) {
        let began = self.fx.now();
        self.read_the_pins_settle();
        if !self.mirror_yields() {
            self.mirror_step();
        }
        #[cfg(test)]
        let began = if self
            .fx
            .mutant(crate::sim::world::Mutant::CommitGraceFromTheSettle)
        {
            self.fx.now()
        } else {
            began
        };
        self.settle_owed_since(began);
    }

    fn sweep_owed(&self) -> bool {
        !self.ghosts.is_empty()
            || !self.exits_owed.is_empty()
            || self.reclaim_owed
            || (self.reclaim_looks() && (self.reclaim_remote || !self.reclaim.is_empty()))
    }

    /// Whether the reclaim may give anything back on this link: not after this connection's
    /// ring refused the lanes' read for good, nor on a log whose retention cut refuses every
    /// retire ([`Bridge::reclaim_subjects`]).
    fn reclaim_looks(&self) -> bool {
        let log = self.conn.as_ref().and_then(|c| c.seen_log_identity());
        !self.reclaim_unread && (log.is_none() || self.reclaim_cut != log)
    }

    /// A HOLD BEHIND THE BROKER'S RATE LIMIT ENDED ([`Bridge::rate`]): the drain sends again what
    /// it held, each sender's posts in its order, a refused one first at its pin, the fence its
    /// refusal set lifted ahead of it. They go out pipelined, as every drain's posts do, so the
    /// bucket that refilled for the first may refuse those behind it in the same window, each
    /// kept at its pin for the next hold's end: one end can meet up to a window of refusals.
    /// A HOLD BEHIND AN UNVERIFIABLE RESERVATION ENDED ([`Bridge::unverified`], critic item
    /// F21): the drain tries the reservation again, then its sender's later posts or receipts
    /// behind it, in their order.
    fn after_holds(&mut self) {
        let now = self.fx.now();
        let rate = self.rate.expire(now);
        let unverified = self.unverified.expire(now);
        if rate || unverified {
            self.drain_outbox();
        }
    }

    fn duty_wait(&self, now: Instant) -> Duration {
        let wait = idle_wait(
            now,
            self.roster_due,
            (!self.deadlines.is_empty()).then_some(self.deadline_due),
            self.sweep_owed().then_some(self.ghost_due),
        );
        // No later than a hold behind the broker's rate limit ends ([`Bridge::rate`]), or one
        // behind an unverifiable reservation ([`Bridge::unverified`]).
        self.rate
            .next_end(now)
            .into_iter()
            .chain(self.unverified.next_end(now))
            .min()
            .map_or(wait, |end| wait.min(end.saturating_duration_since(now)))
    }

    fn park_wait(&self, now: Instant) -> Duration {
        let wait = self.duty_wait(now);
        self.mirror_due(now)
            .map_or(wait, |due| wait.min(due.saturating_duration_since(now)))
    }

    fn round(&mut self, wait: Duration) -> bool {
        self.settle_owed();
        self.park_and_take(wait)
    }

    /// SEND AGAIN THE `ev` RECORDS A GAP FENCE REFUSED ([`Bridge::evs_fenced`]), once the fence
    /// is lifted. The round waits on no answer for them: it reads those already in, lifts the
    /// fence only once none is owed ([`Bridge::lift_gap_fences`]), and while the link is still
    /// fenced, or there is none, or the broker's rate limit holds this producer's publishes
    /// ([`Bridge::rate`]), they wait for a later round. Each goes at a fresh sequence, as it would
    /// have, and one a fence or the limit refuses again waits again.
    fn resend_fenced_evs(&mut self) {
        if self.evs_fenced.is_empty() || self.conn.is_none() || self.rate_held().is_some() {
            return;
        }
        self.try_settle_acks();
        if self.conn.as_ref().is_some_and(|link| link.unacked() == 0) {
            self.lift_gap_fences();
        }
        if !self.conn.as_ref().is_some_and(|link| !link.fenced()) {
            return;
        }
        for (sid, payload) in std::mem::take(&mut self.evs_fenced) {
            self.publish_ev_for(sid.as_deref(), &payload);
        }
    }

    fn settle_owed(&mut self) {
        self.settle_owed_since(self.fx.now());
    }

    /// The round's settle, holding an inbox commit no round holds yet from `began`, the
    /// round's start ([`Bridge::mirror_pass_then_settle`]).
    fn settle_owed_since(&mut self, began: Instant) {
        self.resend_fenced_evs();
        self.resend_held_notices();
        if !self.mailbox.inbox_queued() && !self.hold_inbox_commit(began) {
            self.flush_inbox_commit();
        }
        if self.mailbox.is_idle() {
            #[cfg(test)]
            let post_to_backstop = self
                .fx
                .mutant(crate::sim::world::Mutant::IdlePostLeftToBackstop);
            #[cfg(not(test))]
            let post_to_backstop = false;
            let waited = |then: &Then| {
                then.waited_when_idle() && !(post_to_backstop && then.post().is_some())
            };
            let waiting = self.fx.now();
            self.grouped(|bridge| {
                while bridge
                    .conn
                    .as_ref()
                    .is_some_and(|link| link.owed().any(waited))
                    && bridge.settle_one(waiting)
                {}
            });
            self.try_settle_acks();
            self.tally_idle();
        }
    }

    fn tally_idle(&mut self) {
        let (mailbox, state) = (&self.mailbox, &self.state);
        self.waits.idle(self.committed.unwrap_or(0), |t| {
            mailbox.copy_wakes(t);
            state.copy_dir_rows(t);
        });
    }

    fn park_and_take(&mut self, wait: Duration) -> bool {
        let acks_owed = self.conn.as_ref().is_some_and(|link| link.unacked() > 0);
        let now = self.fx.now();
        let held_left = self
            .commit_held
            .map(|since| IDLE_ACK_GRACE.saturating_sub(now.saturating_duration_since(since)));
        let mut park = if acks_owed {
            wait.min(IDLE_ACK_GRACE)
        } else {
            wait
        };
        if let Some(left) = held_left {
            park = park.min(left);
        }
        let owed = acks_owed || held_left.is_some();
        // A park the bus mirror's pass cut short of the grace leaves the held commit for that
        // pass, whose burst carries it (critic item C13, [`Bridge::take_commit_for_the_mirror`]);
        // the next round's settle still sends it alone once the grace is out
        // ([`Bridge::hold_inbox_commit`]). Any other park that takes nothing sends it.
        let for_the_mirror = held_left.is_some_and(|left| park < left)
            && self
                .mirror_due(now)
                .is_some_and(|at| at.saturating_duration_since(now) <= park);
        #[cfg(test)]
        let for_the_mirror = for_the_mirror
            && !self
                .fx
                .mutant(crate::sim::world::Mutant::ShortParkSendsTheHeldCommit);
        let (go_on, took) = self.take_one(park);
        if go_on && !took && owed {
            if self.commit_held.is_some() && !for_the_mirror {
                self.flush_inbox_commit();
            }
            // THE PIN'S SETTLE IS NOT WAITED ON (critic item F07): its answer comes once the
            // broker's full flush returns, which the loop leaves to the broker; each round's pass
            // reads it once it is in ([`Bridge::read_the_pins_settle`]), and an idle loop's park
            // backstop no later than the grace.
            if self
                .conn
                .as_ref()
                .is_some_and(|link| link.owed().all(Then::is_settle))
            {
                self.try_settle_acks();
            } else {
                self.settle_acks();
            }
            self.tally_idle();
        }
        go_on
    }

    fn take_one(&mut self, wait: Duration) -> (bool, bool) {
        #[cfg(test)]
        {
            self.parked = Some(wait);
        }
        let parked = self.waits.starts();
        let item = self.fx.take(&self.mailbox, wait);
        if parked.is_some() {
            self.waits.note(crate::wait_tally::Row::Park, parked);
            self.taken_at = self.waits.starts();
        }
        if self.commit_held.is_some() && item.as_ref().is_some_and(off_the_bus) {
            self.flush_inbox_commit();
        }
        let took = item.is_some();
        self.in_round = took;
        let go_on = match item {
            Some(Item::Fleet(r)) => {
                self.on_fleet_record(&r);
                true
            }
            Some(Item::Halt(r)) => {
                self.on_halt_record(&r);
                true
            }
            Some(Item::Event(line)) => {
                self.on_event(&line);
                true
            }
            Some(Item::Inbox(r, origin)) => {
                self.mirror_took_inbox();
                self.on_inbox_run(&r, origin);
                true
            }
            Some(Item::Say(r)) => {
                self.on_say_record(&r);
                true
            }
            Some(Item::Closed(_)) => {
                self.in_round = false;
                self.on_aterm_closed();
                stderr_line!("aterm-link: aterm closed the bridge connection; exiting");
                self.wind_down();
                false
            }
            Some(Item::Lost(source, end)) => {
                self.subscription_lost(source, end);
                true
            }
            Some(Item::Refused(source, reason)) => {
                self.subscription_refused(source, reason);
                true
            }
            Some(Item::Resume(source, from)) => {
                self.resume_face(source, from);
                true
            }
            Some(Item::Gap(source, g)) => {
                self.on_gap(source, g);
                true
            }
            None => true,
        };
        self.in_round = false;
        (go_on, took)
    }
}

#[cfg(test)]
impl Bridge {
    /// Enables writing broker call tallies to `path`.
    pub(crate) fn tally_to(&mut self, path: &std::path::Path) {
        self.waits = Recorder::at(path).expect("a fresh tally file");
        self.mailbox.stamp(true);
        self.state.time_ops(true);
    }
}

#[cfg(test)]
impl Bridge {
    /// Returns a shared handle to the bridge mailbox for queuing endpoint `EVENT` lines.
    pub(crate) fn mailbox_handle(&self) -> Arc<Mailbox> {
        Arc::clone(&self.mailbox)
    }

    /// Overrides the boot identifier and mutant selector for state-dir incarnation lease tests.
    pub(crate) fn simulate_boot(&self, boot: [u8; 16], mutant: u8) {
        self.state.set_inc_seam(crate::state::IncSeam {
            boot: Some(Some(boot)),
            kill_at: None,
            mutant,
            span: None,
            off_volume: false,
        });
    }

    /// The state dir stands off the boot volume in the boot [`Bridge::simulate_boot`] set, and
    /// with `before_rz2` its leases name no boot there, as an older build's did
    /// ([`crate::sim::world::Mutant::LeaseNamesNoBootOffTheBootVolume`]).
    pub(crate) fn simulate_off_the_boot_volume(&self, before_rz2: bool) {
        self.state.stand_off_the_boot_volume();
        self.state
            .lease_names_no_boot_off_the_boot_volume(before_rz2);
    }

    /// Returns the uncommitted inbox offset and log identity currently owed by this bridge.
    pub(crate) fn owed_inbox_commit(&self) -> Option<(u64, Option<astream_broker::LogIdentity>)> {
        self.inbox_pending.map(|upto| (upto, self.bus_log))
    }

    /// Whether this bridge holds its inbox commit for its next write
    /// ([`Bridge::hold_inbox_commit`]).
    pub(crate) fn holds_inbox_commit(&self) -> bool {
        self.commit_held.is_some()
    }

    /// Disables broker log-lineage verification on attach for negative-control testing.
    pub(crate) fn simulate_ignore_lineage(&mut self) {
        self.ignore_lineage = true;
    }
}

impl Drop for Bridge {
    fn drop(&mut self) {
        self.close_subscriptions();
    }
}

#[derive(Debug)]
struct Answered(String);

impl std::fmt::Display for Answered {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Answered {}

/// A RESERVATION'S ANSWER ITS READ BACK CANNOT VOUCH FOR ([`Bridge::unverified_publication`],
/// critic item F21): read, so its fate is no transport's ([`Bridge::fate_unknown`]), and a
/// verdict that holds its sender's lane, not the link ([`Bridge::hold_unverifiable`]).
#[derive(Debug)]
struct Unverifiable(String);

impl std::fmt::Display for Unverifiable {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Unverifiable {}

/// Whether `e` is a reservation's answer its read back cannot vouch for ([`Unverifiable`]).
fn unverifiable(e: &io::Error) -> bool {
    e.get_ref().is_some_and(|inner| inner.is::<Unverifiable>())
}

/// Whether `body` is the words `pin`'s attempt published, its `t=` aside: a pin from a build
/// before publication identities names none, so any words are.
fn reconstructs(pin: &PublishPin, body: &[u8]) -> bool {
    pin.identity
        .as_ref()
        .is_none_or(|attempt| publication_identity(&attempt.subject, body) == *attempt)
}

/// A POST ITS SEND CONCLUDED BEFORE THE LINK TOOK IT ([`Bridge::send_pipelined`]: its aterm
/// gone or its sender held, [`Bridge::unsendable`], or the link lost in a wait before it): no
/// connection carried it, so its fate is known whatever became of the link, and its in-flight
/// entry is dropped ([`Bridge::fate_unknown`]). Kept, the next attach on the state dir would
/// publish it, a successor's for an aterm that is gone included.
#[derive(Debug)]
struct Unsent(String);

impl std::fmt::Display for Unsent {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Unsent {}

/// `e`, said of a post that never left the bridge ([`Unsent`]), its kind and words kept.
fn unsent(e: io::Error) -> io::Error {
    io::Error::new(e.kind(), Unsent(e.to_string()))
}

fn plain_pin(seq: u64, subject: &str, body: &[u8]) -> PublishPin {
    PublishPin {
        seq,
        via_key: false,
        identity: Some(publication_identity(subject, body)),
        original: None,
        original_off: None,
        owner: None,
        at: None,
    }
}

fn publication_identity(subject: &str, bytes: &[u8]) -> PublishIdentity {
    let (mut body, _) = Body::decode(bytes);
    body.t = 0;
    let canonical = body.encode();
    let mut hash = aterm_digest::Sha256::new();
    hash.update(b"aterm-link-publication-v1\0");
    hash.update((subject.len() as u64).to_be_bytes());
    hash.update(subject.as_bytes());
    hash.update((canonical.len() as u64).to_be_bytes());
    hash.update(&canonical);
    PublishIdentity {
        subject: subject.to_string(),
        digest: aterm_codec::hex::encode_lower(&hash.finalize()),
    }
}

/// Diagnostic remedy shown when the bridge state directory lock is already held.
pub const STATE_LOCKED_REMEDY: &str = "one state dir is one node: give each aterm instance its \
     own --state dir (its own node, with its own cap), or run one instance";

fn hold_state_dir(
    state: &StateDir,
    ctl: &mut Ctl,
    push: Option<&Ctl>,
    attachment: Attachment,
) -> io::Result<BridgeLock> {
    let held = match state.lock_bridge() {
        Err(e) if e.kind() == io::ErrorKind::WouldBlock => e,
        taken => return taken,
    };
    let (Attachment::Inherited, Some(push)) = (attachment, push) else {
        return Err(io::Error::new(
            io::ErrorKind::WouldBlock,
            format!("{held}; {STATE_LOCKED_REMEDY}"),
        ));
    };
    stderr_line!("aterm-link: {held}; standing by until it exits ({STATE_LOCKED_REMEDY})");
    let reply = ctl.request("link down reason=state-locked")?;
    if !reply.ok() {
        stderr_line!(
            "aterm-link: `link down reason=state-locked` refused: {}",
            reply.header()
        );
    }
    let queued = state.bridge_lock_wait()?;
    stand_by(push.get_ref(), move || queued.wait())
}

/// The name of the thread a standing-by bridge waits for its lock on.
const STANDBY_THREAD: &str = "aterm-link-standby";

/// Stands by until `take` holds the state dir's lock or aterm says anything on `lane`, the
/// bridge's push lane, with no timer between: the take runs on a thread of its own
/// ([`STANDBY_THREAD`]) and closes a pipe once it answers, and this thread waits on the lane
/// and that pipe at once ([`aterm_uds::ready::either`]), so the bridge goes on the moment the
/// kernel hands it the lock ([`crate::state::BridgeLockWait::wait`]).
///
/// The lane ends the standby: its end (aterm closed the connection), a byte (aterm wrote before
/// any subscription) or an error. That answer wins over a take that came at the same moment, and
/// the lock the take holds goes back as its answer is dropped unread. A take still queued then
/// waits on in the kernel until this process exits, which is what a bridge does on this error;
/// one granted before that exit has no reader, so it lets go at once, and nothing runs under it.
/// Nothing is read from the lane while the take wins, so the lane is as aterm left it.
fn stand_by(
    lane: &std::os::unix::net::UnixStream,
    take: impl FnOnce() -> io::Result<BridgeLock> + Send + 'static,
) -> io::Result<BridgeLock> {
    use std::os::fd::AsFd as _;
    let (woken, wake) = aterm_uds::pipe()?;
    let (answer, answered) = std::sync::mpsc::sync_channel(1);
    std::thread::Builder::new()
        .name(STANDBY_THREAD.to_string())
        .spawn(move || {
            let _ = answer.send(take());
            drop(wake);
        })?;
    loop {
        let ready = aterm_uds::ready::either(lane.as_fd(), woken.as_fd())?;
        if ready.first {
            let mut lane = lane;
            match std::io::Read::read(&mut lane, &mut [0u8; 1]) {
                Ok(0) => {
                    return Err(io::Error::new(
                        io::ErrorKind::ConnectionAborted,
                        "aterm closed the bridge connection while this bridge stood by",
                    ))
                }
                Ok(_) => {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "aterm wrote on the push lane before this bridge subscribed",
                    ))
                }
                Err(e)
                    if matches!(
                        e.kind(),
                        io::ErrorKind::WouldBlock
                            | io::ErrorKind::TimedOut
                            | io::ErrorKind::Interrupted
                    ) =>
                {
                    continue
                }
                Err(e) => return Err(e),
            }
        }
        return answered.recv().unwrap_or_else(|_| {
            Err(io::Error::other(
                "the standby's wait for the state dir's lock ended without an answer",
            ))
        });
    }
}

#[cfg(test)]
#[path = "bridge_standby_tests.rs"]
mod standby_tests;

fn watch_ack(line: &str) -> Option<(u64, &str)> {
    let mut toks = line.split_whitespace();
    if toks.next() != Some("sub") {
        return None;
    }
    let local = crate::parse_uint::<u64>(toks.next()?)?;
    let sid = toks.next().filter(|s| subject::is_principal(s))?;
    Some((local, sid))
}

/// The meaning fields an `events` watch's ack `sub <local> <sid> …` says (critic item E05),
/// joined as a `meta` header ([`meta_words`]): `None` when it says none, or says one twice or
/// some but not all. A sampled slot that already says them reads nothing at its ack; one that
/// says other fields, or an ack with none, has the session's own `@<sid> meta` read
/// ([`Bridge::on_event`]).
fn watch_ack_meta(line: &str) -> Option<String> {
    meta_words(line.split_whitespace().skip(3)).flatten()
}

fn bye_of(line: &str) -> Option<Bye> {
    let mut toks = line.split_whitespace();
    if (toks.next(), toks.next(), toks.next()) != (Some("EVENT"), Some("*"), Some("bye")) {
        return None;
    }
    Bye::parse(toks.next()?)
}

/// Whether the record at `off`, on this node's own lane under its own `<src>`, is one it did not
/// publish (forged-self-window): an offset its window `acked` does not hold, at or above the
/// window's `floor`. Below the floor the window knows nothing, and accuses nothing.
fn forged_self(acked: &BTreeSet<u64>, floor: u64, off: u64) -> bool {
    off >= floor && !acked.contains(&off)
}

/// The forged-self window an `acked` log loads as, and its floor (critic item F17's review):
/// its newest [`SELF_ACK_KEEP`] offsets, under the floor the file names, raised to the oldest
/// kept where the keep drops any. A file that holds offsets and names no floor is an older
/// build's, whose readers took the window's oldest offset for its floor: it loads there, so it
/// accuses nothing that build did not, and its entries, every landed post's, age out as own
/// records land ([`Bridge::land_group`]). One that holds neither (no file, one that cannot be
/// read, or one with no whole entry) loads with no floor (critic item F20): the node knows none
/// of its own records, each one an earlier run landed lies below the log's head, and the
/// window accuses nothing until the attach that first reads the head floors it there, as a
/// move does ([`Bridge::floor_at_the_head`]). Every build before F20 loaded it at 0, so it
/// accused each of them as forged.
fn self_window(acked: crate::state::AckedLog) -> (BTreeSet<u64>, Option<u64>) {
    let held = acked.offsets.len();
    let kept = newest(acked.offsets, SELF_ACK_KEEP);
    let oldest = kept.first().copied();
    let floor = match (acked.floor, oldest) {
        (Some(floor), Some(oldest)) if kept.len() < held => Some(floor.max(oldest)),
        (Some(floor), _) => Some(floor),
        (None, oldest) => oldest,
    };
    (kept, floor)
}

fn newest(mut offsets: BTreeSet<u64>, keep: usize) -> BTreeSet<u64> {
    while offsets.len() > keep {
        offsets.pop_first();
    }
    offsets
}

fn note_ctl_loss(ctl: &Ctl, mailbox: &Mailbox, err: Option<&io::Error>) {
    if err.is_some() && ctl.lost() {
        mailbox.push_aterm_closed();
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Delivery {
    Accounted,
    Unaccounted,
    Owed,
    /// Its line waits to go with its inbox run's, and its reply accounts it
    /// ([`Bridge::send_rows`]).
    Queued,
}

/// How the shared read of an attach's refill ended ([`Bridge::refill_node`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Together {
    /// Every session's records to the head were offered (a session whose delivery was owed
    /// stopped there).
    Read,
    /// The connection to aterm or the broker failed, or a page made no progress: the sessions
    /// stay owed their refill.
    Failed,
    /// Every record below this offset was offered, and each session not stopped reads on
    /// alone from there: the node inbox's oldest rose above the read, or what the read carried
    /// passed what it saved.
    HandedOn(u64),
}

/// One page of the attach's shared refill read, weighed by `meter` ([`Bridge::refill_node`]):
/// the page ended at `next` of a log whose head is `head`, carrying `carried` records it
/// offered to no session while `reading` sessions read there. `None` when it ended at or below
/// where it began, short of the head: it made no progress, and the read fails, as a lane's read
/// does ([`Bridge::refill_lane`]), where the meter alone would hand each session on to its own
/// lane from there.
fn weigh_shared_page(
    meter: &mut SharedRefillMeter,
    next: u64,
    head: u64,
    carried: u64,
    reading: usize,
) -> Option<RefillStep> {
    if next < head && next <= meter.cursor() {
        return None;
    }
    Some(meter.observe_page(next, head, carried, reading))
}

/// One session in the attach's shared refill read ([`Bridge::refill_together`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Refilling {
    /// Where its records start being offered: past its watermark, at its lane's floor, or
    /// where its own read below the node inbox's oldest got to.
    from: u64,
    /// It refills from a watermark, so a floor its read meets is told.
    after_seen: bool,
    /// aterm owes one of its records: it is offered nothing more, as its own refill stops
    /// there.
    stopped: bool,
}

/// How one session's refill of its own lane ended ([`Bridge::refill_lane`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Lane {
    /// Its records to the head were offered, or it stopped at one aterm owes.
    Done,
    /// It read its lane up to its `until`: every record of the lane below this offset was
    /// offered or given up (and told), and it reads on from here with the others.
    To(u64),
    /// The connection to aterm or the broker failed: the session stays owed its refill.
    Failed,
}

struct DeliverLine<'a> {
    addr: &'a subject::InAddr,
    body: &'a Body,
    off: u64,
    kind: WakeClass,
    demoted: Option<WakeClass>,
    late: bool,
    topic: Option<&'a str>,
    /// Where the record was first on this log, for a copy a move made across this Mac's own
    /// key rotation ([`Bridge::moved_from`]): the line's `was=`.
    was: Option<u64>,
}

/// A `deliver` request line, and how much of the text it carries.
struct DeliverSend {
    line: String,
    encoded: String,
    cut: bool,
}

/// A record whose checks passed, its line waiting to go with its inbox run's
/// ([`Bridge::send_rows`]).
struct QueuedRow {
    subject: String,
    off: u64,
    addr: subject::InAddr,
    body: Body,
    kind: WakeClass,
    demoted: Option<WakeClass>,
    late: bool,
    send: DeliverSend,
}

/// How many inbox records one run takes ([`Bridge::on_inbox_run`]): how many `deliver` lines
/// wait on its writes, and how long it keeps the loop from what else its mailbox holds. How
/// many lines go in each write is [`Ctl::pipeline`]'s to say, by their bytes.
const DELIVER_RUN_MAX: usize = 32;

fn deliver_verdict(header: &str, broadcast: bool) -> Option<&'static str> {
    let rest = header.strip_prefix("ERR ")?;
    [
        ("quota", "quota", true),
        ("too large", "too-large", true),
        ("no such session", Reject::NotHosted.token(), true),
        ("not-subscribed", "not-subscribed", broadcast),
    ]
    .into_iter()
    .find(|(sentence, _, allowed)| {
        *allowed
            && rest
                .strip_prefix(sentence)
                .is_some_and(|tail| tail.is_empty() || tail.starts_with([' ', ':']))
    })
    .map(|(_, token, _)| token)
}

fn reason_token(header: &str) -> String {
    let mut words = header.split_whitespace();
    let first = words.next().unwrap_or_default();
    let word = if first == "ERR" {
        words.next().unwrap_or_default()
    } else {
        first
    };
    let cleaned: String = word
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
        .take(REASON_TOKEN_MAX)
        .collect();
    if cleaned.is_empty() {
        "unknown".to_string()
    } else {
        cleaned
    }
}

/// Percent-encodes `text` within `budget` bytes on a UTF-8 char boundary, returning `(encoded, truncated)`.
#[must_use]
pub fn encode_bounded(text: &str, budget: usize) -> (String, bool) {
    let whole = crate::pct::encode(text);
    if whole.len() <= budget {
        return (whole, false);
    }
    let mut out = String::with_capacity(budget);
    let mut buf = [0u8; 4];
    for ch in text.chars() {
        let piece = crate::pct::encode(ch.encode_utf8(&mut buf));
        if out.len() + piece.len() > budget {
            break;
        }
        out.push_str(&piece);
    }
    (out, true)
}

fn encode_prefix(text: &str, budget: usize) -> (String, usize) {
    let whole = crate::pct::encode(text);
    if whole.len() <= budget {
        return (whole, text.len());
    }
    let mut out = String::with_capacity(budget);
    let mut used = 0;
    let mut buf = [0u8; 4];
    for ch in text.chars() {
        let piece = crate::pct::encode(ch.encode_utf8(&mut buf));
        if out.len() + piece.len() > budget {
            break;
        }
        out.push_str(&piece);
        used += ch.len_utf8();
    }
    (out, used)
}

/// Formats `deliver <sid> fetched=<off> ...` control line(s) for a fetched record.
pub fn fetched_lines(
    sid: &str,
    off: u64,
    fields: &str,
    text: &str,
) -> Result<Vec<String>, &'static str> {
    let mut end = text.len().min(FETCH_BODY_MAX);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    let carried = &text[..end];
    let mut head = format!("deliver {sid} fetched={off} {fields}");
    if carried.len() < text.len() {
        head.push_str(&format!(" len={}", text.len()));
    }
    let budget = REQUEST_LINE_MAX
        .checked_sub(head.len() + " at= more=1 text=".len() + 20)
        .filter(|b| *b >= 12)
        .ok_or("oversize")?;
    let mut lines = Vec::new();
    let mut at = 0;
    loop {
        let (piece, used) = encode_prefix(&carried[at..], budget);
        let next = at + used;
        let mut line = head.clone();
        if at > 0 {
            line.push_str(&format!(" at={at}"));
        }
        if next < carried.len() {
            line.push_str(" more=1");
        }
        line.push_str(&format!(" text={piece}"));
        lines.push(line);
        if next >= carried.len() {
            return Ok(lines);
        }
        at = next;
    }
}

/// Routing resolution for an outbound post.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Route {
    /// Six-segment owner+source subject prefix under `in/` (with `kind` appended at publish).
    To(String),
    /// Complete destination subject (such as a broadcast `/f/<F>/pub/<node>/<sid>/say/<topic>`).
    Exact(String),
    /// Destination is neither a known principal nor a live advertised session.
    Unroutable,
    /// Multiple nodes claim the destination session ID without a disambiguating pin.
    Ambiguous,
}

impl Route {
    /// Returns the `reason=` refusal for [`Self::Unroutable`] or [`Self::Ambiguous`].
    #[must_use]
    pub const fn reason(&self) -> DeadReason {
        match self {
            Route::Ambiguous => DeadReason::Ambiguous,
            Route::To(_) | Route::Exact(_) | Route::Unroutable => DeadReason::Unroutable,
        }
    }
}

fn post_body(t_ms: u64, post: &QueuedPost, in_body: Option<String>) -> Body {
    let mut body = Body::new(t_ms);
    body.kind = in_body;
    body.from = Some(post.sid.clone());
    body.re = post.re;
    body.dl = post.dl;
    body.via = post.via.clone();
    body.text = String::from_utf8_lossy(&post.body).into_owned();
    body
}

fn receipt_body(t_ms: u64, sid: &str, off: u64, verdict: &str) -> Body {
    let mut receipt = Body::new(t_ms);
    receipt.re = Some(off);
    receipt.from = Some(sid.to_string());
    receipt.verdict = Some(verdict.to_string());
    receipt.text = format!("marked {verdict} by the recipient");
    receipt
}

fn expired_body(t_ms: u64, off: u64, dl: u64) -> Body {
    let mut verdict = Body::new(t_ms);
    verdict.re = Some(off);
    verdict.dl = Some(dl);
    verdict.text = format!("no reply within {}", span_of(dl));
    verdict
}

pub(crate) fn span_of(ms: u64) -> String {
    if ms >= 1_000 && ms.is_multiple_of(1_000) {
        format!("{} s", ms / 1_000)
    } else {
        format!("{ms} ms")
    }
}

const HALT_REASON_DEFAULT: &str = "fleet-halt";

const HALT_REASON_MAX: usize = 128;

fn halt_reason_token(raw: Option<&str>) -> String {
    let out = crate::presence::clamp_pct(raw.unwrap_or_default(), HALT_REASON_MAX);
    if out.is_empty() {
        return HALT_REASON_DEFAULT.to_string();
    }
    out
}

fn presence_inc(body: &Body) -> Option<u64> {
    body.unknown
        .get("inc")
        .filter(|s| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()))?
        .parse()
        .ok()
        .filter(|inc| *inc <= INC_MAX)
}

fn carried_elsewhere(body: &Body, inc: u64, self_inc: u64, host: &str) -> Option<String> {
    if inc == self_inc || body.unknown.get("state").map(String::as_str) != Some("live") {
        return None;
    }
    let row_host = body.unknown.get("host").map(|h| crate::pct::decode(h));
    let row_pid = body
        .unknown
        .get("pid")
        .filter(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()))
        .and_then(|p| p.parse::<u32>().ok());
    if row_host.as_deref() == Some(host)
        && row_pid
            .is_some_and(|pid| pid == std::process::id() || !aterm_uds::process::pid_alive(pid))
    {
        return None;
    }
    Some(format!(
        "inc {inc}, pid {}, on {}",
        row_pid.map_or_else(|| "-".to_string(), |p| p.to_string()),
        row_host.map_or_else(|| "-".to_string(), |h| crate::render::safe(&h, 64))
    ))
}

fn adoptable(inc: Option<u64>, self_inc: u64, witnessed_dead: Option<u64>, marked: bool) -> bool {
    match inc {
        Some(inc) => (marked && inc <= self_inc) || witnessed_dead == Some(inc),
        None => false,
    }
}

fn replier_of(subject: &str) -> Option<String> {
    let seg: Vec<&str> = subject.split('/').collect();
    if seg.len() != 8 || seg[1] != "f" || seg[3] != "in" {
        return None;
    }
    let owner = if seg[4] == "p" { seg[5] } else { seg[4] };
    subject::is_principal(owner).then(|| owner.to_string())
}

fn hostname() -> String {
    static HOST: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    HOST.get_or_init(|| aterm_uds::process::host_name().unwrap_or_else(|| "-".to_string()))
        .clone()
}

/// Let the old halt rows of `fleet` the log keeps (`logged`, each person's newest) say what is in
/// force for each person there: `Some(reason)` for a halt, `None` for one withdrawn.
fn take_old_halt_rows(
    rows: &mut BTreeMap<(String, String), Option<String>>,
    fleet: &str,
    logged: &[BrokerRecord],
) {
    for (_, subject, raw) in logged {
        if let Some(row) = subject::parse_halt(fleet, subject, raw) {
            let reason = row.on.then(|| row.reason.unwrap_or_default());
            rows.insert((fleet.to_owned(), row.human.to_owned()), reason);
        }
    }
}

/// The halts a bridge applied with the mesh on, as its state dir keeps them
/// ([`crate::halt::STATE_FILE`]). A damaged line is said and costs only itself; a file of another
/// version is said and read as none, since this build cannot tell what it holds.
fn load_mesh_halts(state: &StateDir) -> crate::halt::Halts {
    let Some(text) = state.mesh_halts() else {
        return crate::halt::Halts::default();
    };
    match crate::halt::Halts::parse(&text) {
        Ok((halts, damaged)) => {
            for line in damaged {
                stderr_line!(
                    "aterm-link: {}: `{line}` is not a halt this node applied; it is not kept",
                    crate::halt::STATE_FILE
                );
            }
            halts
        }
        Err(e) => {
            stderr_line!(
                "aterm-link: {} in {}: {e}; no halt it names is applied",
                crate::halt::STATE_FILE,
                state.root().display()
            );
            crate::halt::Halts::default()
        }
    }
}

#[cfg(test)]
thread_local! {
    /// The negative control of the person check (§11 C3c): a bridge on a thread where it is set
    /// takes any record that names a person ([`crate::person_key::BY_TOKEN`]) as that person's,
    /// its seal unchecked.
    static TRUST_ANY_PERSON: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

#[cfg(test)]
thread_local! {
    /// The negative control of the lane back to a person on another Mac (§11 C3d): a bridge on a
    /// thread where it is set has none, as before ([`Bridge::person_route`]).
    static NO_WAY_BACK: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    /// The negative control of what another Mac's answer to a person names (§11 C3d): a bridge
    /// on a thread where it is set hands it to the person's inbox with the `re=` that Mac wrote,
    /// the carried copy's offset here ([`Bridge::hand_to_person`]).
    static WIRE_RE_TO_PERSON: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    /// The negative control of what a person's answer to another Mac's session names (§11 C3d):
    /// a bridge on a thread where it is set hands that session's post to the person's inbox
    /// unstamped, as before, so the answer names the copy's offset here
    /// ([`Bridge::hand_to_person`]).
    static UNSTAMPED_COPY: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    /// The negative control of who a person's answer from another Mac comes from (§11 C3d): a
    /// bridge on a thread where it is set judges it by the sender it names, `<person>@<node>`,
    /// which no ask went to, as before ([`Bridge::reply_from`]).
    static PERSON_REPLY_AS_SENDER: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    /// The negative control of the stamp on a copy from a Mac that rotated its key (§11 C3d): a
    /// bridge on a thread where it is set stamps only where the hand-off it reads says the
    /// producer is the sender's ([`Bridge::sender_seq`]), as before, so a copy brought while it
    /// has no hand-off carries none ([`Bridge::hand_to_person`]).
    static STAMP_BY_SENDER_SEQ: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

#[cfg(test)]
thread_local! {
    /// The negative control of a record moved across two rotations of this Mac (§11 C3b): a
    /// bridge on a thread where it is set follows a moved record back one rotation alone
    /// ([`Bridge::wire_re`]).
    pub(crate) static ONE_HOP: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    /// The negative control of a moved copy an attach's refill delivers (§11 C3b): a bridge on a
    /// thread where it is set takes its refill pages' records as stamped with no origin
    /// ([`Bridge::moved_from`]).
    pub(crate) static REFILL_UNSTAMPED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    /// How many reads of a record's origin on a connection of their own a bridge on this thread
    /// made ([`Bridge::held_record`]).
    pub(crate) static ORIGIN_READS: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
    /// The negative control of mail delivered under a node this Mac held before (§11 C3b): a
    /// bridge on a thread where it is set reads a record on this node's lanes alone
    /// ([`Bridge::old_inbox_record`]).
    pub(crate) static OWN_LANES_ONLY: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// The node service's hand-off as a bridge last read it (§11 C3): the file's identity, and what it
/// said. Read again only when the file was replaced.
struct HandoffSeen {
    stamp: Option<(u64, u64, i64, i64, u64)>,
    handoff: Result<crate::federate::Handoff, (NoHandoff, String)>,
    told: Option<String>,
}

impl Default for HandoffSeen {
    fn default() -> Self {
        Self {
            stamp: None,
            handoff: Err((NoHandoff::Missing, String::new())),
            told: None,
        }
    }
}

impl HandoffSeen {
    /// The hand-off in `root` for this Mac (`me`), read again if the file changed since.
    ///
    /// # Errors
    /// The hand-off is missing, unreadable, or names another node as this Mac's (stale): said
    /// once on stderr for each reason in turn.
    fn read(
        &mut self,
        root: &std::path::Path,
        me: &str,
    ) -> Result<&crate::federate::Handoff, NoHandoff> {
        use std::os::unix::fs::MetadataExt;
        let path = root.join(crate::federate::HANDOFF_FILE);
        let at = std::fs::metadata(&path);
        let stamp = at
            .as_ref()
            .ok()
            .map(|m| (m.dev(), m.ino(), m.mtime(), m.mtime_nsec(), m.size()));
        if stamp.is_none() || stamp != self.stamp {
            self.stamp = stamp;
            let missing = || {
                (
                    NoHandoff::Missing,
                    format!(
                        "{} does not exist: the node service writes it as its mail pushes start \
                         (`aterm node status` says whether it runs)",
                        path.display()
                    ),
                )
            };
            let unreadable = |e: &io::Error| {
                (
                    NoHandoff::Unreadable,
                    format!(
                        "{} cannot be read ({e}): the node service rewrites it when the members \
                         change, or at its next start (`aterm node status`)",
                        path.display()
                    ),
                )
            };
            self.handoff = match at {
                Err(e) if e.kind() == io::ErrorKind::NotFound => Err(missing()),
                Err(e) => Err(unreadable(&e)),
                Ok(_) => match crate::federate::Handoff::read(root) {
                    Ok(Some(handoff)) => Ok(handoff),
                    Ok(None) => Err(missing()),
                    Err(e) => Err(unreadable(&e)),
                },
            };
        }
        let refused = match &self.handoff {
            Ok(handoff) if handoff.me == me => None,
            Ok(handoff) => Some((
                NoHandoff::Stale,
                format!(
                    "{} names {} as this Mac, which is {me}: the node service has not rewritten \
                     it since this Mac's node key changed (`aterm node status`)",
                    path.display(),
                    handoff.me
                ),
            )),
            Err((no, why)) => Some((*no, why.clone())),
        };
        if let Some((no, why)) = refused {
            if self.told.as_deref() != Some(why.as_str()) {
                stderr_line!("aterm-link: no other Mac is routed to: {why}");
                self.told = Some(why);
            }
            return Err(no);
        }
        self.told = None;
        self.handoff.as_ref().map_err(|(no, _)| *no)
    }
}

/// The node this Mac's node key names, with the mesh on (§11 C3), recorded as `state`'s node: a
/// cap ring minted for another node is refused, as [`node_for`] refuses it.
fn mesh_node(
    state: &StateDir,
    caps: &[Cap],
    mesh: &crate::federate::MeshPaths,
) -> io::Result<String> {
    let node = crate::federate::node_of_key(&mesh.keys)?.ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::NotFound,
            format!(
                "{} holds no {}: the node service writes this Mac's node key as it starts \
                 (`aterm node on`)",
                mesh.keys.display(),
                crate::federate::NODE_KEY_FILE
            ),
        )
    })?;
    let bound = ring_binds(caps);
    if !bound.is_empty() && !bound.contains(&node) {
        let named = bound.iter().cloned().collect::<Vec<_>>().join(", ");
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "this Mac's node key names {node}, but the cap files are minted for {named}: a \
                 guarded broker refuses every publish of {node} under them. `aterm fabric on` \
                 re-mints the root's cap for {node}"
            ),
        ));
    }
    if let Some(old) = state.name_node(&node)? {
        stderr_line!(
            "aterm-link: this Mac's node is {node}, named by its node key (it was {old}); each \
             sender carries the mail it holds for {old} here"
        );
    }
    Ok(node)
}

fn mint_node_id() -> String {
    let token = aterm_uds::rand::hex_token::<8>().expect("the OS CSPRNG");
    format!("n-{token}")
}

fn node_for(state: &StateDir, caps: &[Cap]) -> io::Result<String> {
    let bound = ring_binds(caps);
    let named = bound.iter().cloned().collect::<Vec<_>>().join(", ");
    let dir = state.root().display();
    let refuse = |msg: String| io::Error::new(io::ErrorKind::InvalidInput, msg);
    match state.node() {
        Some(node) if !crate::enable::is_node_id(&node) => Err(refuse(format!(
            "{dir}/node holds `{}`, which is not a node id (`n-` and [a-z0-9-]): `aterm fabric \
             on` provisions one and keeps this file aside",
            crate::render::safe(&node, 64)
        ))),
        Some(node) if bound.is_empty() || bound.contains(&node) => Ok(node),
        Some(node) => Err(refuse(format!(
            "{dir}/node is {node}, but the cap files are minted for {named}: a guarded broker \
             refuses every publish of {node} under them. `aterm fabric on` re-mints the root's \
             cap for {node}; or give this bridge the --state the cap was minted for"
        ))),
        None if bound.is_empty() => state.node_id(mint_node_id),
        None => Err(refuse(format!(
            "{dir} records no node id, and the cap files are minted for {named}: give this \
             bridge the --state that records {named}, or a cap of its own (`aterm link mint` \
             under the fleet's mint secret, one line per grant of its ring) — taking {named} \
             here would run two bridges as one node"
        ))),
    }
}

fn ring_binds(caps: &[Cap]) -> BTreeSet<String> {
    caps.iter()
        .filter_map(|c| astream_cap::Grant::parse(&c.grant).ok())
        .filter(|g| g.mode == astream_cap::Mode::ReadWrite)
        .filter_map(|g| g.principal)
        .filter(|p| crate::enable::is_node_id(p))
        .collect()
}

/// The ring in `cap_files` if a bridge holding `held` as `node` takes it, or why it keeps `held`.
fn ring_on_disk(cap_files: &[String], held: &[Cap], node: &str) -> Result<Vec<Cap>, String> {
    let fresh = read_ring(cap_files).map_err(|e| e.to_string())?;
    ring_refusal(&fresh, held, node).map_or(Ok(fresh), Err)
}

fn ring_refusal(fresh: &[Cap], held: &[Cap], node: &str) -> Option<String> {
    let binds = ring_binds(fresh);
    if let Some(other) = binds.iter().find(|n| n.as_str() != node) {
        return Some(format!("binds {other}, not this bridge's node {node}"));
    }
    (ring_binds(held).contains(node) && !binds.contains(node))
        .then(|| format!("binds no write for this bridge's node {node}"))
}

/// Another Mac's record for a person of this Mac (§11 C3d, [`Bridge::person_reply`]).
struct PersonReply {
    /// The person it is for.
    person: String,
    /// The node of the Mac that wrote it.
    src: String,
    /// The `<kind>` segment.
    kind: WakeClass,
}

/// Outbound post parsed from an endpoint `outbox` drain frame.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QueuedPost {
    pub sid: String,
    pub id: u64,
    pub to: String,
    pub kind: String,
    pub re: Option<u64>,
    pub dl: Option<u64>,
    pub via: Option<String>,
    /// Optional caller idempotency key (`post key=`).
    pub key: Option<String>,
    pub body: Vec<u8>,
}

/// Parsed contents of a single endpoint `outbox` peek frame.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Drain {
    pub posts: Vec<QueuedPost>,
    /// Owed session receipts parsed from `receipt ...` lines.
    pub receipts: Vec<OwedReceipt>,
    /// Pending `(sid, off)` record fetch requests.
    pub fetches: Vec<(String, u64)>,
    /// Whether additional posts remain queued past the endpoint drain budget.
    pub more: bool,
}

/// Receipt owed by a session after `inbox seen <id> handled|refused|deferred`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OwedReceipt {
    pub sid: String,
    /// Endpoint per-session receipt identifier.
    pub rid: u64,
    /// Bus offset (`re=`) of the ask or task being acknowledged.
    pub off: u64,
    pub verdict: String,
    pub kind: String,
    /// Sender (`from=`) of the original ask or task.
    pub from: String,
    /// Whether `off` refers to a broker log prior to the currently attached log (`log=earlier`).
    pub earlier_log: bool,
}

fn parse_receipt_line(line: &str) -> Option<OwedReceipt> {
    let dec = |v: &str| {
        (!v.is_empty() && v.bytes().all(|b| b.is_ascii_digit()))
            .then(|| v.parse::<u64>().ok())
            .flatten()
    };
    let (mut sid, mut rid, mut off, mut verdict, mut kind, mut from) =
        (None, None, None, None, None, None);
    let mut earlier_log = false;
    for tok in line.split_whitespace() {
        match tok.split_once('=') {
            Some(("sid", v)) => sid = Some(v),
            Some(("rid", v)) => rid = dec(v),
            Some(("off", v)) => off = dec(v),
            Some(("verdict", v)) => verdict = Some(v),
            Some(("kind", v)) => kind = Some(v),
            Some(("from", v)) => from = Some(v),
            Some(("log", v)) => earlier_log = v == "earlier",
            _ => {}
        }
    }
    let sid = sid.filter(|s| subject::is_principal(s))?;
    Some(OwedReceipt {
        sid: sid.to_string(),
        rid: rid.filter(|n| *n > 0)?,
        off: off?,
        verdict: verdict?.to_string(),
        kind: kind?.to_string(),
        from: from?.to_string(),
        earlier_log,
    })
}

/// Parses a raw `outbox` response frame into posts, receipts, fetches, and `more` flag.
#[must_use]
pub fn parse_drain(frame: &[u8]) -> Drain {
    let dec64 = |v: &str| {
        (!v.is_empty() && v.bytes().all(|b| b.is_ascii_digit()))
            .then(|| v.parse::<u64>().ok())
            .flatten()
    };
    let mut out = Drain::default();
    let mut rest = frame;
    while !rest.is_empty() {
        let Some(nl) = rest.iter().position(|b| *b == b'\n') else {
            break;
        };
        let line = String::from_utf8_lossy(&rest[..nl]).into_owned();
        let tail = &rest[nl + 1..];
        if line == "more" {
            out.more = true;
            rest = tail;
            continue;
        }
        if let Some(receipt) = line.strip_prefix("receipt ") {
            if let Some(r) = parse_receipt_line(receipt) {
                out.receipts.push(r);
            }
            rest = tail;
            continue;
        }
        if let Some(fetch) = line.strip_prefix("fetch ") {
            let mut sid: Option<&str> = None;
            let mut off: Option<u64> = None;
            for tok in fetch.split_whitespace() {
                if let Some(s) = tok.strip_prefix("sid=") {
                    sid = Some(s);
                } else if let Some(n) = tok.strip_prefix("off=") {
                    off = dec64(n);
                }
            }
            if let (Some(sid), Some(off)) = (sid, off) {
                if subject::is_principal(sid) {
                    out.fetches.push((sid.to_string(), off));
                }
            }
            rest = tail;
            continue;
        }
        let mut post = QueuedPost {
            sid: String::new(),
            id: 0,
            to: String::new(),
            kind: String::new(),
            re: None,
            dl: None,
            via: None,
            key: None,
            body: Vec::new(),
        };
        let mut len: Option<usize> = None;
        for tok in line.split_whitespace() {
            let Some((k, v)) = tok.split_once('=') else {
                continue;
            };
            match k {
                "sid" => post.sid = v.to_string(),
                "id" => post.id = dec64(v).unwrap_or(0),
                "to" => post.to = v.to_string(),
                "kind" => post.kind = v.to_string(),
                "re" => post.re = dec64(v),
                "dl" => post.dl = dec64(v),
                "via" => post.via = Some(v.to_string()),
                "key" => post.key = Some(v.to_string()),
                "len" => {
                    len = (!v.is_empty() && v.bytes().all(|b| b.is_ascii_digit()))
                        .then(|| v.parse().ok())
                        .flatten()
                }
                _ => {}
            }
        }
        let Some(len) = len.filter(|n| *n <= tail.len()) else {
            break;
        };
        post.body = tail[..len].to_vec();
        if post.sid.is_empty() || post.id == 0 {
            break;
        }
        out.posts.push(post);
        rest = &tail[len..];
    }
    out
}

/// Spawns a background thread pumping a broker subscription into `mailbox` for generation `gen`,
/// each read stamped by the live world's clock (`Fx::os`), the only world this reader runs in.
pub(crate) fn spawn_reader(
    mut sub: astream_broker::AnySubscription,
    closer: Arc<BusCloser>,
    deadline: Duration,
    source: Source,
    mailbox: Arc<Mailbox>,
) -> io::Result<()> {
    let generation = mailbox.broker_generation(source);
    let clock = Fx::os();
    std::thread::Builder::new()
        .name(format!("aterm-link-{source:?}"))
        .spawn(move || loop {
            let read = next_took(&mut sub, &closer, deadline);
            let at = clock.now();
            if !reader_took(read, &mailbox, source, generation, &closer, at, deadline) {
                return;
            }
        })
        .map(drop)
}

/// One read a subscription's reader hands on ([`reader_took`]).
#[derive(Debug)]
pub(crate) enum Took {
    /// A record of the subscription's filter, with the origin its tail stamped on it where the
    /// connection asked ([`Bridge::connect`]).
    Record(BrokerRecord, crate::mailbox::Origin),
    /// A GAP NOTICE (astream R4's addendum A9): retention gave up records of the filter below
    /// the tail, before the records the tail delivers after it.
    Gap(astream_broker::Gap),
}

/// The next record or gap notice on `sub`, its marks skipped: astream's `recv_live` rule (every
/// mark re-arms `deadline` through a live closer), event by event, so a notice is not skipped.
pub(crate) fn next_took(
    sub: &mut astream_broker::AnySubscription,
    closer: &BusCloser,
    deadline: Duration,
) -> io::Result<Option<Took>> {
    loop {
        let event = match closer {
            BusCloser::Os(live) => sub.recv_live_event(live, deadline)?,
            #[cfg(test)]
            BusCloser::Sim(_) => {
                let _ = deadline;
                sub.recv_event()?
            }
        };
        match event {
            None => return Ok(None),
            Some(astream_broker::Event::Mark { .. }) => {}
            Some(astream_broker::Event::Delivery {
                offset,
                subject,
                body,
            }) => {
                return Ok(Some(Took::Record(
                    (offset, subject, body),
                    sub.last_origin(),
                )))
            }
            Some(astream_broker::Event::Gap(g)) => return Ok(Some(Took::Gap(g))),
        }
    }
}

/// Routes one subscription read result into `mailbox` under incarnation `gen`, read at `at` (the
/// reader's world's clock, [`Mailbox::push`]), returning `true` to continue. A read that ended
/// is pushed with how it ended ([`End`]), a timeout as the live read `deadline` passing.
pub(crate) fn reader_took(
    read: io::Result<Option<Took>>,
    mailbox: &Mailbox,
    source: Source,
    generation: u64,
    closer: &BusCloser,
    at: Instant,
    deadline: Duration,
) -> bool {
    match read {
        Ok(Some(Took::Gap(g))) => {
            if mailbox.push_gap(source, g, generation, at) {
                true
            } else {
                closer.close();
                false
            }
        }
        Ok(Some(Took::Record(rec, origin))) => {
            match mailbox.push(source, rec, origin, generation, at) {
                Pushed::Queued => true,
                Pushed::Paused | Pushed::Stale => {
                    closer.close();
                    false
                }
            }
        }
        Err(e) if astream_broker::broker_refusal(&e).is_some() => {
            mailbox.push_refused(source, generation, link_reason(&e, "subscribe"));
            false
        }
        Ok(None) => {
            mailbox.push_lost(source, generation, End::Eof);
            false
        }
        Err(e) => {
            mailbox.push_lost(source, generation, End::of(&e, deadline));
            false
        }
    }
}

fn spawn_event_reader(push: Ctl, mailbox: Arc<Mailbox>) {
    let lost = Arc::clone(&mailbox);
    let spawned = std::thread::Builder::new()
        .name("aterm-link-events".to_string())
        .spawn(move || {
            use std::io::{BufRead, BufReader, Write};
            let Ok(stream) = push.get_ref().try_clone() else {
                mailbox.push_aterm_closed();
                return;
            };
            if push
                .get_ref()
                .write_all(b"subscribe @* events,sessions\n")
                .is_err()
            {
                mailbox.push_aterm_closed();
                return;
            }
            let _ = push.get_ref().flush();
            let mut reader = BufReader::new(stream);
            let mut line = String::new();
            loop {
                line.clear();
                match reader.read_line(&mut line) {
                    Ok(0) | Err(_) => {
                        mailbox.push_aterm_closed();
                        return;
                    }
                    Ok(_) => {
                        let trimmed = line.trim_end().to_string();
                        if let Some(bye) = bye_of(&trimmed) {
                            mailbox.note_bye(bye);
                        }
                        if trimmed.starts_with("EVENT")
                            || trimmed.starts_with("GAP")
                            || trimmed.starts_with("sub ")
                        {
                            mailbox.push_event(trimmed);
                        }
                    }
                }
            }
        });
    if let Err(e) = spawned {
        stderr_line!("aterm-link: the push-lane reader thread could not start: {e}");
        lost.push_aterm_closed();
    }
}

#[cfg(test)]
#[path = "bridge_tests.rs"]
mod tests;
