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

use astream_broker::Record as BrokerRecord;
use aterm_hash::fnv1a_64;
use aterm_types::fabric_grammar::{Bye, ANSWER_KINDS};

use crate::body::{via_ok, Body};
use crate::ctl::{Ctl, Reply, REQUEST_LINE_MAX};
use crate::fx::{BusCloser, Fx};
use crate::mailbox::{EventMark, Item, Mailbox, Pushed, ReplayPressure, Source};
use crate::pipeline::Link;
use crate::presence::{Fields, Marked, Mode, Slot};
use crate::render::trust_of;
use crate::state::{
    Asked, BridgeLock, Deadline, InFlight, LogWitness, PublishIdentity, PublishPin, StandingHalt,
    StateDir, TopicCursor, ASKED_KEEP, DEADLINES_KEEP, INC_MAX,
};
use crate::subject::{self, Reject};
use crate::transport::{self, Backoff, Conn};
use crate::wait_tally::Recorder;

#[path = "bridge_mirror.rs"]
mod mirror_run;

/// Maximum time the publisher connection waits for the broker to answer a single request.
pub(crate) const ACK_DEADLINE: Duration = Duration::from_secs(5);

const ACK_DEADLINE_SEAM: &str = "ATERM_LINK_ACK_DEADLINE_MS";

fn ack_deadline() -> Duration {
    static AT: std::sync::OnceLock<Duration> = std::sync::OnceLock::new();
    *AT.get_or_init(|| {
        aterm_types::dev_seam!(ACK_DEADLINE_SEAM)
            .and_then(|ms| ms.to_str()?.parse::<u64>().ok())
            .map_or(ACK_DEADLINE, Duration::from_millis)
    })
}

const LINK_REFRESH: Duration = Duration::from_secs(2);

const LINK_MOVE_GAP: Duration = Duration::from_millis(250);

/// How long a failed redial [`Bridge::dial_failed`] keeps quiet (its reason was said
/// already) before it says the outage again, with the redials it stood for.
const LINK_DOWN_RETELL: Duration = Duration::from_secs(30);

const REFILL_FLOOR_SPAN: u64 = 4096;

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
            .and_then(|ms| ms.to_str()?.parse::<u64>().ok())
            .map_or(RECLAIM_AFTER, Duration::from_millis)
    })
}

/// How often the sweep runs while it owes one: [`GHOST_SWEEP`], or sooner when the
/// reclaim's own wait is shorter.
fn ghost_sweep() -> Duration {
    GHOST_SWEEP.min(reclaim_after())
}

const SELF_ACK_KEEP: usize = 4096;

const DEMOTE_UNLESS_ACCEPTED: [&str; 2] = ["task", "control"];

const VERDICT_KINDS: [&str; 2] = ["undeliverable", "expired"];

const WAITING_KINDS: [&str; 2] = ["ask", "task"];

const NEW_LANES_PER_NODE: usize = 64;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum NoLane {
    Undrained,
    Capped,
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
}

impl StatusSample {
    fn apply_presence(&self, fields: &mut Fields) {
        fields.set_detail(Some(&self.detail));
        if !self.agent_deferred {
            fields.set_agent(self.agent.as_deref());
        }
    }
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
    for tok in tokens {
        if let Some(v) = tok.strip_prefix("sid=") {
            if sid.replace(v.parse::<u64>().ok()?).is_some() {
                return None;
            }
        } else if let Some(v) = tok.strip_prefix("revision=") {
            if revision.replace(v.parse::<u64>().ok()?).is_some() {
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
    (sid == Some(local)).then_some(StatusSample {
        hold,
        detail: detail?,
        agent,
        agent_deferred,
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
        let Some(local) = words.next().and_then(|w| w.parse::<u64>().ok()) else {
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

    fn fired(self, state: &StateDir) {
        let _ = std::fs::write(state.root().join("fault-fired"), b"1\n");
        stderr_line!("aterm-link: ATERM_LINK_FAULT={self:?} fired");
    }
}

/// Minted capability line parsed from `--cap-file` via [`astream_cap::capfile`].
pub use astream_cap::capfile::Line as Cap;

/// Reads and parses a `--cap-file` into capability lines via [`astream_cap::capfile`].
pub fn read_cap_file(path: &str) -> io::Result<Vec<Cap>> {
    astream_cap::capfile::read_file(path).map_err(|e| match e {
        astream_cap::capfile::ReadError::Io { error, .. } => {
            io::Error::new(error.kind(), format!("{path}: {error}"))
        }
        malformed @ astream_cap::capfile::ReadError::Malformed(_) => {
            io::Error::new(io::ErrorKind::InvalidData, malformed.to_string())
        }
    })
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
        Source::Inbox => "inbox",
        Source::Say => "say",
        Source::Aterm => "aterm",
    }
}

const fn cut_reason(source: Source) -> &'static str {
    match source {
        Source::Fleet => "cut-fleet",
        Source::Inbox => "cut-inbox",
        Source::Say => "cut-say",
        Source::Aterm => "closed",
    }
}

fn refused_for_good(e: &io::Error) -> bool {
    astream_broker::broker_refusal(e).is_some_and(|r| r.is_permanent() && !r.is_expired())
}

fn refusal_is_ack(e: &io::Error) -> bool {
    astream_broker::broker_refusal(e).is_some_and(|r| {
        r.code != astream_broker::proto::ERR_TOO_MANY_CONNS && !r.is_expired() && !r.is_poisoned()
    })
}

fn read_back_rows(
    page: io::Result<astream_broker::Page>,
    off: u64,
) -> io::Result<Vec<BrokerRecord>> {
    match page {
        Ok((rows, _)) => Ok(rows),
        Err(e) if transport::retained_past(&e, off).is_some() => Ok(Vec::new()),
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
        crate::pct::encode(&aterm_types::version::build_id())
    )
}

const LINK_LOG_LANE: u32 = 2;

const LINK_CONTINUES_LANE: u32 = 3;

const LINK_SET_ASIDE_LANE: u32 = 4;

const LINK_QUARANTINE_LANE: u32 = aterm_types::control_verbs::LINK_QUARANTINE_LANE;

const LINK_MIRROR_WAIT_LANE: u32 = 6;

/// The lane that added `link gap from=<a> to=<b> [records=<n>]` (astream R4's addendum A9).
const LINK_GAP_LANE: u32 = aterm_types::control_verbs::LINK_GAP_LANE;

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

const PIPELINE_WINDOW: usize = 64;

const IDLE_ACK_GRACE: Duration = Duration::from_millis(5);

const INBOX_COMMIT_EVERY: usize = 256;

#[derive(Debug)]
enum Then {
    Ev {
        payload: String,
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

    fn own_lane_post(&self) -> bool {
        matches!(self, Then::Post(sent) if sent.journaled)
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PostNext {
    Moves,
    Holds,
    Ends,
}

const SUBJECT_BUDGET: &str = "subject-budget";

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
    group_record: Option<u64>,
    locals: BTreeMap<u64, String>,
    per_sid: PerSession,
    roster_due: Instant,
    ghost_due: Instant,
    ghosts: BTreeMap<String, Instant>,
    exits_owed: BTreeSet<String>,
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
    bus_high: u64,
    bus_log: Option<astream_broker::LogIdentity>,
    indexed_subjects: BTreeSet<String>,
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
    committed: u64,
    commit_refused: bool,
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
    #[cfg(test)]
    moved_at_start: u64,
    /// The next reclaim's read of the lanes this node writes is refused with this code.
    #[cfg(test)]
    reclaim_read_refused: Option<u8>,
    /// A record written onto a subject just ahead of its retire, as a successor or a sender would.
    #[cfg(test)]
    written_before_retire: Option<(String, Vec<u8>)>,
    self_acked: BTreeSet<u64>,
    acked_entries: usize,
    acked_appendable: bool,
    unsettled: BTreeMap<u64, u64>,
    awaiting_drain: BTreeMap<u64, (u64, bool)>,
    unsent: Option<u64>,
    held_senders: BTreeSet<String>,
    collided: Option<(u64, u64)>,
    seq_block: Option<SeqBlock>,
    lanes_opened: BTreeSet<String>,
    deadlines: BTreeMap<u64, Deadline>,
    asked: BTreeMap<u64, String>,
    expired: BTreeMap<u64, String>,
    deadline_due: Instant,
    closers: Vec<(Source, Arc<BusCloser>)>,
    link: LinkReport,
    quiet_dials: QuietDials,
    attaching: bool,
    attach_rtt: Option<Duration>,
    bye: Option<Bye>,
    withdrawn: BTreeSet<String>,
    withdrawal_told: bool,
    set_aside: u64,
    quarantine_said: Option<u64>,
    quarantine_unread: bool,
    pin_unreconciled: Option<String>,
    recovery_owed: bool,
    /// The last bridge of the state dir ran in an earlier boot, as judged when this bridge
    /// started ([`StateDir::last_ran_in_an_earlier_boot`]): the run record named an earlier
    /// boot, or, with no record, the incarnation lease did, and no lease named an attach of this
    /// boot. The start's first roster, where it reads, sweeps that boot's sessions' files
    /// ([`Bridge::sweep_sessions_of_an_earlier_boot`]); no later roster does.
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
        } = self;
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
        let caps = read_ring(&cfg.cap_files)?;
        Self::assemble(cfg, fx, state, caps, ctl, None, Attachment::Inherited)
    }

    fn assemble(
        cfg: Config,
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
        let node = node_for(&state, &caps)?;
        let producer_id = astream_cap::producer_id_of(&node);
        let _ = std::fs::write(
            state.root().join("pid"),
            format!("{}\n", std::process::id()),
        );
        let fault = Fault::from_env(&state);
        let waits = Recorder::from_env(state.root());
        let mailbox = Arc::new(Mailbox::default());
        mailbox.stamp(waits.on());
        state.time_ops(waits.on());
        let log_witness = state.log_witness().unwrap_or_default();
        let acked = state.self_acked_log();
        let mirror = mirror_run::MirrorRun::new(cfg.log, &state, fx.now(), || fx.alias_key());
        let now = fx.now();
        Ok(Self {
            node,
            producer_id,
            inc: 0,
            witnessed_dead: None,
            mark: None,
            self_acked: newest(acked.offsets, SELF_ACK_KEEP),
            acked_entries: acked.entries,
            acked_appendable: acked.appendable,
            unsettled: BTreeMap::new(),
            awaiting_drain: BTreeMap::new(),
            unsent: None,
            held_senders: BTreeSet::new(),
            collided: None,
            seq_block: None,
            lanes_opened: BTreeSet::new(),
            deadlines: state.deadlines().into_iter().map(|d| (d.off, d)).collect(),
            asked: state.asked().into_iter().map(|a| (a.off, a.to)).collect(),
            expired: BTreeMap::new(),
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
            group_record: None,
            locals: BTreeMap::new(),
            per_sid: PerSession::default(),
            roster_due: now + roster_refresh(),
            ghost_due: now + ghost_sweep(),
            ghosts: BTreeMap::new(),
            exits_owed: BTreeSet::new(),
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
            bus_high: log_witness.high,
            bus_log: log_witness.id,
            indexed_subjects: BTreeSet::new(),
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
            committed: 0,
            commit_refused: false,
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
            withdrawn: BTreeSet::new(),
            withdrawal_told: false,
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

    fn ctl_request(&mut self, line: &str) -> io::Result<Reply> {
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

    fn connect(&self) -> io::Result<(Conn, BusCloser)> {
        let (mut c, closer) = self.fx.dial(&self.cfg.broker, ack_deadline())?;
        transport::attach_ring(&mut c, &self.caps)?;
        Ok((c, closer))
    }

    fn reload_caps(&mut self) {
        let fresh = read_ring(&self.cfg.cap_files).map_err(|e| e.to_string());
        let verdict = fresh
            .and_then(|ring| ring_refusal(&ring, &self.caps, &self.node).map_or(Ok(ring), Err));
        match verdict {
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
        self.waits.request(started);
        self.judge(started, r.as_ref().map(drop), other);
        self.service_urgent();
        r
    }

    fn judge(&mut self, started: Instant, r: Result<(), &io::Error>, other: Option<&'static str>) {
        match r {
            Ok(()) => self.note_ack(self.fx.now().saturating_duration_since(started)),
            Err(e) if other.is_none() && refusal_is_ack(e) => {
                self.note_ack(self.fx.now().saturating_duration_since(started));
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
        self.conn.as_mut().and_then(Link::ready)
    }

    fn settle_acks(&mut self) {
        let waiting = self.fx.now();
        while self.settle_one(waiting) {}
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
        while self.try_settle_one() {}
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

    fn settled(
        &mut self,
        (owed, answer): crate::pipeline::Settled<Then>,
        reading: Option<Instant>,
        waiting: Instant,
    ) {
        let commit = owed.tag.is_commit();
        self.waits.ack(reading, commit);
        let rider = owed.rider;
        let other = (commit && !commit_refusal_is_an_answer(&owed.tag, &answer)).then_some("read");
        self.judge(owed.sent.max(waiting), answer.as_ref().map(drop), other);
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
                self.committed = self.committed.max(upto);
                self.commit_refused = false;
                if self
                    .inbox_pending
                    .is_some_and(|owed| owed <= self.committed)
                {
                    self.inbox_pending = None;
                }
            }
            (Then::Commit { upto, .. }, Err(e)) => {
                stderr_line!("aterm-link: commit through {upto} failed: {e}");
                self.commit_refused = self.conn.is_some();
            }
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
        let first = self.unsent == Some(seq);
        let waiting = self.fx.now();
        while self
            .conn
            .as_ref()
            .is_some_and(|link| link.unacked() >= PIPELINE_WINDOW)
            && self.settle_one(waiting)
        {}
        let producer_id = self.producer_id;
        self.note_remote_lane(subject);
        let Some(link) = self.conn.as_mut() else {
            return self.conclude(then, Err(broker_unreachable()));
        };
        let started = self.fx.now();
        let sent = link.send(producer_id, seq, first, subject, body, then, started);
        if first && self.unsent == Some(seq) {
            self.unsent = None;
        }
        if let Err((e, then)) = sent {
            self.judge(started, Err(&e), None);
            return self.conclude(then, Err(e));
        }
        if !self.in_round && self.ack_batch == 0 {
            self.settle_acks();
        }
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
                Item::Gap(source, g) => self.on_gap(source, g),
                Item::Resume(Source::Fleet, from) => {
                    self.resume_face(Source::Fleet, from);
                    if self.conn.is_none() {
                        break;
                    }
                }
                Item::Closed(source) => {
                    self.subscription_lost(source);
                    break;
                }
                Item::Refused(source, reason) => {
                    self.subscription_refused(source, reason);
                    break;
                }
                Item::Event(_) | Item::Inbox(_) | Item::Say(_) | Item::Resume(..) => {}
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
        let producer_id = self.producer_id;
        self.note_remote_lane(subject);
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
        let attempt = pin
            .identity
            .clone()
            .unwrap_or_else(|| publication_identity(subject, body));
        if publication_identity(&attempt.subject, body) != attempt {
            return Err(self.unverified_publication(format!(
                "reservation {} cannot reconstruct its pinned attempt; left queued",
                pin.seq
            )));
        }
        let (off, deduped) = self.publish_at(pin.seq, &attempt.subject, body)?;
        if !deduped {
            return Ok((off, false, Some(attempt)));
        }
        let legacy = pin.identity.is_none() || (pin.via_key && pin.original.is_none());
        let mut candidates = vec![attempt];
        if let Some(original) = &pin.original {
            candidates.push(original.clone());
        }
        let mut faces = BTreeSet::new();
        let mut exact_record = false;
        for candidate in &candidates {
            if !faces.insert(candidate.subject.clone()) {
                continue;
            }
            let fx = self.fx.clone();
            let conn = self
                .conn_now()
                .ok_or_else(|| io::Error::new(io::ErrorKind::NotConnected, "no broker"))?;
            let started = fx.now();
            let answer = read_back_rows(conn.fetch(off, &candidate.subject, 1), off);
            let rows = self
                .observe(started, answer, Some("read"))
                .map_err(|e| io::Error::new(e.kind(), PublicationReadFailure(e)))?;
            for (at, face, bytes) in rows {
                if at != off || face != candidate.subject {
                    continue;
                }
                exact_record = true;
                let actual = publication_identity(&face, &bytes);
                if candidates.contains(&actual) {
                    return Ok((off, true, Some(actual)));
                }
            }
        }
        if exact_record && !legacy {
            return Err(self.collision(pin.seq, off));
        }
        if legacy && self.state.self_acked().contains(&off) {
            return Ok((off, true, None));
        }
        let reason = if legacy {
            "legacy reservation has no verifiable original identity (a keyed re-post may differ)"
        } else {
            "record is not readable at its original offset and pinned face"
        };
        Err(self.unverified_publication(format!(
            "deduped reservation {} at @{off}: {reason}; left queued",
            pin.seq
        )))
    }

    fn unverified_publication(&mut self, reason: String) -> io::Error {
        self.link_down("read");
        self.conn = None;
        io::Error::new(io::ErrorKind::InvalidData, Answered(reason))
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
        if state == "live"
            && !self
                .per_sid
                .presence
                .get(sid)
                .is_some_and(|slot| slot.sampled)
        {
            if let Some(sample) = self.status_sample(sid) {
                self.status_into_presence(sid, &sample);
            }
            self.sample_meta(sid);
        }
        let subject = subject::session_face(&self.cfg.fleet, &self.node, sid, "presence");
        let epoch = self
            .per_sid
            .epochs
            .get(sid)
            .cloned()
            .unwrap_or_else(|| "-".into());
        let hold = u8::from(self.halt_applied);
        let gen = self.live_gen(sid).unwrap_or_else(|| "-".to_string());
        let fields = self.per_sid.presence.get(sid).map_or_else(
            || Fields::default().tokens(self.cfg.presence),
            |slot| slot.fields.tokens(self.cfg.presence),
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
            return match self.publish(&subject, body.as_bytes()) {
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
        let before = self.deadlines.len();
        self.deadlines.retain(|_, d| live.contains(&d.sid));
        if self.deadlines.len() != before {
            self.persist_deadlines();
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
        let filter = format!("/f/{}/in/{}/{sid}/>", self.cfg.fleet, self.node);
        let from = match self.state.seen_off(sid) {
            Some(off) => off + 1,
            None => {
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
        let mut cursor = from;
        loop {
            let page = {
                let fx = self.fx.clone();
                let Some(conn) = self.conn_now() else {
                    return false;
                };
                let started = fx.now();
                let answer = transport::fetch_kept(conn, cursor, &filter, 256);
                match self.observe(started, answer, Some("read")) {
                    Ok((p, floor)) => {
                        if let Some(floor) = floor {
                            stderr_line!(
                                "aterm-link: refill of {sid}: the broker's retention gave up \
                                 this lane's records below @{floor} (from @{cursor}); reading on \
                                 from there"
                            );
                        }
                        p
                    }
                    Err(e) => {
                        stderr_line!("aterm-link: refill of {sid} stopped: {e}");
                        return false;
                    }
                }
            };
            let (rows, (next, head)) = page;
            for (off, subj, body) in rows {
                match self.deliver_record(off, &subj, &body) {
                    Delivery::Accounted => {}
                    Delivery::Unaccounted => return false,
                    Delivery::Owed => return true,
                }
                if self.conn.is_none() {
                    return false;
                }
                self.service_urgent();
            }
            if next >= head {
                return true;
            }
            if next <= cursor {
                stderr_line!("aterm-link: refill of {sid} made no progress at {cursor}");
                return false;
            }
            cursor = next;
        }
    }

    fn on_inbox_record(&mut self, rec: &BrokerRecord) {
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
        let delivery = self.deliver_record(*off, subject, body);
        self.group_record = None;
        match delivery {
            Delivery::Accounted => {
                self.inbox_pending_records += 1;
                self.owe_inbox_commit(*off);
            }
            Delivery::Unaccounted => {
                stderr_line!(
                    "aterm-link: record {off} left for the next bridge: the connection to aterm \
                     is lost"
                );
                self.commit_floor = Some(self.commit_floor.map_or(*off, |have| have.min(*off)));
            }
            Delivery::Owed => {
                let floor = match subject::parse_in(&self.cfg.fleet, &self.node, subject) {
                    Ok(addr) => self.per_sid.lane_owed.entry(addr.sid).or_default(),
                    Err(_) => &mut self.commit_floor,
                };
                *floor = Some(floor.map_or(*off, |have| have.min(*off)));
            }
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

    fn flush_inbox_commit(&mut self) {
        self.inbox_pending_records = 0;
        let waiting = self.fx.now();
        while self.conn.as_ref().is_some_and(Link::riding) && self.settle_one(waiting) {}
        let Some(upto) = self.inbox_pending else {
            return;
        };
        if upto < self.committed || (upto == self.committed && upto != 0) {
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
        if upto < self.committed || (upto == self.committed && upto != 0) {
            return;
        }
        match self.state.set_inbox_owed(upto) {
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
            kind: body.kind.clone().unwrap_or_else(|| "note".to_string()),
        };
        let (kind, demoted, _stray) = self.classify_delivery(&addr, body);
        let (outcome, taken) = self.deliver_line_taking(&DeliverLine {
            addr: &addr,
            body,
            off,
            kind: &kind,
            demoted: demoted.as_deref(),
            late: false,
            topic: Some(&say.topic),
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

    fn deliver_record(&mut self, off: u64, subject: &str, raw: &[u8]) -> Delivery {
        // OBSERVER MODE delivers nothing (§11.2: "presence and `ev` only, no
        // delivery, no hold"). It is checked HERE rather than left to the
        // endpoint's refusal so an observer produces one honest record per
        // message instead of a stream of `ERR denied`.
        if self.attachment == Attachment::Observer {
            self.publish_ev_for(None, &format!("undeliverable off={off} reason=observer"));
            return Delivery::Accounted;
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
                stderr_line!(
                    "aterm-link: @{off} is not delivered while its in-flight entry stands (the \
                     state dir refused a write); reconnecting to offer it again"
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
        let (kind, demoted, stray) = self.classify_delivery(&addr, &body);
        let late = body
            .re
            .filter(|_| !stray && ANSWER_KINDS.contains(&kind.as_str()))
            .is_some_and(|re| {
                self.settle_deadline_for(&addr.sid, re);
                self.expired
                    .get(&re)
                    .is_some_and(|asker| *asker == addr.sid)
            });
        self.deliver_line(&DeliverLine {
            addr: &addr,
            body: &body,
            off,
            kind: &kind,
            demoted: demoted.as_deref(),
            late,
            topic: None,
        })
    }

    fn deliver_line(&mut self, row: &DeliverLine<'_>) -> Delivery {
        self.deliver_line_taking(row).0
    }

    fn deliver_line_taking(&mut self, row: &DeliverLine<'_>) -> (Delivery, Option<u64>) {
        let &DeliverLine {
            addr,
            body,
            off,
            kind,
            demoted,
            late,
            topic,
        } = row;
        if self.per_sid.lane_owed.contains_key(&addr.sid) {
            return (Delivery::Owed, None);
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
        if kind == "ack" {
            if let Some(v) = &body.verdict {
                line.push_str(&format!(" verdict={v}"));
            }
        }
        let Some(budget) = REQUEST_LINE_MAX.checked_sub(line.len() + " len= text=".len() + 20)
        else {
            return (
                self.refuse_delivery(addr, body, off, topic, "oversize"),
                None,
            );
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
            return (
                self.refuse_delivery(addr, body, off, topic, "oversize"),
                None,
            );
        }
        if self.fault == Fault::LoseAtermBeforeDeliver {
            self.fault = Fault::None;
            Fault::LoseAtermBeforeDeliver.fired(&self.state);
            let _ = self.ctl.get_ref().shutdown(std::net::Shutdown::Both);
        }
        let mut taken = None;
        let outcome = match self.ctl_request(&line) {
            Ok(reply) if reply.ok() => {
                taken = reply
                    .header()
                    .split_whitespace()
                    .nth(1)
                    .and_then(|n| n.parse::<u64>().ok());
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
        if cut && outcome == Delivery::Accounted && topic.is_none() {
            self.publish_ev_for(
                Some(&addr.sid),
                &format!(
                    "truncated off={off} len={} sent={}",
                    body.text.len(),
                    crate::pct::decode(&encoded).len()
                ),
            );
        }
        (outcome, taken)
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
        while forged_self(&self.self_acked, off)
            && self
                .conn
                .as_ref()
                .is_some_and(|link| link.owed().any(Then::own_lane_post))
            && self.settle_one(waiting)
        {}
        forged_self(&self.self_acked, off)
    }

    fn classify_kind(&self, addr: &subject::InAddr, body: &Body) -> (String, Option<String>) {
        if body.via.is_some() {
            return ("note".to_string(), Some(addr.kind.clone()));
        }
        if DEMOTE_UNLESS_ACCEPTED.contains(&addr.kind.as_str()) && !self.accepted(&addr.src) {
            return ("note".to_string(), Some(addr.kind.clone()));
        }
        (addr.kind.clone(), None)
    }

    fn classify_delivery(
        &mut self,
        addr: &subject::InAddr,
        body: &Body,
    ) -> (String, Option<String>, bool) {
        let stray = if addr.kind == "expired" {
            addr.src != self.node
        } else {
            ANSWER_KINDS.contains(&addr.kind.as_str())
                && body.re.is_some_and(|re| {
                    self.read_where_asks_went(re);
                    self.reply_from(re, &addr.src) == Some(false)
                })
        };
        if stray {
            return ("note".to_string(), Some(addr.kind.clone()), true);
        }
        let (kind, demoted) = self.classify_kind(addr, body);
        (kind, demoted, stray)
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
                    && (addr.src == self.node || self.may_speak_for(&addr.src, sid)) =>
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
        if VERDICT_KINDS.contains(&addr.kind.as_str()) {
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
                };
                self.publish_ev_for(
                    Some(&addr.sid),
                    &format!("unnotified off={off} src={} reason={reason}", addr.src),
                );
                return;
            }
        };
        let subject = format!("{prefix}/undeliverable");
        let mut notice = Body::new(self.fx.wall_ms());
        notice.re = Some(off);
        notice.from = Some(addr.sid.clone());
        notice.text = format!("not delivered: {why}");
        let encoded = notice.encode();
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
        if let Err(e) = published {
            stderr_line!("aterm-link: could not publish {subject}: {e}");
        }
    }

    fn send_receipt(&mut self, r: &OwedReceipt) -> io::Result<()> {
        if !self.cfg.receipts
            || !WAITING_KINDS.contains(&r.kind.as_str())
            || !crate::body::is_verdict(&r.verdict)
        {
            self.retire_receipt(r, None);
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
            self.retire_receipt(r, None);
            return Ok(());
        }
        let prefix = match self.receipt_lane(&r.from)? {
            Ok(prefix) => prefix,
            Err(no) => {
                let reason = match no {
                    NoLane::Undrained => "no-lane",
                    NoLane::Capped => "lane-cap",
                };
                self.publish_ev_for(
                    Some(&r.sid),
                    &format!("unacked re={} verdict={} reason={reason}", r.off, r.verdict),
                );
                self.retire_receipt(r, None);
                return Ok(());
            }
        };
        let subject = format!("{prefix}/ack");
        let encoded = receipt_body(self.fx.wall_ms(), &r.sid, r.off, &r.verdict).encode();
        let pinned = self.state.receipt_pin(&r.sid, r.rid)?;
        #[cfg(test)]
        let pinned = pinned.filter(|_| !self.fx.mutant(crate::sim::world::Mutant::FreshReceiptSeq));
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
                };
                self.state.set_receipt_pin(&r.sid, r.rid, &pin)?;
                pin
            }
        };
        let published = match self.staged_refusal(&r.sid) {
            Some(refused) => Err(refused),
            None => self.publish_journaled(&pin, &subject, &encoded),
        };
        let (at, deduped, _) = match published {
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
                self.state.clear_receipt_seq(&r.sid, r.rid);
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
                self.retire_receipt(r, None);
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
                self.retire_receipt(r, None);
                return Ok(());
            }
            answer => answer?,
        };
        if !deduped {
            self.publish_ev_for(
                Some(&r.sid),
                &format!("ack re={} off={at} verdict={}", r.off, r.verdict),
            );
        }
        self.retire_receipt(r, Some(at));
        Ok(())
    }

    fn retire_receipt(&mut self, r: &OwedReceipt, at: Option<u64>) {
        let off = at.map_or_else(|| "-".to_string(), |n| n.to_string());
        match self.ctl_request(&format!("deliver {} receipt={} off={off}", r.sid, r.rid)) {
            Ok(reply) if reply.ok() || reply.header().starts_with("ERR no such session") => {
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
            if !(sid.starts_with("s-")
                && subject::is_principal(sid)
                && node.starts_with("n-")
                && subject::is_principal(node))
            {
                return Ok(Err(NoLane::Undrained));
            }
            let lane = format!("/f/{fleet}/in/{node}/{sid}/{}", self.node);
            let allowed = self.may_answer_on(node, &lane)?;
            return Ok(allowed.then_some(lane).ok_or(NoLane::Capped));
        }
        if !subject::is_principal(from) {
            return Ok(Err(NoLane::Undrained));
        }
        if from.starts_with("s-") {
            return Ok(match self.resolve_to(&format!("@{from}"), None)? {
                Route::To(prefix) => Ok(prefix),
                _ => Err(NoLane::Undrained),
            });
        }
        if from.starts_with("n-") {
            return Ok(Err(NoLane::Undrained));
        }
        Ok(Ok(subject::person_lane(&fleet, from, &self.node)))
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
        let fx = self.fx.clone();
        let Some(conn) = self.conn_now() else {
            return Err("unreadable".into());
        };
        let started = fx.now();
        let page = conn.fetch(off, &filter, 1);
        let floor = page
            .as_ref()
            .err()
            .and_then(|e| transport::retained_past(e, off));
        let answer = read_back_rows(page, off);
        let rows = self
            .observe(started, answer, Some("read"))
            .map_err(|_| "unreadable")?;
        let Some((at, subj, raw)) = rows.into_iter().next() else {
            return Err(floor.map_or(Unfetched::Why("no-record"), Unfetched::Retained));
        };
        if at != off {
            return Err("no-record".into());
        }
        let addr =
            subject::parse_in(&self.cfg.fleet, &self.node, &subj).map_err(|_| "malformed")?;
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
        if kind == "ack" {
            if let Some(v) = &body.verdict {
                fields.push_str(&format!(" verdict={v}"));
            }
        }
        Ok((fields, body.text))
    }

    fn sender_lane(&mut self, addr: &subject::InAddr, body: &Body) -> Result<String, NoLane> {
        let fleet = self.cfg.fleet.clone();
        if addr.src.starts_with("s-") {
            return match self.resolve_to(&format!("@{}", addr.src), None) {
                Ok(Route::To(prefix)) => Ok(prefix),
                _ => Err(NoLane::Undrained),
            };
        }
        if addr.src.starts_with("n-") {
            let sid = body
                .from
                .as_deref()
                .filter(|s| s.starts_with("s-") && subject::is_principal(s))
                .ok_or(NoLane::Undrained)?;
            let lane = format!("/f/{fleet}/in/{}/{sid}/{}", addr.src, self.node);
            return match self.may_answer_on(&addr.src, &lane) {
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
        let face = pin
            .identity
            .as_ref()
            .map_or(subject, |identity| identity.subject.as_str())
            .to_string();
        let journaled = self.on_own_lane(&face);
        if journaled && self.conn.is_none() {
            return Err(broker_unreachable());
        }
        if journaled {
            self.state.set_in_flight(&InFlight {
                pin: pin.clone(),
                subject: face,
                body: body.to_vec(),
                log: self.conn.as_ref().and_then(|conn| conn.seen_log_identity()),
            })?;
        }
        let published = match answers {
            Some(upto) => self
                .publish_answer(pin.seq, subject, body, upto)
                .map(|(off, deduped)| (off, deduped, None)),
            None => self.publish_claimed(pin, subject, body),
        };
        match &published {
            Ok((off, _, actual)) => {
                let ours = actual
                    .as_ref()
                    .map_or(journaled, |identity| self.on_own_lane(&identity.subject));
                if journaled {
                    self.settle_landed(pin.seq, *off, ours);
                } else if ours {
                    let _ = self.remember_self_ack(*off);
                }
            }
            Err(e) if journaled && !self.fate_unknown(e) => self.drop_in_flight(pin.seq),
            Err(_) => {}
        }
        published
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
        let producer_id = self.producer_id;
        let group = subject::inbox_group(&self.cfg.fleet, &self.node);
        let fx = self.fx.clone();
        let Some(conn) = self.conn_now() else {
            return Err(io::Error::new(
                io::ErrorKind::NotConnected,
                "the broker is unreachable",
            ));
        };
        let started = fx.now();
        let answer = astream_broker::ack(conn, producer_id, &group, upto, subject, body);
        let (off, deduped) = self.observe(started, answer, None)?;
        self.bus_high = self.bus_high.max(off.saturating_add(1));
        if !deduped {
            self.committed = self.committed.max(upto);
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

    fn fate_unknown(&self, e: &io::Error) -> bool {
        self.conn.is_none() && !e.get_ref().is_some_and(|inner| inner.is::<Answered>())
    }

    fn settle_in_flight(&mut self) -> bool {
        let settled = self.replay_in_flight();
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
        for (seq, entry) in entries {
            if Body::decode(&entry.body).0.re.is_some() {
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
            match self.publish_claimed(&entry.pin, &entry.subject, &entry.body) {
                Ok((off, _, actual)) => {
                    let ours = actual
                        .as_ref()
                        .is_none_or(|identity| self.on_own_lane(&identity.subject));
                    self.land_replayed(seq, off, ours);
                }
                Err(e) if self.fate_unknown(&e) => {
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
                    self.drop_in_flight(seq);
                    if self.conn.is_none() {
                        return false;
                    }
                }
            }
        }
        true
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

    fn note_deadline(&mut self, sid: &str, off: u64, dl: u64, to: Option<String>) {
        if self.deadlines.contains_key(&off) {
            return;
        }
        let was_empty = self.deadlines.is_empty();
        self.deadlines.insert(
            off,
            Deadline {
                off,
                sid: sid.to_string(),
                at: self.fx.wall_ms().saturating_add(dl),
                dl,
                to,
            },
        );
        while self.deadlines.len() > DEADLINES_KEEP {
            self.deadlines.pop_first();
        }
        self.arm_deadline_sweep(was_empty, false);
        self.persist_deadlines();
    }

    fn note_asked(&mut self, off: u64, to: &str) {
        self.asked.insert(off, to.to_string());
        while self.asked.len() > ASKED_KEEP {
            self.asked.pop_first();
        }
        let list: Vec<Asked> = self
            .asked
            .iter()
            .map(|(off, to)| Asked {
                off: *off,
                to: to.clone(),
            })
            .collect();
        if let Err(e) = self.state.set_asked(&list) {
            stderr_line!("aterm-link: could not persist the asked table: {e}");
        }
    }

    fn reply_from(&self, off: u64, src: &str) -> Option<bool> {
        self.asked
            .get(&off)
            .cloned()
            .or_else(|| self.deadlines.get(&off).and_then(|d| d.to.clone()))
            .map(|to| to == src)
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

    fn settle_deadline_for(&mut self, sid: &str, off: u64) {
        if self.deadlines.get(&off).is_some_and(|d| d.sid == sid) {
            self.deadlines.remove(&off);
            self.persist_deadlines();
        }
    }

    fn persist_deadlines(&mut self) {
        let list: Vec<Deadline> = self.deadlines.values().cloned().collect();
        if let Err(e) = self.state.set_deadlines(&list) {
            stderr_line!("aterm-link: could not persist the deadline table: {e}");
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
                self.deadlines.remove(&d.off);
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
                    self.deadlines.remove(&d.off);
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
                            self.deadlines.remove(&d.off);
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
            self.persist_deadlines();
        }
    }

    fn settled_on_bus(&mut self, sid: &str, off: u64) -> io::Result<Option<&'static str>> {
        let filter = format!("/f/{}/in/{}/{sid}/>", self.cfg.fleet, self.node);
        let mut cursor = off.saturating_add(1);
        loop {
            let fx = self.fx.clone();
            let Some(conn) = self.conn_now() else {
                return Err(io::Error::new(
                    io::ErrorKind::NotConnected,
                    "the broker is unreachable",
                ));
            };
            let started = fx.now();
            let answer = transport::fetch_kept(conn, cursor, &filter, 256);
            let ((rows, (next, head)), floor) = self.observe(started, answer, Some("read"))?;
            if let Some(floor) = floor {
                stderr_line!(
                    "aterm-link: the deadline of @{off} on {sid}'s lane: the broker's retention \
                     gave up its records below @{floor}; judged on what it keeps"
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
                let from_right = if kind == "expired" {
                    src == self.node
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

    fn land_replayed(&mut self, seq: u64, off: u64, ours: bool) {
        #[cfg(test)]
        let at_once = self.deliver_replay_at_once
            || self
                .fx
                .mutant(crate::sim::world::Mutant::DeliverBeforeTheDrain);
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

    fn release_replays_by_a_peek(&mut self) {
        if self.awaiting_drain.is_empty() {
            return;
        }
        const LEGACY_OUTBOX_USAGE: &str = "ERR usage: outbox [<max>] | outbox sent <sid> <post-id> off=<n|-> [dup=1] [reason=<word>]";
        let reply = match self.ctl_request("outbox earlier-receipts=1") {
            Ok(reply) if reply.header() == LEGACY_OUTBOX_USAGE => self.ctl_request("outbox"),
            answer => answer,
        };
        match reply {
            Ok(reply) if reply.ok() => {
                let drain = parse_drain(reply.body());
                self.release_replays(&drain);
            }
            Ok(_) => self.release_replays(&Drain::default()),
            Err(_) => {}
        }
    }

    fn release_replays(&mut self, drain: &Drain) {
        if self.awaiting_drain.is_empty() || drain.more {
            return;
        }
        let pinned = (|| -> io::Result<BTreeSet<u64>> {
            let mut pinned = BTreeSet::new();
            for post in &drain.posts {
                if let Some(pin) = self.state.post_pin(&post.sid, post.id)? {
                    pinned.insert(pin.seq);
                }
                if let Some(key) = post.key.as_deref() {
                    if let Some(pin) = self.state.key_pin(&post.sid, key)? {
                        pinned.insert(pin.seq);
                    }
                }
            }
            for r in &drain.receipts {
                if let Some(pin) = self.state.receipt_pin(&r.sid, r.rid)? {
                    pinned.insert(pin.seq);
                }
            }
            Ok(pinned)
        })();
        let Ok(pinned) = pinned else {
            return;
        };
        let free: Vec<(u64, u64, bool)> = self
            .awaiting_drain
            .iter()
            .filter(|(seq, _)| !pinned.contains(seq))
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

    fn remember_self_ack(&mut self, off: u64) -> io::Result<()> {
        #[cfg(test)]
        let below_too = self
            .fx
            .mutant(crate::sim::world::Mutant::RememberBelowTheWindow);
        #[cfg(not(test))]
        let below_too = false;
        if !below_too && self.self_acked.first().is_some_and(|oldest| off < *oldest) {
            return Ok(());
        }
        if !self.self_acked.insert(off) && self.acked_appendable {
            return Ok(());
        }
        while self.self_acked.len() > SELF_ACK_KEEP {
            self.self_acked.pop_first();
        }
        let compact = !self.acked_appendable || self.acked_entries >= 2 * SELF_ACK_KEEP;
        let written = if compact {
            self.state
                .set_self_acked(&self.self_acked, SELF_ACK_KEEP)
                .map(|()| self.self_acked.len())
        } else {
            self.state
                .append_self_acked(off)
                .map(|()| self.acked_entries + 1)
        };
        match written {
            Ok(entries) => {
                self.acked_entries = entries;
                self.acked_appendable = true;
                Ok(())
            }
            Err(e) => {
                self.acked_appendable = false;
                stderr_line!("aterm-link: could not record @{off} as this node's own: {e}");
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
                (below > self.committed).then_some(below)
            }
            _ => Some(off),
        }
    }

    fn inbox_commit_floor(&self) -> Option<u64> {
        self.per_sid
            .lane_owed
            .values()
            .filter_map(|off| *off)
            .chain(self.commit_floor)
            .min()
    }

    fn on_fleet_record(&mut self, rec: &BrokerRecord) {
        self.unyielding(|bridge| bridge.take_fleet_record(rec));
    }

    fn take_fleet_record(&mut self, rec: &BrokerRecord) {
        let (off, subject, raw) = rec;
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
        let keep_below = keep_below.map(|f| f.min(head));
        let kept = |off: u64| keep_below.is_some_and(|f| off < f);
        let dropped = self
            .state
            .forget_log_offsets(head, log, keep_below, lineage)?;
        #[cfg(test)]
        let forget_pin = !self
            .fx
            .mutant(crate::sim::world::Mutant::KeepMirrorPinAcrossMove);
        #[cfg(not(test))]
        let forget_pin = true;
        if forget_pin {
            self.mirror_forget_log();
        }
        for (seq, face) in &dropped {
            stderr_line!(
                "aterm-link: the in-flight record at sequence {seq} on {face} was decided on \
                 another log; it is not published on this one"
            );
        }
        self.unsettled.retain(|off, _| kept(*off));
        self.awaiting_drain.retain(|_, (off, _)| kept(*off));
        let before = self.deadlines.len();
        let acked = self.state.self_acked_log();
        self.self_acked = newest(acked.offsets, SELF_ACK_KEEP);
        self.acked_entries = acked.entries;
        self.acked_appendable = acked.appendable;
        self.deadlines.retain(|off, _| kept(*off));
        self.asked.retain(|off, _| kept(*off));
        self.expired.retain(|off, _| kept(*off));
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
        self.commit_floor = self.commit_floor.filter(|o| kept(*o));
        self.indexed_subjects.clear();
        self.seen_nodes.clear();
        self.committed = 0;
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
                    .or_else(|| since.strip_prefix('@').and_then(|n| n.parse::<u64>().ok()))
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
            .and_then(|n| n.parse::<u64>().ok());
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
            let offset = since.strip_prefix('@').and_then(|n| n.parse::<u64>().ok());
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
        let fx = self.fx.clone();
        let Some(conn) = self.conn_now() else {
            return false;
        };
        let started = fx.now();
        let page = transport::fetch_kept(conn, from, &filter, records);
        let Ok(((rows, (next, _)), floor)) = self.observe(started, page, Some("read")) else {
            return false;
        };
        if let Some(floor) = floor {
            stderr_line!(
                "aterm-link: the broadcast topic {topic}: the broker's retention gave up its \
                 records below @{floor} (from @{from}); replaying from there"
            );
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
        let fields = &mut self
            .per_sid
            .presence
            .entry(sid.to_string())
            .or_default()
            .fields;
        sample.apply_presence(fields);
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

    fn live_gen(&mut self, sid: &str) -> Option<String> {
        let reply = self.ctl_request(&format!("@{sid} text --json")).ok()?;
        gen_of_frame(reply.rows().first()?)
    }

    fn staged_refusal(&self, sid: &str) -> Option<io::Error> {
        if self.fault != Fault::FailPostPublishWhileMarked {
            return None;
        }
        let marked = std::fs::read_to_string(self.state.root().join("fail-post-publish")).ok()?;
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

    fn drain_outbox(&mut self) {
        if let Some(peeked) = self.drain_whole() {
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
        let waiting = self.fx.now();
        while !peek_past
            && self
                .conn
                .as_ref()
                .is_some_and(|link| link.owed().any(|then| then.post().is_some()))
            && self.settle_one(waiting)
        {}
        self.conn.as_ref()?;
        self.held_senders.clear();
        let peeked = self.mailbox.event_mark();
        const LEGACY_OUTBOX_USAGE: &str = "ERR usage: outbox [<max>] | outbox sent <sid> <post-id> off=<n|-> [dup=1] [reason=<word>]";
        let reply = match self.ctl_request("outbox earlier-receipts=1") {
            Ok(reply) if reply.header() == LEGACY_OUTBOX_USAGE => self.ctl_request("outbox"),
            answer => answer,
        };
        let drained = match reply {
            Ok(reply) if reply.ok() => reply.body().to_vec(),
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
        self.waits.count(
            crate::wait_tally::Row::DrainPosts,
            u64::try_from(drain.posts.len()).unwrap_or(u64::MAX),
        );
        self.release_replays(&drain);
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
        if let Err(e) = self.refresh_sessions() {
            stderr_line!("aterm-link: the roster could not be re-read before a drain: {e}");
            return None;
        }
        let mut stalled: BTreeSet<String> = BTreeSet::new();
        for receipt in drain.receipts {
            if self.aterm_gone() {
                return None;
            }
            if stalled.contains(&receipt.sid) {
                continue;
            }
            if let Err(e) = self.send_receipt(&receipt) {
                stderr_line!(
                    "aterm-link: could not publish the receipt for {} ({}): {e}",
                    receipt.off,
                    receipt.sid
                );
                self.conn.as_ref()?;
                stalled.insert(receipt.sid.clone());
            }
        }
        let finished = self.batched(|bridge| bridge.drain_posts(drain.posts));
        (finished && stalled.is_empty() && self.held_senders.is_empty())
            .then_some(whole)
            .flatten()
    }

    fn drain_posts(&mut self, posts: Vec<QueuedPost>) -> bool {
        for post in posts {
            self.service_urgent();
            if self.aterm_gone() {
                return false;
            }
            #[cfg(test)]
            let settled = self.fx.mutant(crate::sim::world::Mutant::PipelineSender)
                || self.settle_sender(&post.sid);
            #[cfg(not(test))]
            let settled = self.settle_sender(&post.sid);
            if !settled || self.aterm_gone() {
                return false;
            }
            if self.held_senders.contains(&post.sid) {
                continue;
            }
            if !self.drain_post(post) {
                return false;
            }
        }
        true
    }

    fn drain_post(&mut self, post: QueuedPost) -> bool {
        let routed = match self.resolve_to(&post.to, Some(&post.sid)) {
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
                if existing_pin.is_none() && keyed.is_none() && !self.budget_allows(&subject, opens)
                {
                    self.retire_at_the_door(&post, SUBJECT_BUDGET);
                    return true;
                }
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
                        },
                        Some(keyed) => PublishPin {
                            seq: keyed.seq,
                            via_key: true,
                            identity: Some(publication_identity(&subject, &encoded)),
                            original: keyed.identity,
                            original_off: keyed.original_off,
                            owner: keyed.owner,
                        },
                        None => {
                            let pin = PublishPin {
                                seq: self.next_seq()?,
                                via_key: false,
                                identity: Some(publication_identity(&subject, &encoded)),
                                original: None,
                                original_off: None,
                                owner: Some(post.id),
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
                let published = match staged {
                    Some(refused) => Err(refused),
                    None => self.publish_journaled(&pin, &subject, &encoded),
                };
                let next = match published {
                    Ok((off, deduped, actual)) => {
                        self.post_landed(&post, &subject, &pin, off, deduped, actual)
                    }
                    Err(e) => self.post_refused(&post, &subject, pin.seq, e),
                };
                if !self.follow(&post.sid, next) {
                    return false;
                }
            }
            route => self.retire_at_the_door(&post, route.reason()),
        }
        true
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
        #[cfg(test)]
        let pipelined = self.fx.mutant(crate::sim::world::Mutant::PipelineSender);
        #[cfg(not(test))]
        let pipelined = false;
        debug_assert!(
            pipelined
                || !self
                    .conn
                    .as_ref()
                    .is_some_and(|link| link.owed().any(|then| then.posted_by(&post.sid))),
            "one pipelined post in flight per sender ({})",
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
        let unsent = self.unsent.take();
        let record = self.group_record.take();
        let next = match outcome {
            Ok(off) => {
                if journaled {
                    self.settle_landed(pin.seq, off, true);
                }
                let identity = pin.identity.clone();
                self.post_landed(&post, &subject, &pin, off, false, identity)
            }
            Err(e) => {
                if journaled && !self.fate_unknown(&e) {
                    self.drop_in_flight(pin.seq);
                }
                self.post_refused(&post, &subject, pin.seq, e)
            }
        };
        if next != PostNext::Moves {
            self.held_senders.insert(post.sid.clone());
        }
        self.unsent = unsent;
        self.group_record = record;
    }

    fn post_landed(
        &mut self,
        post: &QueuedPost,
        subject: &str,
        pin: &PublishPin,
        off: u64,
        deduped: bool,
        actual: Option<PublishIdentity>,
    ) -> PostNext {
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
            return PostNext::Holds;
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
                return PostNext::Holds;
            }
        }
        let _ = self.remember_self_ack(off);
        self.indexed_subjects.insert(subject.to_string());
        let to = actual
            .as_ref()
            .and_then(|identity| replier_of(&identity.subject));
        if appended && WAITING_KINDS.contains(&post.kind.as_str()) {
            if let Some(to) = &to {
                self.note_asked(off, to);
            }
        }
        if let Some(dl) = post
            .dl
            .filter(|_| appended && WAITING_KINDS.contains(&post.kind.as_str()))
        {
            self.note_deadline(&post.sid, off, dl, to);
        }
        let dup = if via_key && deduped && !appended {
            " dup=1"
        } else {
            ""
        };
        if self.fault == Fault::ParkRetireWhileMarked {
            self.park_retire_while_marked();
        }
        let retired = self.ctl_request(&format!(
            "outbox sent {} {} off={off}{dup}",
            post.sid, post.id
        ));
        if retired.is_ok_and(|r| r.ok()) {
            self.state.clear_post_seq(&post.sid, post.id);
        }
        PostNext::Moves
    }

    fn post_refused(
        &mut self,
        post: &QueuedPost,
        subject: &str,
        seq: u64,
        e: io::Error,
    ) -> PostNext {
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
        if refused_for_good(&e) {
            self.retire_at_the_door(post, "refused");
            PostNext::Moves
        } else if past_the_budget(&e) && self.writes_alone(subject) {
            self.indexed_subjects.remove(subject);
            self.retire_at_the_door(post, SUBJECT_BUDGET);
            PostNext::Moves
        } else if self.conn.is_none() {
            PostNext::Ends
        } else {
            PostNext::Holds
        }
    }

    fn retire_at_the_door(&mut self, post: &QueuedPost, why: &str) {
        let retired = self.ctl_request(&format!(
            "outbox sent {} {} off=- reason={why}",
            post.sid, post.id
        ));
        if retired.is_ok_and(|r| r.ok()) {
            self.state.clear_post_seq(&post.sid, post.id);
        }
        self.publish_ev(&format!("undeliverable to={} reason={why}", post.to));
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

    fn budget_allows(&mut self, subject: &str, opens: Opens) -> bool {
        if self.indexed_subjects.contains(subject) {
            return true;
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
        let budget = match self.observe(started, answer, None) {
            Ok(Some(budget)) => budget,
            Ok(None) | Err(_) => return true,
        };
        let reserve = opens.reserve(budget.max);
        if budget.remaining() > reserve {
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
        stderr_line!(
            "aterm-link: the post to {subject} would open a subject with {} of this \
             node's {} left: the last {reserve} are kept for {}",
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
            if !subject::is_principal(&sid) || !sid.starts_with("s-") {
                return Ok(Route::Unroutable);
            }
            let mut hosted = self.per_sid.epochs.contains_key(&sid);
            if !hosted {
                let _ = self.refresh_sessions();
                hosted = self.per_sid.epochs.contains_key(&sid);
            }
            let node = match (node, hosted) {
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
        Ok(if subject::is_principal(&to) {
            Route::To(subject::person_lane(&fleet, &to, &self.node))
        } else {
            Route::Unroutable
        })
    }

    fn pinned_node_for(&mut self, sid: &str) -> io::Result<Result<String, Route>> {
        let pinned = self.state.pins().get(sid).cloned();
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
            Some("EVENT") => self.mirror_wake(),
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
                if self
                    .per_sid
                    .presence
                    .get(sid)
                    .is_some_and(|slot| slot.sampled)
                {
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
                let Some(sid) = target.parse::<u64>().ok().and_then(|l| self.locals.get(&l)) else {
                    return;
                };
                let sid = sid.clone();
                let named: Option<u64> = toks.next().and_then(|t| t.parse().ok());
                let mut off: Option<u64> = None;
                let mut floor: Option<Option<u64>> = None;
                let mut watermark: Option<u64> = None;
                let mut decided = false;
                for t in toks {
                    if let Some(n) = t.strip_prefix("off=") {
                        off = n.parse().ok();
                    } else if let Some(f) = t.strip_prefix("floor=") {
                        floor = Some(f.parse().ok());
                    } else if let Some(w) = t.strip_prefix("seen=") {
                        watermark = w.parse().ok();
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
                let Some(sid) = target.parse::<u64>().ok().and_then(|l| self.locals.get(&l)) else {
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
                let Some(sid) = target.parse::<u64>().ok().and_then(|l| self.locals.get(&l)) else {
                    return;
                };
                let sid = sid.clone();
                self.topic_change(&sid, &mut toks);
            }
            "agent" => {
                let Some(sid) = target.parse::<u64>().ok().and_then(|l| self.locals.get(&l)) else {
                    return;
                };
                let sid = sid.clone();
                let word = toks.next();
                self.per_sid
                    .presence
                    .entry(sid.clone())
                    .or_default()
                    .fields
                    .set_agent(word);
                self.publish_presence_if_due(&sid);
            }
            "meta" => {
                // See [`Fault::DropMetaEventWhileMarked`].
                if self.fault == Fault::DropMetaEventWhileMarked
                    && self.state.root().join("drop-meta-event").exists()
                {
                    return;
                }
                let Some(sid) = target.parse::<u64>().ok().and_then(|l| self.locals.get(&l)) else {
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
                let _ = self.refresh_sessions();
            }
            _ => {}
        }
    }

    fn take_bye(&mut self, bye: Bye) {
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
        self.apply_fleet_halts(halts, fleet_from);
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
        let sids: Vec<String> = self.locals.values().cloned().collect();
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
            self.drain_outbox();
            if self.conn.is_none() {
                return false;
            }
        }
        if !self.refill_pending_admissions(false) {
            return false;
        }
        let handed_over = self
            .state
            .inbox_owed()
            .filter(|_| self.attachment != Attachment::Observer);
        if let Some(owed) = handed_over {
            self.inbox_pending = Some(self.inbox_pending.map_or(owed, |have| have.max(owed)));
        }
        self.land_inbox_commit();
        if self.conn.is_none() {
            return false;
        }
        if self.inbox_pending.is_none() && handed_over.is_some() {
            if let Err(e) = self.state.clear_inbox_owed() {
                stderr_line!("aterm-link: could not clear the inbox commit handed over: {e}");
            }
        }
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

    fn subscription_lost(&mut self, source: Source) {
        let face = face_name(source);
        if self.conn.is_none() {
            stderr_line!("aterm-link: the broker's {face} subscription ended; reconnecting");
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
                "aterm-link: the broker ended the {face} subscription and still answers \
                 ({reason}); reconnecting every subscription"
            );
            self.link_down(reason);
        } else {
            stderr_line!(
                "aterm-link: the broker's {face} subscription ended and the broker does not \
                 answer; reconnecting"
            );
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
            astream_broker::IDLE_MARK_DEADLINE,
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

    /// At a stop, the post journal's records written as the files an older build reads
    /// ([`StateDir::hand_posts_to_files`]), for whichever bridge runs next. Whether they were.
    fn hand_posts_to_files(&self) -> bool {
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
        let own = aterm_types::version::build_id();
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
        self.endpoint_lane = field("lane").and_then(|n| n.parse().ok()).unwrap_or(0);
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

    /// Unlink the files an earlier boot's sessions left in the state dir, keeping those of every
    /// session the roster just read ([`StateDir::sweep_sessions_of_an_earlier_boot`]); once, at
    /// the start, after its recovery tried the sweep of torn files. A roster that did not read
    /// sweeps nothing, and what is left goes at the next boot's start.
    fn sweep_sessions_of_an_earlier_boot(&mut self) {
        let live: BTreeSet<String> = self.locals.values().cloned().collect();
        match self.state.sweep_sessions_of_an_earlier_boot(&live) {
            Ok(0) => {}
            Ok(n) => stderr_line!(
                "aterm-link: the last bridge ran in an earlier boot: {n} files of the sessions \
                 that boot ended (keys/, acks/, unhandled/, seen/, topics/) removed from the \
                 state dir"
            ),
            Err(e) => stderr_line!(
                "aterm-link: the files of the sessions an earlier boot ended could not all be \
                 removed from the state dir: {e}; what is left goes at the next boot's start"
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
        self.mirror_step();
        self.settle_owed();
        let wait = self.park_wait(self.fx.now());
        self.park_and_take(wait)
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

    fn duty_wait(&self, now: Instant) -> Duration {
        idle_wait(
            now,
            self.roster_due,
            (!self.deadlines.is_empty()).then_some(self.deadline_due),
            self.sweep_owed().then_some(self.ghost_due),
        )
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

    fn settle_owed(&mut self) {
        if !self.mailbox.inbox_queued() {
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
            while self
                .conn
                .as_ref()
                .is_some_and(|link| link.owed().any(waited))
                && self.settle_one(waiting)
            {}
            self.try_settle_acks();
            self.tally_idle();
        }
    }

    fn tally_idle(&mut self) {
        let (mailbox, state) = (&self.mailbox, &self.state);
        self.waits.idle(self.committed, |t| {
            mailbox.copy_wakes(t);
            state.copy_dir_rows(t);
        });
    }

    fn park_and_take(&mut self, wait: Duration) -> bool {
        let owed = self.conn.as_ref().is_some_and(|link| link.unacked() > 0);
        let park = if owed { wait.min(IDLE_ACK_GRACE) } else { wait };
        let (go_on, took) = self.take_one(park);
        if go_on && !took && owed {
            self.settle_acks();
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
        let took = item.is_some();
        self.in_round = took;
        let go_on = match item {
            Some(Item::Fleet(r)) => {
                self.on_fleet_record(&r);
                true
            }
            Some(Item::Event(line)) => {
                self.on_event(&line);
                true
            }
            Some(Item::Inbox(r)) => {
                self.on_inbox_record(&r);
                true
            }
            Some(Item::Say(r)) => {
                self.on_say_record(&r);
                true
            }
            Some(Item::Closed(Source::Aterm)) => {
                self.in_round = false;
                self.on_aterm_closed();
                stderr_line!("aterm-link: aterm closed the bridge connection; exiting");
                self.wind_down();
                false
            }
            Some(Item::Closed(source)) => {
                self.subscription_lost(source);
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

    /// Returns the uncommitted inbox offset and log identity currently owed by this bridge.
    pub(crate) fn owed_inbox_commit(&self) -> Option<(u64, Option<astream_broker::LogIdentity>)> {
        self.inbox_pending.map(|upto| (upto, self.bus_log))
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

fn plain_pin(seq: u64, subject: &str, body: &[u8]) -> PublishPin {
    PublishPin {
        seq,
        via_key: false,
        identity: Some(publication_identity(subject, body)),
        original: None,
        original_off: None,
        owner: None,
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
    let (woken, wake) = std::io::pipe()?;
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
    let local = toks.next()?.parse::<u64>().ok()?;
    let sid = toks.next().filter(|s| subject::is_principal(s))?;
    Some((local, sid))
}

fn bye_of(line: &str) -> Option<Bye> {
    let mut toks = line.split_whitespace();
    if (toks.next(), toks.next(), toks.next()) != (Some("EVENT"), Some("*"), Some("bye")) {
        return None;
    }
    Bye::parse(toks.next()?)
}

fn forged_self(acked: &BTreeSet<u64>, off: u64) -> bool {
    if acked.contains(&off) {
        return false;
    }
    !matches!(acked.first(), Some(oldest) if off < *oldest)
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
}

struct DeliverLine<'a> {
    addr: &'a subject::InAddr,
    body: &'a Body,
    off: u64,
    kind: &'a str,
    demoted: Option<&'a str>,
    late: bool,
    topic: Option<&'a str>,
}

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
    /// Returns the `reason=` refusal token for [`Self::Unroutable`] or [`Self::Ambiguous`].
    #[must_use]
    pub const fn reason(&self) -> &'static str {
        match self {
            Route::Ambiguous => "ambiguous",
            Route::To(_) | Route::Exact(_) | Route::Unroutable => "unroutable",
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

fn span_of(ms: u64) -> String {
    if ms >= 1_000 && ms.is_multiple_of(1_000) {
        format!("{} s", ms / 1_000)
    } else {
        format!("{ms} ms")
    }
}

/// Extracts the `<content_seq>:<fp16>` generation token (`serial=`) from a `text --json` frame.
#[must_use]
pub fn gen_of_frame(frame: &str) -> Option<String> {
    let seq = frame
        .split(",\"seq\":")
        .nth(1)?
        .split(|c: char| !c.is_ascii_digit())
        .next()?
        .parse::<u64>()
        .ok()?;
    let rows = frame
        .strip_prefix("{\"rows\":[")?
        .split_once("],\"cursor\":")?
        .0;
    Some(format!("{seq}:{:016x}", fnv1a_64(rows.as_bytes())))
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
        .get("inc")?
        .parse()
        .ok()
        .filter(|inc| *inc <= INC_MAX)
}

fn carried_elsewhere(body: &Body, inc: u64, self_inc: u64, host: &str) -> Option<String> {
    if inc == self_inc || body.unknown.get("state").map(String::as_str) != Some("live") {
        return None;
    }
    let row_host = body.unknown.get("host").map(|h| crate::pct::decode(h));
    let row_pid = body.unknown.get("pid").and_then(|p| p.parse::<u32>().ok());
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

fn ring_refusal(fresh: &[Cap], held: &[Cap], node: &str) -> Option<String> {
    let binds = ring_binds(fresh);
    if let Some(other) = binds.iter().find(|n| n.as_str() != node) {
        return Some(format!("binds {other}, not this bridge's node {node}"));
    }
    (ring_binds(held).contains(node) && !binds.contains(node))
        .then(|| format!("binds no write for this bridge's node {node}"))
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
    let (mut sid, mut rid, mut off, mut verdict, mut kind, mut from) =
        (None, None, None, None, None, None);
    let mut earlier_log = false;
    for tok in line.split_whitespace() {
        match tok.split_once('=') {
            Some(("sid", v)) => sid = Some(v),
            Some(("rid", v)) => rid = v.parse::<u64>().ok(),
            Some(("off", v)) => off = v.parse::<u64>().ok(),
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
                    off = n.parse().ok();
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
                "id" => post.id = v.parse().unwrap_or(0),
                "to" => post.to = v.to_string(),
                "kind" => post.kind = v.to_string(),
                "re" => post.re = v.parse().ok(),
                "dl" => post.dl = v.parse().ok(),
                "via" => post.via = Some(v.to_string()),
                "key" => post.key = Some(v.to_string()),
                "len" => len = v.parse().ok(),
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

/// Spawns a background thread pumping a broker subscription into `mailbox` for generation `gen`.
pub(crate) fn spawn_reader(
    mut sub: astream_broker::AnySubscription,
    closer: Arc<BusCloser>,
    deadline: Duration,
    source: Source,
    mailbox: Arc<Mailbox>,
) -> io::Result<()> {
    let generation = mailbox.broker_generation(source);
    std::thread::Builder::new()
        .name(format!("aterm-link-{source:?}"))
        .spawn(move || loop {
            let read = next_took(&mut sub, &closer, deadline);
            if !reader_took(read, &mailbox, source, generation, &closer) {
                return;
            }
        })
        .map(drop)
}

/// One read a subscription's reader hands on ([`reader_took`]).
#[derive(Debug)]
pub(crate) enum Took {
    /// A record of the subscription's filter.
    Record(BrokerRecord),
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
            }) => return Ok(Some(Took::Record((offset, subject, body)))),
            Some(astream_broker::Event::Gap(g)) => return Ok(Some(Took::Gap(g))),
        }
    }
}

/// Routes one subscription read result into `mailbox` under incarnation `gen`, returning `true` to continue.
pub(crate) fn reader_took(
    read: io::Result<Option<Took>>,
    mailbox: &Mailbox,
    source: Source,
    generation: u64,
    closer: &BusCloser,
) -> bool {
    match read {
        Ok(Some(Took::Gap(g))) => {
            if mailbox.push_gap(source, g, generation) {
                true
            } else {
                closer.close();
                false
            }
        }
        Ok(Some(Took::Record(rec))) => match mailbox.push(source, rec, generation) {
            Pushed::Queued => true,
            Pushed::Paused | Pushed::Stale => {
                closer.close();
                false
            }
        },
        Err(e) if astream_broker::broker_refusal(&e).is_some() => {
            mailbox.push_refused(source, generation, link_reason(&e, "subscribe"));
            false
        }
        Ok(None) | Err(_) => {
            mailbox.push_closed(source, generation);
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
mod tests {
    use super::*;

    #[test]
    fn receipt_and_expired_bodies_are_what_astream_reads() {
        use astream_wire::{Offset, ReceiptError, ReceiptVerdict, V1Receipt};
        use std::collections::BTreeSet;
        const T: u64 = 1_700_000_000_999;
        let aterm_words: BTreeSet<&str> = crate::body::VERDICTS.into_iter().collect();
        let astream_words: BTreeSet<&str> =
            ReceiptVerdict::ALL.iter().map(|v| v.as_str()).collect();
        assert_eq!(
            aterm_words.len(),
            crate::body::VERDICTS.len(),
            "no word twice"
        );
        assert_eq!(
            aterm_words, astream_words,
            "aterm's verdict words are astream's, both ways"
        );
        for verdict in ReceiptVerdict::ALL {
            let word = verdict.as_str();
            let wire = receipt_body(T, "s-host", 41, word).encode();
            let line = String::from_utf8_lossy(&wire).into_owned();
            assert_eq!(
                V1Receipt::decode("ack", &wire),
                Ok(V1Receipt::Ack {
                    re: Offset(41),
                    verdict: Some(verdict),
                    sender_t_ms: Some(T),
                }),
                "{line}"
            );
            let (ours, _) = Body::decode(&wire);
            assert_eq!(
                (ours.re, ours.verdict.as_deref(), ours.from.as_deref()),
                (Some(41), Some(word), Some("s-host")),
                "{line}"
            );
        }
        let posted = |dl: Option<u64>, via: Option<&str>| QueuedPost {
            sid: "s-b".into(),
            id: 1,
            to: "s-a".into(),
            kind: "ack".into(),
            re: Some(41),
            dl,
            via: via.map(str::to_string),
            key: None,
            body: b"got it".to_vec(),
        };
        for post in [posted(None, None), posted(Some(30_000), Some("h-x"))] {
            let wire = post_body(T, &post, None).encode();
            let line = String::from_utf8_lossy(&wire).into_owned();
            assert_eq!(
                V1Receipt::decode(&post.kind, &wire),
                Ok(V1Receipt::Ack {
                    re: Offset(41),
                    verdict: None,
                    sender_t_ms: Some(T),
                }),
                "{line}"
            );
            let (ours, _) = Body::decode(&wire);
            assert_eq!(
                (ours.re, ours.verdict, ours.kind, ours.text.as_str()),
                (Some(41), None, None, "got it"),
                "{line}"
            );
        }
        for (dl, span) in [(240_000, "240 s"), (1_500, "1500 ms")] {
            let wire = expired_body(T, 7, dl).encode();
            let line = String::from_utf8_lossy(&wire).into_owned();
            assert_eq!(
                V1Receipt::decode("expired", &wire),
                Ok(V1Receipt::Expired {
                    re: Offset(7),
                    dl_ms: dl,
                    sender_t_ms: Some(T),
                }),
                "{line}"
            );
            let (ours, _) = Body::decode(&wire);
            let text = format!("no reply within {span}");
            assert_eq!(
                (ours.re, ours.dl, ours.text.as_str()),
                (Some(7), Some(dl), text.as_str()),
                "{line}"
            );
        }
        let mut receipt = receipt_body(T, "s-host", 41, "handled");
        receipt.re = None;
        let mut post = posted(None, None);
        post.re = None;
        let unanswering = post_body(T, &post, None);
        let mut expired = expired_body(T, 7, 240_000);
        expired.re = None;
        for (kind, body) in [("ack", receipt), ("ack", unanswering), ("expired", expired)] {
            assert_eq!(
                V1Receipt::decode(kind, &body.encode()),
                Err(ReceiptError::MissingOrDuplicateField("re")),
                "{kind}"
            );
        }
    }

    #[test]
    fn replay_scheduler_refines_shared_budget_and_rotation() {
        let model = aterm_models_session::say_replay_budget_model();
        let mut state = model.init_state();
        let now = Instant::now();
        let mut replay = SayReplay::new(now);
        let owed = BTreeSet::from(["s-a".to_string(), "s-b".to_string()]);

        assert!(model.fire("Control", &mut state));
        for _ in 0..SAY_REPLAY_PAGES {
            let sid = replay.take(&owed).expect("one shared page remains");
            let action = if sid == "s-a" { "FetchA" } else { "FetchB" };
            assert!(model.action_enabled(action, &state), "{action}: {state:?}");
            assert!(model.fire(action, &mut state));
        }
        assert_eq!((state["got_a"], state["got_b"]), (2, 2));
        replay.refill(now + roster_refresh() - Duration::from_millis(1));
        assert!(replay.take(&owed).is_none());

        let old = aterm_spec::interp::with_buggy(&model, 1);
        let mut unfair = old.init_state();
        assert!(old.fire("Control", &mut unfair));
        assert!(old.fire("FetchA", &mut unfair));
        assert!(old.fire("FetchA", &mut unfair));
        assert!(!old.check_invariant("FairRotation", &unfair));

        replay.refill(now + roster_refresh());
        assert!(model.fire("Refill", &mut state));
        for _ in 0..SAY_REPLAY_PAGES {
            let sid = replay.take(&owed).expect("the next interval has pages");
            let action = if sid == "s-a" { "FetchA" } else { "FetchB" };
            assert!(model.action_enabled(action, &state), "{action}: {state:?}");
            assert!(model.fire(action, &mut state));
        }
        assert_eq!((state["got_a"], state["got_b"]), (4, 4));

        let mailbox = Mailbox::default();
        let _ = mailbox.push(Source::Inbox, (1, "mail".to_string(), Vec::new()), 0);
        let mut queued = model.init_state();
        assert!(model.fire("Control", &mut queued));
        assert!(model.fire("QueuePriority", &mut queued));
        assert!(
            !may_take_replay_page(&mailbox, false, 0),
            "a pushed opt-in cannot fetch ahead of queued mail"
        );
        assert!(may_take_replay_page(&mailbox, true, 0));
        assert!(model.action_enabled("FetchA", &queued));
        assert!(model.fire("FetchA", &mut queued));
        assert_eq!(
            may_take_replay_page(&mailbox, true, 1),
            model.action_enabled("FetchB", &queued),
            "the roster yields after one page"
        );
        assert!(matches!(mailbox.take(Duration::ZERO), Some(Item::Inbox(_))));
        assert!(model.fire("HandlePriority", &mut queued));
        assert_eq!(
            may_take_replay_page(&mailbox, false, 0),
            model.action_enabled("FetchB", &queued),
            "replay resumes once addressed mail was handled"
        );
        let _ = mailbox.push(Source::Fleet, (2, "halt".to_string(), Vec::new()), 0);
        assert!(
            !may_take_replay_page(&mailbox, true, 0),
            "even the roster's one-page exception must yield to a halt"
        );

        let quiet = Mailbox::default();
        let mut pushed = model.init_state();
        assert!(model.fire("Control", &mut pushed));
        assert!(model.fire("BeginPush", &mut pushed));
        assert_eq!(
            may_take_replay_page(&quiet, false, 0),
            model.action_enabled("FetchA", &pushed)
        );
        assert!(model.fire("FetchA", &mut pushed));
        assert_eq!(
            may_take_replay_page(&quiet, false, 1),
            model.action_enabled("FetchB", &pushed),
            "a pushed add cannot use a second page in the same event"
        );
        assert!(model.fire("EndPush", &mut pushed));
        assert!(model.fire("BeginPush", &mut pushed));
        assert_eq!(
            may_take_replay_page(&quiet, false, 0),
            model.action_enabled("FetchB", &pushed)
        );
    }

    #[test]
    fn many_backlogged_sessions_get_one_turn_before_any_get_a_second() {
        let now = Instant::now();
        let mut replay = SayReplay::new(now);
        let owed: BTreeSet<String> = (0..12).map(|n| format!("s-{n:02}")).collect();
        let mut served = Vec::new();
        for round in 0..3 {
            replay.refill(now + roster_refresh() * round);
            for _ in 0..SAY_REPLAY_PAGES {
                served.push(replay.take(&owed).expect("a page remains"));
            }
            assert!(
                replay.take(&owed).is_none(),
                "round {round} exceeds its Fetch bound"
            );
        }
        assert_eq!(served.into_iter().collect::<BTreeSet<_>>(), owed);

        let mut reduced = owed;
        reduced.remove("s-11");
        replay.refill(now + roster_refresh() * 3);
        assert_eq!(replay.take(&reduced).as_deref(), Some("s-00"));
    }

    #[test]
    fn bridge_roster_accepts_only_complete_unique_identity_rows() {
        let a = "0123456789abcdef0123456789abcdef";
        let b = "fedcba9876543210fedcba9876543210";
        let rows = [
            format!("9 s-nine nonce={a}"),
            format!("2 s-two nonce={b} role=worker"),
        ];
        let (locals, epochs) = parse_bridge_roster(rows.iter().map(String::as_str)).unwrap();
        assert_eq!(locals.get(&2).map(String::as_str), Some("s-two"));
        assert_eq!(locals.get(&9).map(String::as_str), Some("s-nine"));
        assert_eq!(epochs.get("s-two").map(String::as_str), Some(b));
        assert_eq!(epochs.get("s-nine").map(String::as_str), Some(a));

        for bad in [
            "2 s-two",
            "2 s-two nonce=-",
            "2 s-two nonce=FFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFF",
            "2 s-two nonce=0123456789abcdef0123456789abcdef my title",
            "2 s-two nonce=0123456789abcdef0123456789abcdef =1",
            "2 title s-two nonce=0123456789abcdef0123456789abcdef",
            "not-a-local s-two nonce=0123456789abcdef0123456789abcdef",
        ] {
            assert!(parse_bridge_roster(std::iter::once(bad)).is_err(), "{bad}");
        }
        for duplicate in [
            [format!("2 s-two nonce={a}"), format!("2 s-new nonce={b}")],
            [format!("2 s-two nonce={a}"), format!("3 s-two nonce={b}")],
        ] {
            assert!(
                parse_bridge_roster(duplicate.iter().map(String::as_str)).is_err(),
                "duplicate local id or sid cannot be admitted"
            );
        }
    }

    #[test]
    fn status_batch_keeps_each_session_and_incarnation_separate() {
        let locals = BTreeMap::from([
            (1, "s-one".to_string()),
            (2, "s-two".to_string()),
            (3, "s-three".to_string()),
        ]);
        let epochs = BTreeMap::from([
            ("s-one".to_string(), "a1".to_string()),
            ("s-two".to_string(), "b2".to_string()),
            ("s-three".to_string(), "c3".to_string()),
        ]);
        let rows = [
            "1 s-one a1 sid=1 revision=7 hold=1 hold_origin=local detail=Claude%20Code \
             agent=prompt",
            "2 s-two b2 sid=2 revision=4 hold=0 hold_origin=- detail=- agent=-",
            "3 s-three NEW sid=3 revision=99 hold=0 detail=new",
        ];
        let parsed = parse_batch_rows(rows.into_iter(), &locals, &epochs);
        assert!(matches!(
            parsed.get(&1),
            Some(BatchSample::Ready(StatusSample {
                hold: EndpointHold::Local,
                detail,
                agent: Some(agent),
                agent_deferred: false,
            })) if detail == "Claude%20Code" && agent == "prompt"
        ));
        assert!(matches!(
            parsed.get(&2),
            Some(BatchSample::Ready(StatusSample {
                hold: EndpointHold::Free,
                detail,
                agent: Some(agent),
                agent_deferred: false,
            })) if detail == "-" && agent == "-"
        ));
        assert!(matches!(parsed.get(&3), Some(BatchSample::Stale)));

        let wrong_sid = ["2 s-other b2 sid=2 revision=88 hold=1 detail=other"];
        let parsed = parse_batch_rows(wrong_sid.into_iter(), &locals, &epochs);
        assert!(matches!(parsed.get(&2), Some(BatchSample::Stale)));
    }

    #[test]
    fn batch_agent_deferral_keeps_the_verdict_and_malformed_rows_retry_singly() {
        let locals = BTreeMap::from([(1, "s-one".to_string())]);
        let epochs = BTreeMap::from([("s-one".to_string(), "a1".to_string())]);
        let row = "1 s-one a1 sid=1 revision=7 hold=1 detail=Claude%20Code agent_deferred=1";
        let mut parsed = parse_batch_rows(std::iter::once(row), &locals, &epochs);
        let Some(BatchSample::Ready(sample)) = parsed.remove(&1) else {
            panic!("a deferred agent is a usable hold/detail sample");
        };
        assert_eq!(sample.hold, EndpointHold::Unsaid);
        assert_eq!(sample.agent, None);
        assert!(sample.agent_deferred);
        let mut fields = Fields::default();
        fields.set_agent(Some("prompt"));
        sample.apply_presence(&mut fields);
        assert_eq!(fields.detail, "Claude%20Code");
        assert_eq!(fields.phase, "prompt", "a busy writer is not agent=-");
        StatusSample::default().apply_presence(&mut fields);
        assert_eq!(
            fields.phase, "-",
            "an old single-status reply without agent= still clears the phase"
        );

        for malformed in [
            "1 s-one a1 sid=1 revision=7 hold=1 detail=- agent_deferred=1 agent=idle",
            "1 s-one a1 sid=1 revision=7 hold=1 detail=- agent_deferred=1 agent_deferred=1",
            "1 s-one a1 sid=1 revision=7 hold=1 detail=- agent_deferred=0",
            "1 s-one a1 sid=1 revision=7 hold=1 detail=Claude Code agent=prompt",
            "1 s-one a1 sid=1 revision=4 hold=0 agent=idle",
            "1 s-one a1 sid=1 revision=7 hold=1 detail=-",
        ] {
            assert!(
                parse_batch_rows(std::iter::once(malformed), &locals, &epochs).is_empty(),
                "a malformed row must retry singly: {malformed}"
            );
        }
    }

    #[test]
    fn a_batch_row_says_whose_hold_stands() {
        let locals = BTreeMap::from([(1, "s-one".to_string())]);
        let epochs = BTreeMap::from([("s-one".to_string(), "a1".to_string())]);
        let parse = |fields: &str| {
            let row = format!("1 s-one a1 sid=1 revision=7 {fields} detail=- agent=-");
            match parse_batch_rows(std::iter::once(row.as_str()), &locals, &epochs).remove(&1) {
                Some(BatchSample::Ready(sample)) => Some(sample.hold),
                Some(BatchSample::Stale) => panic!("{row}: not a reused id"),
                None => None,
            }
        };
        assert_eq!(parse("hold=1 hold_origin=fleet"), Some(EndpointHold::Fleet));
        assert_eq!(parse("hold=1 hold_origin=local"), Some(EndpointHold::Local));
        assert_eq!(
            parse("hold=1 hold_origin=person"),
            Some(EndpointHold::Local)
        );
        assert_eq!(
            parse("hold=1 hold_origin=person,fleet,local"),
            Some(EndpointHold::Both),
        );
        assert!(!EndpointHold::of(true, Some("person")).fleet());
        assert!(EndpointHold::of(true, Some("person,fleet")).fleet());
        assert_eq!(
            parse("hold=1 hold_origin=remote"),
            Some(EndpointHold::Local)
        );
        assert_eq!(
            parse("hold=1 hold_origin=remote,fleet"),
            Some(EndpointHold::Both)
        );
        assert_eq!(
            parse("hold=1 hold_origin=person,remote,fleet,local"),
            Some(EndpointHold::Both)
        );
        assert!(!EndpointHold::of(true, Some("remote")).fleet());
        assert!(EndpointHold::of(true, Some("remote,fleet")).fleet());
        assert_eq!(parse("hold=1 hold_origin=person,person"), None);
        assert_eq!(parse("hold=1 hold_origin=someone"), None);
        assert_eq!(
            parse("hold=1 hold_origin=fleet,local"),
            Some(EndpointHold::Both),
            "both slots stand"
        );
        assert_eq!(parse("hold=0 hold_origin=-"), Some(EndpointHold::Free));
        assert_eq!(parse("hold=1"), Some(EndpointHold::Unsaid));
        assert_eq!(parse("hold=0"), Some(EndpointHold::Free));
        for malformed in [
            "hold=1 hold_origin=-",
            "hold=0 hold_origin=local",
            "hold=0 hold_origin=fleet",
            "hold=1 hold_origin=owner",
            "hold=1 hold_origin=local hold_origin=local",
            "hold=0 hold_origin=fleet,local",
            "hold=1 hold_origin=local,fleet",
            "hold=1 hold_origin=fleet,fleet",
            "hold=1 hold_origin=fleet,",
            "hold=1 hold_origin=local,person",
            "hold=1 hold_origin=fleet,remote",
            "hold=1 hold_origin=remote,remote",
            "hold=0 hold_origin=remote",
        ] {
            assert_eq!(parse(malformed), None, "{malformed}");
        }
        let duplicate = [
            "1 s-one a1 sid=1 revision=7 hold=1 detail=- agent=prompt",
            "1 s-one a1 sid=1 revision=8 hold=0 detail=- agent=idle",
        ];
        let parsed = parse_batch_rows(duplicate.into_iter(), &locals, &epochs);
        assert!(
            matches!(parsed.get(&1), Some(BatchSample::Stale)),
            "a duplicate row fails closed"
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_refused_status_batch_never_fans_out() {
        use std::io::{BufRead as _, Write as _};
        use std::os::unix::net::UnixListener;
        use std::sync::atomic::{AtomicU64, Ordering};

        static NEXT: AtomicU64 = AtomicU64::new(0);
        for batch_reply in [
            "ERR main thread did not answer within the placement deadline\n",
            "ERR usage: sessions\n",
        ] {
            let scratch = std::env::temp_dir().join(format!(
                "al-status-batch-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            std::fs::create_dir_all(&scratch).unwrap();
            let socket = scratch.join("ctl.sock");
            let listener = UnixListener::bind(&socket).unwrap();
            let responder = std::thread::spawn(move || {
                let (mut stream, _) = listener.accept().unwrap();
                let mut reader = std::io::BufReader::new(stream.try_clone().unwrap());
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                assert_eq!(line, "AUTH test\n");
                line.clear();
                reader.read_line(&mut line).unwrap();
                assert_eq!(line, "outbox\n");
                stream.write_all(b"OK 0\n").unwrap();

                let mut requests = Vec::new();
                loop {
                    line.clear();
                    if reader.read_line(&mut line).unwrap() == 0 {
                        break;
                    }
                    requests.push(line.trim_end().to_string());
                    if line == "sessions status\n" {
                        stream.write_all(batch_reply.as_bytes()).unwrap();
                    } else {
                        panic!("unexpected control request: {line}");
                    }
                }
                requests
            });

            let mut bridge = Bridge::new(Config {
                fleet: "test".into(),
                broker: String::new(),
                cap_files: Vec::new(),
                state_dir: scratch.join("state").to_string_lossy().into_owned(),
                accept_from: Vec::new(),
                sock: Some(socket.to_string_lossy().into_owned()),
                token: Some("test".into()),
                presence: Mode::Meta,
                receipts: true,
                log: crate::mirror::LogMode::Off,
            })
            .unwrap();
            bridge.attachment = Attachment::Inherited;
            for (local, sid) in [(1, "s-one"), (2, "s-two"), (3, "s-three")] {
                bridge.locals.insert(local, sid.to_string());
                bridge
                    .per_sid
                    .epochs
                    .insert(sid.to_string(), "nonce".to_string());
                bridge.per_sid.pending_admit.insert(sid.to_string());
                let fields = &mut bridge
                    .per_sid
                    .presence
                    .entry(sid.to_string())
                    .or_default()
                    .fields;
                fields.set_detail(Some("old"));
                fields.set_agent(Some("prompt"));
            }
            bridge.sample_local_control();
            for sid in ["s-one", "s-two", "s-three"] {
                let fields = &bridge.per_sid.presence[sid].fields;
                assert_eq!(fields.detail, "old", "{batch_reply:?}");
                assert_eq!(fields.phase, "prompt", "{batch_reply:?}");
            }
            drop(bridge);
            let requests = responder.join().unwrap();
            assert_eq!(requests, ["sessions status"], "{batch_reply:?}");
            std::fs::remove_dir_all(scratch).unwrap();
        }
    }

    #[test]
    fn busy_local_events_cannot_accelerate_a_failed_broker_retry() {
        let model = aterm_models_session::fabric_reconnect_backoff_model();
        let mut retry = model.init_state();
        let mailbox = Mailbox::default();
        for i in 0..64 {
            mailbox.push_event(format!("EVENT 1 title {i}"));
            if i < 2 {
                assert!(model.fire("Event", &mut retry));
            }
        }
        assert_eq!(retry["result"], 0, "local events do not enable a retry");
        let mut seen = 0;
        assert!(!wait_for_retry_or_aterm_close(
            &Fx::os(),
            &mailbox,
            Duration::from_millis(250),
            |_| seen += 1
        ));
        assert_eq!(
            seen, 64,
            "queued local events must not turn the reconnect backoff into a rapid dial loop"
        );
        assert!(model.fire("Deadline", &mut retry));
        assert_eq!(retry["result"], 1, "the real wait reached its deadline");

        let mut closed = model.init_state();
        for i in 0..64 {
            mailbox.push_event(format!("EVENT 1 bell {i}"));
            if i < 2 {
                assert!(model.fire("Event", &mut closed));
            }
        }
        mailbox.push_aterm_closed();
        const RETRY: Duration = Duration::from_secs(300);
        let start = Instant::now();
        assert!(wait_for_retry_or_aterm_close(
            &Fx::os(),
            &mailbox,
            RETRY,
            |_| {}
        ));
        assert!(
            start.elapsed() < aterm_spec::wait::MUST_HAPPEN,
            "a closed aterm lane must interrupt the backoff promptly"
        );
        assert!(model.fire("Closed", &mut closed));
        assert_eq!(closed["result"], 2);
        assert!(!model.action_enabled("Deadline", &closed));
    }

    #[test]
    fn idle_bridge_arms_only_the_earliest_deadline_and_retries_failed_due_work() {
        let now = Instant::now();
        let roster = now + ROSTER_REFRESH;
        let model = aterm_models_core::idle_deadline_model();
        let mut arm = model.init_state();
        let no_asks = deadline_wake(now, 1_000, None, roster, false);
        assert_eq!(no_asks, None);
        assert_eq!(
            idle_wait(now, roster, no_asks, None),
            ROSTER_REFRESH,
            "no ask or ghost needs the old 250 ms wake"
        );
        let far = deadline_wake(now, 1_000, Some(3_601_000), roster, false).unwrap();
        assert_eq!(far, roster, "a far ask shares the two-second roster wake");
        assert!(model.fire("ArmFar", &mut arm));
        assert_eq!(arm["armed"], 2);
        let near = deadline_wake(now, 1_000, Some(1_010), roster, false).unwrap();
        assert_eq!(near, now + Duration::from_millis(10));
        assert!(model.fire("ArmNear", &mut arm));
        assert_eq!(arm["armed"], 1);
        assert!(model.check_invariant("EarliestArmed", &arm));
        assert_eq!(
            idle_wait(now, roster, Some(far.min(near)), None),
            Duration::from_millis(10),
            "a new earlier ask moves the arm forward immediately"
        );
        let mut keep_first = arm.clone();
        keep_first.insert("armed", 2);
        assert!(
            !model.check_invariant("EarliestArmed", &keep_first),
            "a stale far arm after a near ask is the caught negative control"
        );
        assert_eq!(
            deadline_wake(now, 1_000, Some(1_000), roster, false),
            Some(now),
            "an already-due ask is checked on the next loop iteration"
        );
        assert_eq!(
            deadline_wake(now, 1_000, Some(1_000), roster, true),
            Some(now + DEADLINE_RETRY),
            "a failed due check backs off instead of spinning"
        );
        assert_eq!(
            deadline_wake(
                roster,
                3_601_000,
                Some(3_601_000),
                roster + ROSTER_REFRESH,
                false
            ),
            Some(roster),
            "the roster round notices a forward wall-clock jump"
        );
        assert_eq!(
            idle_wait(now, roster, None, Some(now + Duration::from_millis(100))),
            Duration::from_millis(100),
            "a ghost sweep due sooner than the roster still wakes on time"
        );
        assert_eq!(idle_wait(now, now, None, None), Duration::ZERO);
    }

    #[cfg(unix)]
    #[test]
    fn bridge_deadlines_restore_then_move_the_shipping_arm_from_far_to_near() {
        let dir = aterm_tempfile::Builder::new()
            .prefix("al-dl")
            .tempdir()
            .expect("the fixture dir");
        let state_path = dir.path().join("state");
        StateDir::open(&state_path)
            .unwrap()
            .set_deadlines(&[Deadline {
                off: 1,
                sid: "s-one".into(),
                at: 0,
                dl: 1_000,
                to: None,
            }])
            .unwrap();
        let (_lane, mut bridge) = FakeVerbLane::bridge_with_state(&[], Some(&state_path));
        assert!(
            bridge.deadlines.contains_key(&1),
            "the old ask was restored"
        );
        bridge.roster_due = Instant::now() + Duration::from_secs(60);
        bridge.arm_deadline_sweep(true, false);
        assert!(
            bridge.deadline_due <= Instant::now(),
            "run's startup arm must check a restored overdue ask immediately"
        );

        bridge.deadlines.clear();
        bridge.note_deadline("s-one", 2, 3_600_000, None);
        let far = bridge.deadline_due;
        bridge.note_deadline("s-one", 3, 0, None);
        assert!(
            bridge.deadline_due < far,
            "a nearer ask replaces the far arm"
        );
    }

    #[test]
    fn moving_irrelevant_cursors_get_one_checkpoint_per_256_broadcasts() {
        let mut topics = (0..64)
            .map(|i| {
                (
                    format!("s-{i:03}"),
                    BTreeMap::from([("quiet".to_string(), TopicCursor { next: 10 })]),
                )
            })
            .collect::<BTreeMap<_, _>>();
        let mut counts = BTreeMap::<String, usize>::new();
        for tick in 1..=SAY_CURSOR_CHECKPOINT_SHARDS {
            let checkpoint = advance_frontier_cursors(
                &mut topics,
                &BTreeMap::new(),
                9 + tick,
                10 + tick,
                tick % SAY_CURSOR_CHECKPOINT_SHARDS,
            );
            assert!(checkpoint.len() <= 1, "tick {tick}: {checkpoint:?}");
            for sid in checkpoint {
                *counts.entry(sid).or_default() += 1;
            }
        }
        assert!(topics.values().all(|t| t["quiet"].next == 266));
        assert!(
            topics.keys().all(|sid| counts.get(sid) == Some(&1)),
            "each continuously moving cursor was checkpointed exactly once: {counts:?}"
        );
    }

    #[test]
    fn only_this_incarnation_its_dirs_earlier_ones_and_a_witnessed_dead_one_are_adoptable() {
        let (a_live, b) = (1, 2);
        assert!(
            !adoptable(Some(a_live), b, None, false),
            "an older sibling that is still running had its sessions retired"
        );
        assert!(adoptable(Some(b), b, None, true));
        assert!(
            !adoptable(Some(b), b, None, false),
            "a sibling can concurrently take the same incarnation: its writer mark is required"
        );
        assert!(adoptable(Some(a_live), b, Some(a_live), false));
        assert!(
            !adoptable(Some(1), 3, Some(2), false),
            "`inc <= witnessed` is the generous reading, and it is not proven"
        );
        assert!(adoptable(Some(1), 3, None, true));
        assert!(
            !adoptable(Some(1), 3, None, false),
            "a number this dir may have taken too says nothing of whose row it is"
        );
        assert!(!adoptable(Some(9), b, None, false));
        assert!(!adoptable(Some(9), b, Some(a_live), true));
        assert!(!adoptable(None, b, Some(a_live), true));
        assert!(!adoptable(None, b, None, false));
    }

    #[test]
    fn a_bus_incarnation_past_the_will_space_is_not_adopted() {
        let inc = |raw: &str| presence_inc(&Body::decode(raw.as_bytes()).0);
        assert_eq!(inc("v=1 t=1 state=gone inc=7"), Some(7));
        assert_eq!(
            inc(&format!("v=1 t=1 state=live inc={INC_MAX}")),
            Some(INC_MAX)
        );
        assert_eq!(
            inc(&format!("v=1 t=1 state=gone inc={}", INC_MAX + 1)),
            None
        );
        assert_eq!(inc("v=1 t=1 state=gone inc=3000000000"), None);
        assert_eq!(inc(&format!("v=1 t=1 state=gone inc={}", u64::MAX)), None);
        assert_eq!(inc("v=1 t=1 state=gone inc=x"), None);

        use crate::state::next_incarnation;
        assert_eq!(next_incarnation(5, 3), Some(6));
        assert_eq!(next_incarnation(0, INC_MAX - 1), Some(INC_MAX));
        assert_eq!(next_incarnation(INC_MAX, 0), None, "a corrupt local inc");
        assert_eq!(next_incarnation(u64::MAX, 0), None, "checked, not wrapped");
    }

    #[test]
    fn every_per_sid_map_is_pruned_to_the_roster() {
        const LIVE: &str = "s-live";
        const GONE: &str = "s-gone";
        let mut maps = PerSession::default();
        {
            let PerSession {
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
            } = &mut maps;
            for sid in [LIVE, GONE] {
                let sid = sid.to_string();
                epochs.insert(sid.clone(), "e".to_string());
                pending_admit.insert(sid.clone());
                topics.insert(sid.clone(), BTreeMap::new());
                unhandled.insert(sid.clone(), BTreeMap::new());
                topic_floors_dirty.insert(sid.clone());
                handled_to.insert(sid.clone(), 1);
                topics_sampled.insert(sid.clone());
                watched.insert(sid.clone(), 1);
                presence.insert(sid.clone(), Slot::default());
                lane_owed.insert(sid, None);
            }
        }
        maps.prune_to(&BTreeSet::from([LIVE.to_string()]));
        let PerSession {
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
        } = &maps;
        let keys: [(&str, Vec<&String>); 10] = [
            ("epochs", epochs.keys().collect()),
            ("pending_admit", pending_admit.iter().collect()),
            ("topics", topics.keys().collect()),
            ("unhandled", unhandled.keys().collect()),
            ("topic_floors_dirty", topic_floors_dirty.iter().collect()),
            ("handled_to", handled_to.keys().collect()),
            ("topics_sampled", topics_sampled.iter().collect()),
            ("watched", watched.keys().collect()),
            ("presence", presence.keys().collect()),
            ("lane_owed", lane_owed.keys().collect()),
        ];
        for (name, sids) in keys {
            assert_eq!(
                sids,
                [LIVE],
                "`{name}` must keep the live session and drop the departed one"
            );
        }
    }

    #[test]
    fn the_node_row_carries_a_real_host_name() {
        let askable = std::path::Path::new("/usr/bin/uname").exists()
            || std::path::Path::new("/bin/uname").exists()
            || std::path::Path::new("/etc/hostname").exists();
        if !askable {
            aterm_spec::host::could_not_run(
                "no uname and no /etc/hostname: this host cannot be asked its name",
            );
        }
        let host = hostname();
        assert_ne!(
            host, "-",
            "the host name is readable on this machine, so the presence row must carry it"
        );
        assert!(
            !host.is_empty() && !host.contains(char::is_whitespace),
            "{host:?}"
        );
    }

    #[test]
    fn a_drain_frame_carries_receipts_and_fetches_after_the_posts() {
        let frame = b"post sid=s-a id=1 to=%40s-b kind=ask re=7 len=8\nline\none\
                      receipt sid=s-a rid=3 off=40 verdict=handled kind=ask from=s-z@n-q\n\
                      receipt sid=bad rid=4 off=40 verdict=handled kind=ask from=s-z@n-q\n\
                      receipt sid=s-a rid=0 off=40 verdict=handled kind=ask from=s-z@n-q\n\
                      receipt sid=s-a rid=5 off=40 kind=ask from=s-z@n-q\n\
                      receipt sid=s-a rid=6 off=12 verdict=deferred kind=task from=h-x \
                      log=earlier\n\
                      receipt sid=s-a rid=7 off=12 verdict=deferred kind=task from=h-x log=a.0\n\
                      fetch sid=s-a off=41\nfetch sid=s-b off=7\nfetch sid=bad off=9\n\
                      more\n";
        let drain = parse_drain(frame);
        assert_eq!(drain.posts.len(), 1);
        assert!(drain.more, "the last line says the listing was cut");
        assert_eq!(
            drain.posts[0].body, b"line\none",
            "a body may hold newlines"
        );
        assert_eq!(drain.posts[0].re, Some(7));
        assert!(parse_drain(b"post sid=s-a id=1 to=x kind=note len=99\nhi")
            .posts
            .is_empty());
        let owed = OwedReceipt {
            sid: "s-a".to_string(),
            rid: 3,
            off: 40,
            verdict: "handled".to_string(),
            kind: "ask".to_string(),
            from: "s-z@n-q".to_string(),
            earlier_log: false,
        };
        let earlier = OwedReceipt {
            rid: 6,
            off: 12,
            verdict: "deferred".to_string(),
            kind: "task".to_string(),
            from: "h-x".to_string(),
            earlier_log: true,
            ..owed.clone()
        };
        let unknown = OwedReceipt {
            rid: 7,
            earlier_log: false,
            ..earlier.clone()
        };
        assert_eq!(
            drain.receipts,
            vec![owed, earlier, unknown],
            "a bad sid, a zero rid and a missing verdict are each dropped; `log=earlier` is \
             read, and no other `log=` word means it"
        );
        assert_eq!(
            drain.fetches,
            vec![("s-a".to_string(), 41), ("s-b".to_string(), 7)],
            "a sid that is not a principal is dropped, never spliced into a filter"
        );
        let ahead = b"fetch sid=s-a off=41\npost sid=s-a id=1 to=%40s-b kind=note len=0\n";
        let drain = parse_drain(ahead);
        assert_eq!((drain.posts.len(), drain.fetches.len()), (1, 1));
        assert!(!drain.more, "no `more` line, a whole listing");
    }

    #[test]
    fn a_fetch_below_the_lanes_retained_floor_names_the_floor() {
        const SID: &str = "s-kept";
        let (lane, mut bridge, _server, broker) = rig_with_kept_broker(&[(1, SID)], false);
        let node = bridge.node.clone();
        let subject = format!("/f/f/in/{node}/{SID}/{node}/note");
        let gone = bridge
            .publish_own_off(&subject, b"v=1 t=1 from=s-gone text=first")
            .unwrap();
        let kept = bridge
            .publish_own_off(&subject, b"v=1 t=1 from=s-gone text=second")
            .unwrap();
        assert!(
            broker.retain_before_carrying_state(kept).unwrap() > 0,
            "the cut"
        );

        bridge.answer_fetch(SID, gone);
        bridge.answer_fetch(SID, kept);
        bridge.answer_fetch(SID, kept + 1);
        let lines = lane.deliveries.lock().unwrap().clone();
        let first = |off: u64| {
            let head = format!("deliver {SID} fetched={off} ");
            lines
                .iter()
                .find_map(|l| l.strip_prefix(&head))
                .unwrap_or_else(|| panic!("no answer for {off}: {lines:?}"))
                .to_string()
        };
        let floor: u64 = first(gone)
            .strip_prefix("err=retained floor=")
            .unwrap_or_else(|| panic!("the lane's floor: {lines:?}"))
            .parse()
            .expect("a floor");
        assert!(gone < floor && floor <= kept, "{gone} < {floor} <= {kept}");
        let whole = first(kept);
        assert!(
            whole.starts_with("from=") && whole.ends_with("text=second"),
            "{lines:?}"
        );
        assert_eq!(first(kept + 1), "err=no-record");
    }

    #[test]
    fn fetched_lines_carry_a_body_whole_in_chunks_and_cut_one_over_the_bound() {
        let fields = "from=s-a@n-b kind=note trust=agent";
        let decode = |lines: &[String]| -> String {
            let mut body = String::new();
            for (i, line) in lines.iter().enumerate() {
                let at: usize = line
                    .split_whitespace()
                    .find_map(|t| t.strip_prefix("at="))
                    .map_or(0, |n| n.parse().expect("at="));
                assert_eq!(at, body.len(), "chunk {i} starts where the last ended");
                assert_eq!(
                    line.contains(" more=1 "),
                    i + 1 < lines.len(),
                    "more=1 on every line but the last"
                );
                assert!(line.len() <= REQUEST_LINE_MAX, "chunk {i} fits one line");
                let text = line.split(" text=").nth(1).expect("text=");
                body.push_str(&crate::pct::decode(text));
            }
            body
        };
        let small = fetched_lines("s-x", 7, fields, "hello world").expect("small");
        assert_eq!(
            small,
            vec!["deliver s-x fetched=7 from=s-a@n-b kind=note trust=agent text=hello%20world"]
        );
        let mut text = "x ".repeat(19_999);
        text.push('x');
        let lines = fetched_lines("s-x", 4, fields, &text).expect("chunked");
        assert!(lines.len() > 1, "{} line(s)", lines.len());
        assert!(
            !lines.iter().any(|l| l.contains(" len=")),
            "not cut, so no len="
        );
        assert_eq!(decode(&lines), text, "whole, byte for byte");
        let wide = "é🙂".repeat(20_000);
        let lines = fetched_lines("s-x", 5, fields, &wide).expect("wide");
        assert!(lines.len() > 1);
        assert_eq!(decode(&lines), wide);
        let big = format!("{}é", "z".repeat(FETCH_BODY_MAX - 1));
        let lines = fetched_lines("s-x", 6, fields, &big).expect("big");
        let got = decode(&lines);
        assert_eq!(
            got.len(),
            FETCH_BODY_MAX - 1,
            "cut before the split character"
        );
        assert!(lines
            .iter()
            .all(|l| l.contains(&format!(" len={} ", big.len()))));
        let huge = "x".repeat(REQUEST_LINE_MAX);
        assert_eq!(fetched_lines("s-x", 8, &huge, "hi"), Err("oversize"));
    }

    #[test]
    fn a_configs_debug_never_prints_the_instance_token() {
        let cfg = Config {
            fleet: "f".into(),
            broker: "/tmp/b.sock".into(),
            cap_files: vec!["/tmp/node.cap".into()],
            state_dir: "/tmp/state".into(),
            accept_from: Vec::new(),
            sock: None,
            token: Some("owner-token-0123".into()),
            presence: Mode::Meta,
            receipts: true,
            log: crate::mirror::LogMode::Off,
        };
        let printed = format!("{cfg:?}");
        assert!(
            !printed.contains("owner-token-0123") && printed.contains("<redacted>"),
            "{printed}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_subscription_whose_link_goes_silent_is_closed_not_parked() {
        use astream_broker::proto::{
            decode_request, encode_response, read_frame, write_frame, Request, Response,
        };
        let dir = aterm_tempfile::Builder::new()
            .prefix("al-silent")
            .tempdir_in("/tmp")
            .expect("the fixture dir");
        let sock = dir.path().join("b.sock");
        let listener = std::os::unix::net::UnixListener::bind(&sock).expect("bind");
        let (release, held) = std::sync::mpsc::channel::<()>();
        let broker = std::thread::spawn(move || {
            let (mut c, _) = listener.accept().expect("accept");
            let mark = Response::Mark {
                next: 0,
                head: 0,
                resume: String::new(),
                durable: None,
                oldest: None,
            };
            for want in [Request::IdleMarks, Request::GapNotices] {
                let opt_in = read_frame(&mut c).expect("the opt-in").expect("a frame");
                assert_eq!(decode_request(&opt_in), Some(want));
                write_frame(&mut c, &encode_response(&mark)).expect("the opt-in's ack");
            }
            let subscribe = read_frame(&mut c).expect("the subscribe").expect("a frame");
            assert!(
                matches!(decode_request(&subscribe), Some(Request::Subscribe { .. })),
                "the subscription follows its opt-in"
            );
            write_frame(&mut c, &encode_response(&mark)).expect("the catch-up mark");
            let _ = held.recv();
        });
        let (conn, closer) =
            transport::connect(&sock.to_string_lossy(), Duration::from_secs(60)).expect("connect");
        let sub = conn.subscribe(0, "/f/f/fleet/>").expect("subscribe");
        closer
            .set_read_timeout(None)
            .expect("lift the dial's bound");
        let mailbox = Arc::new(Mailbox::default());
        spawn_reader(
            sub,
            Arc::new(BusCloser::Os(closer)),
            Duration::from_millis(200),
            Source::Fleet,
            Arc::clone(&mailbox),
        )
        .expect("the reader thread");
        let item = mailbox.take(Duration::from_secs(60));
        assert!(
            matches!(item, Some(Item::Closed(Source::Fleet))),
            "a silent link ends the subscription: {item:?}"
        );
        let _ = release.send(());
        broker.join().expect("the stand-in broker");
    }

    #[test]
    fn a_lost_verb_lane_reaches_the_loop_and_a_refused_line_does_not() {
        use std::os::unix::net::UnixStream;

        let (near, far) = UnixStream::pair().expect("socketpair");
        let mut ctl = Ctl::from_stream(near).expect("client");
        let mailbox = Mailbox::default();
        let err = ctl
            .request(&"x".repeat(REQUEST_LINE_MAX + 1))
            .expect_err("over the bound");
        note_ctl_loss(&ctl, &mailbox, Some(&err));
        assert!(
            mailbox.take(Duration::from_millis(0)).is_none(),
            "a refused line must not end the bridge"
        );
        drop(far);

        let (near, far) = UnixStream::pair().expect("socketpair");
        drop(far);
        let mut ctl = Ctl::from_stream(near).expect("client");
        let err = ctl.request("sessions").expect_err("the peer is gone");
        note_ctl_loss(&ctl, &mailbox, Some(&err));
        assert!(
            matches!(
                mailbox.take(Duration::from_millis(0)),
                Some(Item::Closed(Source::Aterm))
            ),
            "a lost verb lane must reach the loop as Closed(Aterm)"
        );
    }

    #[test]
    fn a_bounded_encode_cuts_on_a_character_and_never_inside_an_escape() {
        let (out, cut) = encode_bounded("two words", 64);
        assert_eq!((out.as_str(), cut), ("two%20words", false));

        let text = "é β 漢 x".repeat(40);
        for budget in 0..64 {
            let (out, cut) = encode_bounded(&text, budget);
            assert!(out.len() <= budget, "budget {budget}: {out}");
            assert!(cut, "budget {budget} cannot hold the whole text");
            assert!(
                !out.ends_with('%') && !(out.len() >= 2 && out.as_bytes()[out.len() - 2] == b'%'),
                "budget {budget} split an escape: {out}"
            );
            assert!(
                text.starts_with(&crate::pct::decode(&out)),
                "budget {budget} did not cut on a character: {out}"
            );
        }
    }

    #[test]
    fn the_self_lane_check_accuses_only_inside_the_window_it_remembers() {
        let acked: BTreeSet<u64> = [100, 200, 300].into_iter().collect();
        assert!(!forged_self(&acked, 200));
        assert!(forged_self(&acked, 250));
        assert!(forged_self(&acked, 9_000));
        assert!(!forged_self(&acked, 99));
        assert!(!forged_self(&acked, 0));
        assert!(forged_self(&BTreeSet::new(), 7));
    }

    #[test]
    fn a_via_chain_is_bounded_by_grammar_and_by_length() {
        let hop = format!("n-{}", "a".repeat(32));
        assert_eq!(hop.len(), subject::PRINCIPAL_MAX);
        let worst_via = vec![hop; crate::body::VIA_MAX_HOPS].join(",");
        assert!(via_ok(&worst_via), "the longest chain the predicate admits");
        let worst_prefix = format!(
            "deliver {sid} off={off} from={sid}@{sid} kind=undeliverable trust=relayed \
             re={off} dl={off} demoted=undeliverable via={worst_via} len={off} text=",
            sid = "s-".to_string() + &"a".repeat(32),
            off = u64::MAX,
        );
        assert!(
            worst_prefix.len() < REQUEST_LINE_MAX,
            "the worst metadata prefix is {} bytes, against a {REQUEST_LINE_MAX}-byte bound",
            worst_prefix.len()
        );
    }

    #[test]
    fn only_the_recipients_verdicts_book_a_refusal_and_each_reaches_the_bus_as_one_token() {
        assert_eq!(deliver_verdict("ERR quota", false), Some("quota"));
        assert_eq!(deliver_verdict("ERR too large", false), Some("too-large"));
        assert_eq!(
            deliver_verdict("ERR no such session", false),
            Some(Reject::NotHosted.token())
        );
        assert_eq!(deliver_verdict("ERR not-subscribed", false), None);
        assert_eq!(
            deliver_verdict("ERR not-subscribed", true),
            Some("not-subscribed")
        );
        for lane_fault in [
            "ERR usage: deliver <sid> off=<n> | deliver <sid> landed=<id>",
            "ERR unknown verb (try: help)",
            "ERR denied",
            "ERR quotas",
            "ERR no such sessionx",
            "ERR",
            "",
            "OK 3",
        ] {
            assert_eq!(deliver_verdict(lane_fault, false), None, "{lane_fault:?}");
            assert_eq!(deliver_verdict(lane_fault, true), None, "{lane_fault:?}");
        }
        assert_eq!(reason_token("ERR quota"), "quota");
        assert_eq!(reason_token("ERR"), "unknown");
        assert_eq!(reason_token(""), "unknown");
        for reply in [
            "ERR quota",
            "ERR no such session",
            "ERR usage: deliver <sid> off=<n> [via=<p,...>] | x",
            "ERR \u{1b}[31mred",
            "ERR a=b c=d",
        ] {
            let token = reason_token(reply);
            assert!(
                !token.is_empty()
                    && token.len() <= REASON_TOKEN_MAX
                    && token
                        .chars()
                        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.')),
                "{reply:?} became {token:?}"
            );
        }
    }

    #[test]
    fn a_link_record_is_sent_on_change_on_a_2x_move_and_on_a_stale_report() {
        let t0 = Instant::now();
        let mut link = LinkReport::new();
        assert!(link.down_wants_report("refused"), "the first down is news");
        link.up = false;
        link.reason = "refused";
        link.reported_at = Some(t0);
        assert!(
            !link.down_wants_report("refused"),
            "a retry that fails the same way is not"
        );
        assert!(link.down_wants_report("no-socket"), "a different reason is");
        assert!(link.ack_wants_report(1, t0), "down to up is");

        let up = |rtt: u64| LinkReport {
            up: true,
            reason: "",
            rtt_ms: Some(rtt),
            reported_at: Some(t0),
        };
        assert!(up(2).down_wants_report("closed"), "up to down is");
        let ms = Duration::from_millis;
        for (was, rtt, after, wants, case) in [
            (2, 3, ms(300), false, "within 2x"),
            (2, 4, ms(300), false, "exactly 2x"),
            (2, 5, ms(300), true, "past 2x, past the gap"),
            (2, 5, ms(100), false, "past 2x, inside the gap"),
            (2, 1, LINK_MOVE_GAP, false, "a drop to exactly half"),
            (3, 1, LINK_MOVE_GAP, true, "a drop past 2x, past the gap"),
            (3, 1, ms(100), false, "a drop past 2x, inside the gap"),
            (2, 2, ms(1_999), false, "same rtt, inside the refresh"),
            (
                2,
                2,
                LINK_REFRESH,
                true,
                "same rtt, after the refresh window",
            ),
        ] {
            assert_eq!(up(was).ack_wants_report(rtt, t0 + after), wants, "{case}");
        }
    }

    #[test]
    fn a_link_reason_is_one_token_per_failure_and_an_ack_never_reads_zero() {
        use io::ErrorKind as K;
        for (kind, token) in [
            (K::NotFound, "no-socket"),
            (K::ConnectionRefused, "refused"),
            (K::PermissionDenied, "denied"),
            (K::TimedOut, "no-ack"),
            (K::WouldBlock, "no-ack"),
            (K::UnexpectedEof, "closed"),
            (K::BrokenPipe, "closed"),
            (K::ConnectionReset, "closed"),
            (K::NotConnected, "closed"),
            (K::InvalidData, "error"),
        ] {
            assert_eq!(
                link_reason(&io::Error::new(kind, "x"), "attach"),
                token,
                "{kind:?}"
            );
        }
        assert_eq!(
            link_reason(&io::Error::other("denied by cap"), "attach"),
            "attach"
        );
        assert_eq!(
            link_reason(&io::Error::other("denied by cap"), "read"),
            "read"
        );
        use astream_broker::proto as p;
        let refusal = |code: u8| -> io::Error {
            astream_broker::BrokerRefusal {
                code,
                msg: "x".to_string(),
            }
            .into()
        };
        for (code, token) in [
            (p::ERR_TOO_MANY_CONNS, "busy"),
            (p::ERR_RECORD_REFUSED, "record"),
            (p::ERR_PROTOCOL, "protocol"),
            (p::ERR_UNAUTHORIZED, "attach"),
            (p::ERR_REFUSED, "attach"),
            (0xEE, "attach"),
        ] {
            assert_eq!(link_reason(&refusal(code), "attach"), token, "code {code}");
        }
        assert_eq!(link_reason(&refusal(p::ERR_UNAUTHORIZED), "read"), "read");
        let lapsed = |code: u8| -> io::Error {
            astream_broker::BrokerRefusal {
                code,
                msg: format!("{} at 1 (now 2, unix ms)", p::CAP_EXPIRED_PREFIX),
            }
            .into()
        };
        for caller in ["attach", "read", "subscribe"] {
            assert_eq!(link_reason(&lapsed(p::ERR_UNAUTHORIZED), caller), "expired");
        }
        assert!(!refused_for_good(&lapsed(p::ERR_UNAUTHORIZED)));
        assert_eq!(link_reason(&lapsed(p::ERR_REFUSED), "attach"), "attach");
        assert!(refused_for_good(&refusal(p::ERR_UNAUTHORIZED)));
        assert_eq!(
            link_reason(&refusal(p::ERR_UNAUTHORIZED), "attach"),
            "attach"
        );
        let words = |code: u8, msg: &str| -> io::Error {
            astream_broker::BrokerRefusal {
                code,
                msg: msg.to_string(),
            }
            .into()
        };
        let poisoned = format!(
            "{} (a flush failed (EIO) with records no full flush covers); reopen it to recover",
            p::LOG_POISONED_PREFIX
        );
        for msg in [poisoned.clone(), format!("{poisoned} — attach")] {
            for caller in ["attach", "read", "commit"] {
                assert_eq!(
                    link_reason(&words(p::ERR_REFUSED, &msg), caller),
                    "poisoned",
                    "{msg}"
                );
            }
            assert!(!refusal_is_ack(&words(p::ERR_REFUSED, &msg)), "{msg}");
            assert!(!refused_for_good(&words(p::ERR_REFUSED, &msg)), "{msg}");
        }
        for (code, msg) in [
            (p::ERR_UNAUTHORIZED, poisoned.as_str()),
            (p::ERR_REFUSED, "commit failed: injected fsync fault"),
            (
                p::ERR_REFUSED,
                "durability flush failed: injected; the outcome is unknown, and the log refuses \
                 every write until it is reopened: resend then",
            ),
            (
                p::ERR_REFUSED,
                "append failed: log poisoned (x); reopen it to recover",
            ),
        ] {
            assert_ne!(
                link_reason(&words(code, msg), "attach"),
                "poisoned",
                "{msg}"
            );
        }
        assert!(
            refusal_is_ack(&words(
                p::ERR_REFUSED,
                "commit failed: injected fsync fault"
            )),
            "a write rolled back is the broker working"
        );
        assert!(refusal_is_ack(&refusal(p::ERR_UNAUTHORIZED)));
        assert!(!refusal_is_ack(&lapsed(p::ERR_UNAUTHORIZED)));
        assert!(!refusal_is_ack(&refusal(p::ERR_TOO_MANY_CONNS)));
        assert!(!refusal_is_ack(&io::Error::new(K::BrokenPipe, "x")));
        let skew: io::Error = astream_broker::ProtocolMismatch {
            broker: astream_broker::proto::PROTO_VERSION + 1,
            client: astream_broker::proto::PROTO_VERSION,
        }
        .into();
        assert_eq!(link_reason(&skew, "attach"), "protocol");
        assert_eq!(link_reason(&skew, "read"), "protocol");
        assert_eq!(
            link_reason(&io::Error::new(K::Unsupported, "unsupported"), "attach"),
            "error"
        );
        assert_eq!(rtt_ms_of(Duration::from_micros(1)), 1);
        assert_eq!(rtt_ms_of(Duration::from_micros(999)), 1);
        assert_eq!(rtt_ms_of(Duration::from_micros(1_000)), 1);
        assert_eq!(rtt_ms_of(Duration::from_micros(1_001)), 2);
        assert_eq!(
            rtt_ms_of(Duration::ZERO),
            0,
            "only a zero-length round trip reads 0, and none is"
        );
    }

    #[test]
    fn a_request_sent_past_an_owed_ack_is_this_bridges_bug() {
        let owed: io::Error = astream_broker::AcksOwed { owed: 2 }.into();
        assert!(sent_past_an_owed_ack(&owed));
        assert_eq!(link_reason(&owed, "read"), "error");
        assert_eq!(link_reason(&owed, "attach"), "error");
        let others: [(io::Error, &str); 3] = [
            (
                astream_broker::BrokerRefusal {
                    code: astream_broker::proto::ERR_REFUSED,
                    msg: "x".to_string(),
                }
                .into(),
                "link down reason=read",
            ),
            (
                io::Error::new(io::ErrorKind::BrokenPipe, "x"),
                "link down reason=closed",
            ),
            (
                io::Error::new(io::ErrorKind::ResourceBusy, "busy"),
                "link down reason=error",
            ),
        ];
        let (lane, mut bridge) = FakeVerbLane::bridge(&[]);
        let socket = lane.dir.path().join("broker.sock");
        let broker = crate::test_broker(lane.dir.path().join("broker.log")).unwrap();
        let _server = broker.serve(&socket).unwrap();
        bridge.cfg.broker = socket.to_string_lossy().into_owned();
        for (e, down) in &others {
            assert!(!sent_past_an_owed_ack(e), "{e}");
            bridge.conn = Some(bridge.connect().unwrap().0.into());
            bridge.note_ack(Duration::from_millis(1));
            assert!(bridge.link.up, "{e}: the link is up before the verdict");
            bridge.judge(bridge.fx.now(), Err(e), Some("read"));
            assert!(bridge.conn.is_none(), "{e}: the connection is dropped");
            assert!(!bridge.link.up, "{e}: the link is down");
            assert_eq!(
                lane.links.lock().unwrap().last().map(String::as_str),
                Some(*down),
                "{e}: reported under its own token"
            );
        }
        bridge.conn = Some(bridge.connect().unwrap().0.into());
        let stopped = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            bridge.judge(bridge.fx.now(), Err(&owed), Some("read"));
        }));
        assert!(stopped.is_err(), "a debug build stops at the verdict");
    }

    #[test]
    fn a_read_back_that_retention_answered_is_no_rows() {
        use astream_broker::proto::{ERR_RECORD_REFUSED, ERR_REFUSED, RETAINED_FROM_PREFIX};
        let refusal =
            |code, msg: String| -> io::Error { astream_broker::BrokerRefusal { code, msg }.into() };
        let retained = || {
            refusal(
                ERR_RECORD_REFUSED,
                format!("{RETAINED_FROM_PREFIX}5: retention gave up the records below offset 5"),
            )
        };
        assert_eq!(
            link_reason(&retained(), "read"),
            "record",
            "PRECONDITION: judged as it stands, it would drop the connection"
        );
        assert!(read_back_rows(Err(retained()), 4).unwrap().is_empty());
        assert!(read_back_rows(Err(retained()), 0).unwrap().is_empty());
        for (e, off) in [
            (retained(), 5),
            (
                refusal(ERR_REFUSED, format!("{RETAINED_FROM_PREFIX}5: x")),
                4,
            ),
            (
                refusal(ERR_RECORD_REFUSED, "sequence in the reserved half".into()),
                4,
            ),
            (io::Error::new(io::ErrorKind::BrokenPipe, "x"), 4),
        ] {
            let what = e.to_string();
            assert!(read_back_rows(Err(e), off).is_err(), "{what} at {off}");
        }
        let row: BrokerRecord = (7, "/f/f/x".to_string(), b"b".to_vec());
        assert_eq!(
            read_back_rows(Ok((vec![row.clone()], (8, 8))), 7).unwrap(),
            vec![row]
        );
    }

    #[test]
    fn a_halt_reason_is_bounded_sanitized_and_never_empty() {
        assert_eq!(halt_reason_token(None), "fleet-halt");
        assert_eq!(halt_reason_token(Some("")), "fleet-halt");
        assert_eq!(halt_reason_token(Some("main%20broken")), "main%20broken");
        assert_eq!(halt_reason_token(Some("a\u{7}b\nc\u{202e}d")), "abcd");
        let long = "x".repeat(200);
        assert_eq!(halt_reason_token(Some(&long)).len(), HALT_REASON_MAX);
        let escaped = format!("{}%41", "x".repeat(HALT_REASON_MAX - 2));
        let clamped = halt_reason_token(Some(&escaped));
        assert!(
            !clamped.ends_with('%') && !clamped.contains("%4"),
            "a clamp split an escape: {clamped}"
        );
    }

    struct FixtureLease;

    static FIXTURE_ADMISSION: (std::sync::Mutex<usize>, std::sync::Condvar) =
        (std::sync::Mutex::new(0), std::sync::Condvar::new());

    fn fixture_admission() -> usize {
        static ADMITTED: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
        *ADMITTED.get_or_init(|| {
            if aterm_spec::host::raise_open_files(1024) >= 1024 {
                16
            } else {
                4
            }
        })
    }

    std::thread_local! {
        static FIXTURE_LEASE: std::cell::RefCell<std::sync::Weak<FixtureLease>> =
            const { std::cell::RefCell::new(std::sync::Weak::new()) };
    }

    impl FixtureLease {
        fn take() -> Arc<Self> {
            FIXTURE_LEASE.with(|current| {
                if let Some(lease) = current.borrow().upgrade() {
                    return lease;
                }
                let (admission, changed) = &FIXTURE_ADMISSION;
                let mut active = admission.lock().unwrap_or_else(|e| e.into_inner());
                let until = Instant::now() + aterm_spec::wait::MUST_HAPPEN;
                let admitted = fixture_admission();
                while *active >= admitted {
                    let remaining = until
                        .checked_duration_since(Instant::now())
                        .expect("a bridge fixture did not release its resource lease");
                    (active, _) = changed
                        .wait_timeout(active, remaining)
                        .unwrap_or_else(|e| e.into_inner());
                }
                *active += 1;
                let lease = Arc::new(Self);
                *current.borrow_mut() = Arc::downgrade(&lease);
                lease
            })
        }
    }

    impl Drop for FixtureLease {
        fn drop(&mut self) {
            let (admission, changed) = &FIXTURE_ADMISSION;
            let mut active = admission.lock().unwrap_or_else(|e| e.into_inner());
            *active -= 1;
            changed.notify_one();
        }
    }

    #[derive(Default)]
    struct LaneReaderState {
        stopped: bool,
        active: Option<std::sync::Weak<std::os::unix::net::UnixStream>>,
    }

    struct LaneReaderGuard {
        state: Arc<(std::sync::Mutex<LaneReaderState>, std::sync::Condvar)>,
        worker: Option<std::thread::JoinHandle<()>>,
    }

    impl Drop for LaneReaderGuard {
        fn drop(&mut self) {
            let (state, changed) = &*self.state;
            let mut state = state.lock().unwrap_or_else(|e| e.into_inner());
            state.stopped = true;
            if let Some(stream) = state.active.take().and_then(|stream| stream.upgrade()) {
                let _ = stream.shutdown(std::net::Shutdown::Write);
                let _ = stream.shutdown(std::net::Shutdown::Read);
            }
            changed.notify_one();
            drop(state);
            if let Some(worker) = self.worker.take() {
                if let Err(panic) = worker.join() {
                    if !std::thread::panicking() {
                        std::panic::resume_unwind(panic);
                    }
                }
            }
        }
    }

    struct FakeVerbLane {
        _lease: Arc<FixtureLease>,
        _reader: Option<LaneReaderGuard>,
        dir: aterm_tempfile::TempDir,
        sessions: Arc<std::sync::Mutex<Vec<(u64, String)>>>,
        topics: Arc<std::sync::Mutex<BTreeMap<String, Vec<String>>>>,
        topics_seen: Arc<std::sync::Mutex<BTreeMap<String, u64>>>,
        reads: Arc<std::sync::Mutex<Vec<String>>>,
        links: Arc<std::sync::Mutex<Vec<String>>>,
        log_answer: Arc<std::sync::Mutex<String>>,
        outbox: Arc<std::sync::Mutex<Option<String>>>,
        sent: Arc<std::sync::Mutex<Vec<String>>>,
        delivered: Arc<std::sync::Mutex<Vec<u64>>>,
        roster_refused: Arc<std::sync::atomic::AtomicBool>,
        roster_answer: Arc<std::sync::Mutex<Option<String>>>,
        hellos: Arc<std::sync::Mutex<Vec<String>>>,
        hello_answer: Arc<std::sync::Mutex<String>>,
        reads_refused: Arc<std::sync::Mutex<BTreeMap<String, usize>>>,
        transcript: Arc<std::sync::Mutex<Vec<String>>>,
        on_deliver: Arc<std::sync::Mutex<Option<LaneHook>>>,
        deliver_answers: Arc<std::sync::Mutex<BTreeMap<String, String>>>,
        hang_up: Arc<std::sync::atomic::AtomicBool>,
        hold_refusals: Arc<std::sync::Mutex<BTreeMap<String, String>>>,
        holds: Arc<std::sync::Mutex<BTreeMap<String, bool>>>,
        status_rows: Arc<std::sync::Mutex<Option<Vec<String>>>>,
        retirements_refused: Arc<std::sync::atomic::AtomicBool>,
        outbox_retires: Arc<std::sync::atomic::AtomicBool>,
        deliveries: Arc<std::sync::Mutex<Vec<String>>>,
        mirror: Arc<std::sync::Mutex<FakeMirror>>,
    }

    #[derive(Default)]
    struct FakeMirror {
        logs: BTreeMap<String, aterm_slog::Log>,
        adopting: bool,
        gone_while_adopting: bool,
        skip_covers: bool,
        refuse_take: Option<String>,
        takes: usize,
        landings: Vec<String>,
        /// Sessions whose landing answers `done`, as the endpoint answers for a closed
        /// session whose tail is all on the bus.
        done: BTreeSet<String>,
    }

    impl FakeMirror {
        fn take(&mut self, rest: &str) -> String {
            self.takes += 1;
            if let Some(refusal) = &self.refuse_take {
                return refusal.clone();
            }
            let n: usize = rest
                .split_whitespace()
                .find_map(|t| t.strip_prefix("n="))
                .and_then(|n| n.parse().ok())
                .unwrap_or(aterm_slog::TAKE_MAX);
            let (mut body, mut k, mut more) = (String::new(), 0usize, false);
            for (sid, log) in &mut self.logs {
                if k == n {
                    more = true;
                    break;
                }
                let copy = log.bus_copy(n - k, aterm_slog::Stamp::new(1, 1));
                more |= copy.more();
                for s in copy.encode(aterm_slog::Tier::Wire, &|_| None).slots {
                    if self.skip_covers && s.kind == aterm_slog::SlotKind::Cover {
                        continue;
                    }
                    body.push_str(&format!("{sid} {} {}\n", s.at.word(), s.body));
                    k += 1;
                }
            }
            let more = if more { " more=1" } else { "" };
            format!("OK {} slots={k}{more}\n{body}", body.len())
        }

        fn land(&mut self, rest: &str) -> String {
            self.landings.push(rest.to_string());
            let toks: Vec<&str> = rest.split_whitespace().collect();
            let mut words = Vec::new();
            for pair in toks.as_chunks::<2>().0 {
                let at = aterm_slog::BusCursor::parse(pair[1]).expect("a cursor");
                let done = self.done.contains(pair[0]);
                let word = match self.logs.get_mut(pair[0]) {
                    Some(log) => match log.land(at) {
                        Ok(_) if done => "done",
                        Ok(aterm_slog::Landed::Moved) => "ok",
                        Ok(aterm_slog::Landed::Stale) => "stale",
                        Err(aterm_slog::LandRefused::Fenced) => "fenced",
                        Err(aterm_slog::LandRefused::Ahead) => "ahead",
                    },
                    None if self.adopting && !self.gone_while_adopting => "adopting",
                    None => "gone",
                };
                words.push(word);
            }
            format!("OK {}\n", words.join(" "))
        }
    }

    type LaneHook = Box<dyn FnOnce() + Send>;

    impl FakeVerbLane {
        fn bridge(sessions: &[(u64, &str)]) -> (Self, Bridge) {
            Self::bridge_with_state(sessions, None)
        }

        fn bridge_with_state(
            sessions: &[(u64, &str)],
            previous: Option<&std::path::Path>,
        ) -> (Self, Bridge) {
            use std::io::{BufRead, BufReader, Write};
            let lease = FixtureLease::take();
            let dir = aterm_tempfile::Builder::new()
                .prefix("al-v")
                .tempdir_in("/tmp")
                .expect("the fixture dir");
            let sock = dir.path().join("v.sock");
            let broker = dir.path().join("no-broker").to_string_lossy().into_owned();
            let state_dir = previous
                .map_or_else(|| dir.path().join("state"), std::path::Path::to_path_buf)
                .to_string_lossy()
                .into_owned();
            let listener = std::os::unix::net::UnixListener::bind(&sock).expect("bind the lane");
            listener.set_nonblocking(true).expect("nonblocking accept");
            let mut lane = FakeVerbLane {
                _lease: lease.clone(),
                _reader: None,
                dir,
                sessions: Arc::new(std::sync::Mutex::new(
                    sessions.iter().map(|(l, s)| (*l, s.to_string())).collect(),
                )),
                topics: Arc::default(),
                topics_seen: Arc::default(),
                reads: Arc::default(),
                links: Arc::default(),
                log_answer: Arc::new(std::sync::Mutex::new("OK\n".to_string())),
                outbox: Arc::default(),
                sent: Arc::default(),
                delivered: Arc::default(),
                roster_refused: Arc::default(),
                roster_answer: Arc::default(),
                hellos: Arc::default(),
                hello_answer: Arc::new(std::sync::Mutex::new(
                    "ERR unknown verb (try: help)\n".to_string(),
                )),
                reads_refused: Arc::default(),
                transcript: Arc::default(),
                on_deliver: Arc::default(),
                deliver_answers: Arc::default(),
                hang_up: Arc::default(),
                hold_refusals: Arc::default(),
                holds: Arc::default(),
                status_rows: Arc::default(),
                retirements_refused: Arc::default(),
                outbox_retires: Arc::default(),
                deliveries: Arc::default(),
                mirror: Arc::default(),
            };
            let (transcript, on_deliver) = (lane.transcript.clone(), lane.on_deliver.clone());
            let deliver_answers = lane.deliver_answers.clone();
            let roster_answer = lane.roster_answer.clone();
            let (hellos, hello_answer) = (lane.hellos.clone(), lane.hello_answer.clone());
            let hang_up = lane.hang_up.clone();
            let (hold_refusals, holds) = (lane.hold_refusals.clone(), lane.holds.clone());
            let status_rows = lane.status_rows.clone();
            let (roster, topics, reads, delivered) = (
                lane.sessions.clone(),
                lane.topics.clone(),
                lane.reads.clone(),
                lane.delivered.clone(),
            );
            let topics_seen = lane.topics_seen.clone();
            let (roster_refused, reads_refused) =
                (lane.roster_refused.clone(), lane.reads_refused.clone());
            let links = lane.links.clone();
            let log_answer = lane.log_answer.clone();
            let (outbox, sent) = (lane.outbox.clone(), lane.sent.clone());
            let retirements_refused = lane.retirements_refused.clone();
            let (outbox_retires, deliveries) =
                (lane.outbox_retires.clone(), lane.deliveries.clone());
            let mirror = lane.mirror.clone();
            let reader_state = Arc::new((
                std::sync::Mutex::new(LaneReaderState::default()),
                std::sync::Condvar::new(),
            ));
            let accepting = reader_state.clone();
            let worker = std::thread::spawn(move || {
                let _lease = lease;
                let (state, changed) = &*accepting;
                let stream = loop {
                    let mut state = state.lock().unwrap_or_else(|e| e.into_inner());
                    if state.stopped {
                        return;
                    }
                    match listener.accept() {
                        Ok((stream, _)) => {
                            stream.set_nonblocking(false).expect("blocking lane reads");
                            let stream = Arc::new(stream);
                            state.active = Some(Arc::downgrade(&stream));
                            break stream;
                        }
                        Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                            drop(
                                changed
                                    .wait_timeout(state, Duration::from_millis(20))
                                    .unwrap_or_else(|e| e.into_inner()),
                            );
                        }
                        Err(e) => panic!("accept the fake control lane: {e}"),
                    }
                };
                let mut out = stream.as_ref();
                for line in BufReader::new(stream.as_ref()).lines() {
                    let Ok(line) = line else { return };
                    let reply = if line.starts_with("AUTH ") {
                        continue;
                    } else if matches!(line.as_str(), "outbox" | "outbox earlier-receipts=1") {
                        match outbox.lock().unwrap().as_ref() {
                            Some(frame) => format!("OK {}\n{frame}", frame.len()),
                            None => "ERR denied\n".to_string(),
                        }
                    } else if line.starts_with("outbox sent ") {
                        if retirements_refused.load(std::sync::atomic::Ordering::SeqCst) {
                            "ERR busy\n".to_string()
                        } else {
                            if outbox_retires.load(std::sync::atomic::Ordering::SeqCst) {
                                let mut words = line.split_whitespace().skip(2);
                                let (sid, id) = (words.next(), words.next());
                                if let (Some(frame), Some(sid), Some(id)) =
                                    (outbox.lock().unwrap().as_mut(), sid, id)
                                {
                                    *frame = without_post(frame, sid, id);
                                }
                            }
                            sent.lock().unwrap().push(line);
                            "OK\n".to_string()
                        }
                    } else if let Some(rest) = line.strip_prefix("log-take") {
                        mirror.lock().unwrap().take(rest)
                    } else if let Some(rest) = line.strip_prefix("log-landed ") {
                        mirror.lock().unwrap().land(rest)
                    } else if line.starts_with("link log ") {
                        links.lock().unwrap().push(line);
                        let reply = log_answer.lock().unwrap().clone();
                        if reply.is_empty() {
                            return;
                        }
                        reply
                    } else if line.starts_with("link ") {
                        links.lock().unwrap().push(line);
                        "OK\n".to_string()
                    } else if line.starts_with("hello ") {
                        hellos.lock().unwrap().push(line);
                        hello_answer.lock().unwrap().clone()
                    } else if let Some(answer) = (line == "sessions bridge")
                        .then(|| roster_answer.lock().unwrap().clone())
                        .flatten()
                    {
                        answer
                    } else if line == "sessions bridge"
                        && roster_refused.load(std::sync::atomic::Ordering::SeqCst)
                    {
                        "ERR busy\n".to_string()
                    } else if let Some(rows) = (line == "sessions status")
                        .then(|| status_rows.lock().unwrap().clone())
                        .flatten()
                    {
                        let mut r = format!("OK {}\n", rows.len());
                        for row in rows {
                            r.push_str(&row);
                            r.push('\n');
                        }
                        r
                    } else if line == "sessions bridge" {
                        let rows = roster.lock().unwrap();
                        let mut r = format!("OK {}\n", rows.len());
                        for (local, sid) in rows.iter() {
                            r.push_str(&format!("{local} {sid} nonce={local:032x}\n"));
                        }
                        r
                    } else if let Some(rest) = line.strip_prefix("hold ") {
                        transcript.lock().unwrap().push(line.clone());
                        if hang_up.load(std::sync::atomic::Ordering::SeqCst) {
                            return;
                        }
                        let mut words = rest.split_whitespace();
                        let sid = words.next().expect("a hold's sid");
                        let on = words.next() == Some("on");
                        match hold_refusals.lock().unwrap().get(sid) {
                            Some(reply) => format!("{reply}\n"),
                            None => {
                                holds.lock().unwrap().insert(sid.to_string(), on);
                                format!("OK hold={}\n", u8::from(on))
                            }
                        }
                    } else if let Some(rest) = line.strip_prefix("deliver ") {
                        deliveries.lock().unwrap().push(line.clone());
                        let off = rest
                            .split_whitespace()
                            .find_map(|word| word.strip_prefix("off="))
                            .and_then(|word| word.parse::<u64>().ok());
                        transcript.lock().unwrap().push(format!(
                            "deliver {}",
                            off.map_or_else(|| "-".to_string(), |o| o.to_string())
                        ));
                        let hook = on_deliver.lock().unwrap().take();
                        if let Some(hook) = hook {
                            hook();
                        }
                        let answer = rest
                            .split_whitespace()
                            .next()
                            .and_then(|sid| deliver_answers.lock().unwrap().get(sid).cloned());
                        match (answer, off) {
                            (Some(answer), _) => answer,
                            (None, Some(off)) => {
                                let mut ring = delivered.lock().unwrap();
                                let id = match ring.iter().position(|have| *have == off) {
                                    Some(i) => i + 1,
                                    None => {
                                        ring.push(off);
                                        ring.len()
                                    }
                                };
                                format!("OK {id}\n")
                            }
                            (None, None)
                                if rest.split_whitespace().any(|w| w.starts_with("receipt=")) =>
                            {
                                "OK\n".to_string()
                            }
                            (None, None) => "ERR invalid deliver\n".to_string(),
                        }
                    } else if let Some(sid) = line
                        .strip_prefix('@')
                        .and_then(|l| l.strip_suffix(" topic ls"))
                    {
                        reads.lock().unwrap().push(sid.to_string());
                        let refused = reads_refused
                            .lock()
                            .unwrap()
                            .get_mut(sid)
                            .filter(|left| **left > 0)
                            .map(|left| *left -= 1)
                            .is_some();
                        if refused {
                            if out.write_all(b"ERR busy\n").is_err() {
                                return;
                            }
                            continue;
                        }
                        let rows = topics.lock().unwrap().get(sid).cloned().unwrap_or_default();
                        let mut r = match topics_seen.lock().unwrap().get(sid) {
                            Some(seen) => format!("OK {} seen={seen}\n", rows.len()),
                            None => format!("OK {}\n", rows.len()),
                        };
                        for row in rows {
                            r.push_str(&row);
                            r.push('\n');
                        }
                        r
                    } else {
                        "ERR denied\n".to_string()
                    };
                    if out.write_all(reply.as_bytes()).is_err() {
                        return;
                    }
                }
            });
            lane._reader = Some(LaneReaderGuard {
                state: reader_state,
                worker: Some(worker),
            });
            let bridge = Bridge::new(Config {
                fleet: "f".to_string(),
                broker,
                cap_files: Vec::new(),
                state_dir,
                accept_from: Vec::new(),
                sock: Some(sock.to_string_lossy().into_owned()),
                token: Some("t".to_string()),
                presence: Mode::default(),
                receipts: true,
                log: crate::mirror::LogMode::Off,
            })
            .expect("a bridge on the fake lane");
            assert_eq!(bridge.attachment, Attachment::Observer);
            let mut bridge = bridge;
            bridge.attachment = Attachment::Inherited;
            (lane, bridge)
        }

        fn report_handled(&self, sid: &str, seen: u64) {
            self.topics_seen
                .lock()
                .unwrap()
                .insert(sid.to_string(), seen);
        }

        fn opt_in(&self, sid: &str, rows: &[&str]) {
            self.topics.lock().unwrap().insert(
                sid.to_string(),
                rows.iter().map(|r| (*r).to_string()).collect(),
            );
        }

        fn refuse_roster(&self, refused: bool) {
            self.roster_refused
                .store(refused, std::sync::atomic::Ordering::SeqCst);
        }

        fn refuse_retirements(&self, refused: bool) {
            self.retirements_refused
                .store(refused, std::sync::atomic::Ordering::SeqCst);
        }

        fn retire_from_outbox(&self) {
            self.outbox_retires
                .store(true, std::sync::atomic::Ordering::SeqCst);
        }

        fn deliveries(&self) -> Vec<String> {
            self.deliveries.lock().unwrap().clone()
        }

        fn answer_status_with(&self, rows: &[String]) {
            *self.status_rows.lock().unwrap() = Some(rows.to_vec());
        }

        fn answer_roster_with(&self, reply: &str) {
            *self.roster_answer.lock().unwrap() = Some(reply.to_string());
        }

        fn links(&self) -> Vec<String> {
            self.links.lock().unwrap().clone()
        }

        fn answer_hello_with(&self, reply: &str) {
            *self.hello_answer.lock().unwrap() = reply.to_string();
        }

        fn hellos(&self) -> Vec<String> {
            self.hellos.lock().unwrap().clone()
        }

        fn refuse_reads(&self, sid: &str, n: usize) {
            self.reads_refused
                .lock()
                .unwrap()
                .insert(sid.to_string(), n);
        }

        fn reads_of(&self, sid: &str) -> usize {
            self.reads
                .lock()
                .unwrap()
                .iter()
                .filter(|s| *s == sid)
                .count()
        }

        fn delivery_offsets(&self) -> Vec<u64> {
            self.delivered.lock().unwrap().clone()
        }

        fn transcript(&self) -> Vec<String> {
            self.transcript.lock().unwrap().clone()
        }

        fn answer_deliveries_to(&self, sid: &str, reply: &str) {
            self.deliver_answers
                .lock()
                .unwrap()
                .insert(sid.to_string(), reply.to_string());
        }

        fn on_first_deliver(&self, hook: impl FnOnce() + Send + 'static) {
            *self.on_deliver.lock().unwrap() = Some(Box::new(hook));
        }

        fn refuse_hold(&self, sid: &str, reply: Option<&str>) {
            let mut refused = self.hold_refusals.lock().unwrap();
            match reply {
                Some(reply) => {
                    refused.insert(sid.to_string(), reply.to_string());
                }
                None => {
                    refused.remove(sid);
                }
            }
        }

        fn holds(&self, sid: &str) -> bool {
            self.holds
                .lock()
                .unwrap()
                .get(sid)
                .copied()
                .unwrap_or(false)
        }

        fn hang_up_on_hold(&self) {
            self.hang_up
                .store(true, std::sync::atomic::Ordering::SeqCst);
        }
    }

    #[test]
    fn a_live_sequence_collision_reports_the_link_down_before_reconnect() {
        let (lane, mut bridge) = FakeVerbLane::bridge(&[]);
        let socket = lane.dir.path().join("broker.sock");
        let broker = crate::test_broker(lane.dir.path().join("broker.log")).unwrap();
        let _server = broker.serve(&socket).unwrap();
        bridge.cfg.broker = socket.to_string_lossy().into_owned();
        let (conn, _) = bridge.connect().unwrap();
        bridge.conn = Some(conn.into());
        bridge.note_ack(Duration::from_millis(1));
        assert!(bridge.link.up);

        let subject = subject::node_face(&bridge.cfg.fleet, &bridge.node, "ev");
        let seq = bridge.next_seq().unwrap();
        let (own, deduped) = bridge.publish_at(seq, &subject, b"ours").unwrap();
        assert!(!deduped);
        assert_eq!(
            bridge.publish_at(seq, &subject, b"ours").unwrap(),
            (own, true)
        );
        assert!(bridge.link.up, "our own retry leaves the link connected");
        assert!(bridge.collided.is_none());

        let seq = bridge.next_seq().unwrap();
        let (mut other, _) = bridge.connect().unwrap();
        let (off, deduped) = other
            .publish(bridge.producer_id, seq, "/f/f/x/decoy", b"another writer")
            .unwrap();
        assert!(!deduped);
        let error = bridge
            .publish_at(seq, &subject, b"not published")
            .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::AlreadyExists);
        assert!(
            bridge.conn.is_none(),
            "the collision dropped the connection"
        );
        assert!(
            !bridge.link.up,
            "a collision is stalled before any reconnect can restore the link"
        );
        assert_eq!(
            lane.links.lock().unwrap().last().map(String::as_str),
            Some("link down reason=attach"),
            "the endpoint hears the loss, so it can release queued post waits"
        );
        assert_eq!(bridge.collided, Some((seq, off)));
        assert!(!bridge.self_acked.contains(&off), "the offset is not ours");
    }

    #[test]
    fn a_denied_dedup_verification_keeps_the_post_queued() {
        for verification in [false, true] {
            denied_publication_read(false, verification, false);
        }
    }

    #[test]
    fn a_denied_dedup_verification_keeps_the_receipt_owed() {
        for verification in [false, true] {
            denied_publication_read(true, verification, false);
        }
    }

    #[test]
    fn a_lapsed_publish_keeps_the_post_queued_and_the_receipt_owed() {
        for receipt in [false, true] {
            denied_publication_read(receipt, false, true);
        }
    }

    #[test]
    fn a_poisoned_publish_downs_the_link_and_keeps_the_post_queued() {
        for poisoned in [true, false] {
            refused_publish(poisoned);
        }
    }

    fn refused_publish(poisoned: bool) {
        use astream_broker::proto::{
            decode_request, encode_response, read_frame, write_frame, Request, Response,
            ERR_REFUSED, LOG_POISONED_PREFIX,
        };
        let (lane, mut bridge) = FakeVerbLane::bridge(&[(1, "s-host")]);
        let face = format!("/f/f/in/p/h-a/{}/note", bridge.node);
        let mut body = Body::new(1);
        body.from = Some("s-host".into());
        body.text = "retry".into();
        *lane.outbox.lock().unwrap() =
            Some("post sid=s-host id=1 to=h-a kind=note len=5\nretry".into());
        let pin = PublishPin {
            seq: bridge.next_seq().unwrap(),
            via_key: false,
            identity: Some(publication_identity(&face, &body.encode())),
            original: None,
            original_off: None,
            owner: None,
        };
        bridge.state.set_post_pin("s-host", 1, &pin).unwrap();
        bridge.unsent = None;
        let socket = lane.dir.path().join("poisoned.sock");
        let listener = std::os::unix::net::UnixListener::bind(&socket).unwrap();
        let (producer, seq) = (bridge.producer_id, pin.seq);
        let (judged, wait_judged) = std::sync::mpsc::channel::<()>();
        let broker = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(60)))
                .unwrap();
            let request = read_frame(&mut stream).unwrap().unwrap();
            let Some(Request::Publish {
                producer_id,
                producer_seq,
                ..
            }) = decode_request(&request)
            else {
                panic!("the retry must publish first");
            };
            assert_eq!((producer_id, producer_seq), (producer, seq));
            let msg = if poisoned {
                format!(
                    "{LOG_POISONED_PREFIX} (a flush failed (injected) with records no full flush \
                     covers); reopen it to recover"
                )
            } else {
                "commit failed: injected fsync fault".to_string()
            };
            write_frame(
                &mut stream,
                &encode_response(&Response::Error {
                    code: ERR_REFUSED,
                    msg,
                }),
            )
            .unwrap();
            let _ = wait_judged.recv_timeout(Duration::from_secs(60));
        });
        bridge.cfg.broker = socket.to_string_lossy().into_owned();
        bridge.conn = Some(bridge.connect().unwrap().0.into());
        bridge.note_ack(Duration::from_millis(1));
        bridge.drain_outbox();
        let retained = bridge.state.post_pin("s-host", 1).unwrap().is_some();
        let retired = !lane.sent.lock().unwrap().is_empty();
        assert_eq!(
            (retained, retired),
            (true, false),
            "nothing is retired, poisoned={poisoned}"
        );
        if poisoned {
            assert!(bridge.conn.is_none(), "the poisoned connection is dropped");
            assert!(!bridge.link.up);
            assert_eq!(
                lane.links.lock().unwrap().last().map(String::as_str),
                Some("link down reason=poisoned"),
            );
        } else {
            assert!(
                bridge.conn.is_some(),
                "a rolled-back write keeps the connection"
            );
            assert!(bridge.link.up, "a rolled-back write is the broker working");
            assert!(
                !lane
                    .links
                    .lock()
                    .unwrap()
                    .iter()
                    .any(|l| l.starts_with("link down")),
                "{:?}",
                lane.links.lock().unwrap()
            );
        }
        judged.send(()).unwrap();
        broker.join().unwrap();
    }

    fn denied_publication_read(receipt: bool, verification: bool, lapsed: bool) {
        use astream_broker::proto::{
            decode_request, encode_response, read_frame, write_frame, Request, Response,
            CAP_EXPIRED_PREFIX, ERR_UNAUTHORIZED,
        };
        assert!(
            !(lapsed && verification),
            "a lapsed cap refuses the Publish"
        );
        let (lane, mut bridge) = FakeVerbLane::bridge(&[(1, "s-host")]);
        let kind = if receipt { "ack" } else { "note" };
        let face = format!("/f/f/in/p/h-a/{}/{kind}", bridge.node);
        let mut body = Body::new(1);
        body.from = Some("s-host".into());
        body.text = "retry".into();
        let owed = OwedReceipt {
            sid: "s-host".into(),
            rid: 1,
            off: 4,
            verdict: "handled".into(),
            kind: "ask".into(),
            from: "h-a".into(),
            earlier_log: false,
        };
        if receipt {
            body = receipt_body(1, "s-host", owed.off, &owed.verdict);
        } else {
            *lane.outbox.lock().unwrap() =
                Some("post sid=s-host id=1 to=h-a kind=note len=5\nretry".into());
        }
        let pin = PublishPin {
            seq: bridge.next_seq().unwrap(),
            via_key: false,
            identity: Some(publication_identity(&face, &body.encode())),
            original: None,
            original_off: None,
            owner: None,
        };
        if receipt {
            bridge.state.set_receipt_pin("s-host", 1, &pin).unwrap();
        } else {
            bridge.state.set_post_pin("s-host", 1, &pin).unwrap();
        }
        bridge.unsent = None;
        let socket = lane.dir.path().join("denied.sock");
        let listener = std::os::unix::net::UnixListener::bind(&socket).unwrap();
        let identity = pin.identity.clone().unwrap();
        let (producer, seq) = (bridge.producer_id, pin.seq);
        let broker = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(60)))
                .unwrap();
            let request = read_frame(&mut stream).unwrap().unwrap();
            let Some(Request::Publish {
                producer_id,
                producer_seq,
                subject,
                body,
            }) = decode_request(&request)
            else {
                panic!("the retry must publish first");
            };
            assert_eq!((producer_id, producer_seq), (producer, seq));
            assert_eq!(publication_identity(&subject, &body), identity);
            let refusal = Response::Error {
                code: ERR_UNAUTHORIZED,
                msg: if lapsed {
                    format!(
                        "{CAP_EXPIRED_PREFIX} before this request: every grant that covered it \
                         has lapsed (now 2, unix ms)"
                    )
                } else {
                    "the requested operation is not granted".into()
                },
            };
            if verification {
                write_frame(
                    &mut stream,
                    &encode_response(&Response::PublishAck {
                        offset: 9,
                        deduped: true,
                    }),
                )
                .unwrap();
                let request = read_frame(&mut stream).unwrap().unwrap();
                assert_eq!(
                    decode_request(&request),
                    Some(Request::Fetch {
                        from_offset: 9,
                        filter: subject,
                        max: 1,
                    }),
                    "the denial follows a successful dedup acknowledgement"
                );
            }
            write_frame(&mut stream, &encode_response(&refusal)).unwrap();
        });
        bridge.cfg.broker = socket.to_string_lossy().into_owned();
        bridge.conn = Some(bridge.connect().unwrap().0.into());
        let failed = if receipt {
            bridge.send_receipt(&owed).is_err()
        } else {
            bridge.drain_outbox();
            false
        };
        broker.join().unwrap();
        let retained = if receipt {
            bridge.state.receipt_pin("s-host", 1).unwrap().is_some()
        } else {
            bridge.state.post_pin("s-host", 1).unwrap().is_some()
        };
        let retired = if receipt {
            lane.transcript().iter().any(|line| line == "deliver -")
        } else {
            !lane.sent.lock().unwrap().is_empty()
        };
        if lapsed {
            assert_eq!(
                (retained, retired),
                (true, false),
                "a lapsed cap retires nothing: receipt={receipt}"
            );
            assert!(bridge.conn.is_none(), "the lapsed connection is dropped");
            assert!(!bridge.link.up);
            assert_eq!(
                lane.links.lock().unwrap().last().map(String::as_str),
                Some("link down reason=expired"),
            );
            if receipt {
                assert!(failed, "the receipt stays owed for the redial");
            }
            return;
        }
        assert_eq!(
            (retained, retired),
            (verification, !verification),
            "only an actual Publish refusal retires debt: receipt={receipt}, verification={verification}"
        );
        if receipt {
            assert_eq!(failed, verification);
        }
    }

    /// ONE OLDER BUILD'S DAMAGED RESERVATION LEAVES THE NODE ATTACHING (B19 review): an older
    /// build's `sent/<sid>.<id>` that does not read holds that one post, as that build held it;
    /// another session's reservation reads, the attach's in-flight settle goes on, and what is
    /// owed reads.
    #[test]
    fn one_older_builds_damaged_reservation_leaves_the_node_attaching() {
        let (lane, mut bridge) = FakeVerbLane::bridge(&[]);
        let (socket, _broker, _server) = serve_log(&lane, "broker");
        bridge.cfg.broker = socket;
        bridge.conn = Some(bridge.connect().unwrap().0.into());
        let root = bridge.state.root().to_path_buf();
        std::fs::create_dir_all(root.join("sent")).unwrap();
        std::fs::write(root.join("sent/s-z.9"), b"7 key identity\n").unwrap();
        bridge.state = StateDir::open(&root).unwrap();
        assert_eq!(bridge.state.post_pin("s-a", 1).unwrap(), None);
        assert!(
            bridge.settle_in_flight(),
            "the attach's in-flight settle goes on"
        );
        assert!(bridge
            .state
            .owed_publications()
            .unwrap()
            .sids
            .contains("s-z"));
        let held = bridge.state.post_pin("s-z", 9).unwrap_err();
        assert!(held.to_string().contains("sent/s-z.9"), "{held}");
    }

    /// A DROP THE DISK REFUSES IS NOT PUBLISHED AT THE NEXT ATTACH (B19 review): a record the
    /// bridge drops for good while the journal's writes are refused (a full disk) is forgotten by
    /// the open all the same, as an older build's unlink of its file was done, so the attach
    /// that follows in the same process publishes nothing.
    #[test]
    fn a_record_dropped_while_the_disk_is_full_is_not_published_at_the_next_attach() {
        let (lane, mut bridge) = FakeVerbLane::bridge(&[]);
        let (socket, _broker, _server) = serve_log(&lane, "broker");
        bridge.cfg.broker = socket;
        bridge.conn = Some(bridge.connect().unwrap().0.into());
        let face = format!(
            "/f/{}/in/{}/s-b/{}/note",
            bridge.cfg.fleet, bridge.node, bridge.node
        );
        let seq = bridge.state.reserve_next_sequence(bridge.inc).unwrap();
        let body = b"v=1 t=1 text=refused%20for%20good";
        bridge
            .state
            .set_in_flight(&InFlight {
                pin: plain_pin(seq, &face, body),
                subject: face.clone(),
                body: body.to_vec(),
                log: None,
            })
            .unwrap();
        bridge
            .state
            .refuse_writes_of(Some(crate::state::journal::FILE));
        bridge.drop_in_flight(seq);
        bridge.state.refuse_writes_of(None);
        assert!(bridge.settle_in_flight());
        let (mut reader, _) = bridge.connect().unwrap();
        let (rows, _) = reader.fetch(0, &face, 8).unwrap();
        assert!(
            rows.is_empty(),
            "a record the bridge dropped was published at the next attach: {rows:?}"
        );
    }

    fn assert_uncertain_in_flight_waits(kind: &str) {
        let (lane, mut bridge) = FakeVerbLane::bridge(&[]);
        let (socket, _broker, _server) = serve_log(&lane, "broker");
        bridge.cfg.broker = socket;
        bridge.conn = Some(bridge.connect().unwrap().0.into());
        assert!(bridge.settle_in_flight(), "no journal is no pending work");
        let face = format!(
            "/f/{}/in/{}/s-b/{}/note",
            bridge.cfg.fleet, bridge.node, bridge.node
        );
        let seq = bridge.state.reserve_next_sequence(bridge.inc).unwrap();
        let body = b"v=1 t=1 text=already%20landed";
        bridge
            .state
            .set_in_flight(&InFlight {
                pin: plain_pin(seq, &face, body),
                subject: face.clone(),
                body: body.to_vec(),
                log: None,
            })
            .unwrap();
        let (mut reader, _) = bridge.connect().unwrap();
        let (off, deduped) = reader
            .publish(bridge.producer_id, seq, &face, body)
            .unwrap();
        assert!(!deduped);
        let root = bridge.state.root().to_path_buf();
        let dir = root.join(crate::state::journal::DIR);
        let entry = root.join(crate::state::journal::FILE);
        let blocked = if kind == "listing" {
            dir.clone()
        } else {
            entry.clone()
        };
        let saved = if kind == "listing" {
            dir.with_extension("saved")
        } else {
            dir.join(".saved")
        };
        std::fs::rename(&blocked, &saved).unwrap();
        if kind == "entry" {
            std::fs::create_dir(&blocked).unwrap();
        } else {
            std::fs::write(&blocked, b"not a journal entry\n").unwrap();
        }
        bridge.state = StateDir::open(&root).unwrap();
        assert!(
            !bridge.settle_in_flight(),
            "{kind}: uncertain publication fate was accepted as settled"
        );
        assert!(saved.exists(), "{kind}: the original journal was discarded");
        assert!(
            blocked.exists(),
            "{kind}: the uncertain entry was discarded"
        );
        if kind == "entry" {
            std::fs::remove_dir(&blocked).unwrap();
        } else {
            std::fs::remove_file(&blocked).unwrap();
        }
        std::fs::rename(&saved, &blocked).unwrap();
        assert!(
            bridge.settle_in_flight(),
            "{kind}: the restored journal recovers"
        );
        assert!(
            bridge.self_acked.contains(&off),
            "the already-landed offset is ours"
        );
        assert!(
            bridge.state.self_acked().contains(&off),
            "the offset is remembered on disk"
        );
        assert!(
            bridge.state.in_flight().unwrap().is_empty()
                && StateDir::open(&root)
                    .unwrap()
                    .in_flight()
                    .unwrap()
                    .is_empty(),
            "the known publication is settled"
        );
        let (rows, _) = reader.fetch(0, &face, 8).unwrap();
        assert_eq!(
            rows.len(),
            1,
            "same-sequence recovery duplicated the record: {rows:?}"
        );
        assert_eq!(rows[0].0, off);
    }

    #[test]
    fn an_unlistable_in_flight_journal_stops_recovery_until_it_can_be_read() {
        assert_uncertain_in_flight_waits("listing");
    }

    #[test]
    fn an_unreadable_in_flight_entry_stops_recovery_until_it_can_be_read() {
        assert_uncertain_in_flight_waits("entry");
    }

    #[test]
    fn a_damaged_in_flight_entry_stops_recovery_until_it_is_repaired() {
        assert_uncertain_in_flight_waits("damaged");
    }

    fn leave_journaled_publications(
        bridge: &mut Bridge,
        records: &[(String, Vec<u8>)],
    ) -> Vec<(u64, u64)> {
        let acked = bridge.state.root().join("acked");
        let saved = std::fs::read(&acked).unwrap_or_default();
        if acked.is_file() {
            std::fs::remove_file(&acked).unwrap();
        }
        std::fs::create_dir(&acked).unwrap();
        let mut published = Vec::new();
        for (subject, body) in records {
            let pin = plain_pin(bridge.next_seq().unwrap(), subject, body);
            let (off, deduped, _) = bridge.publish_journaled(&pin, subject, body).unwrap();
            assert!(!deduped);
            published.push((pin.seq, off));
        }
        std::fs::remove_dir(&acked).unwrap();
        std::fs::write(&acked, saved).unwrap();
        assert_eq!(bridge.state.in_flight().unwrap().len(), records.len());
        published
    }

    #[cfg(unix)]
    #[test]
    fn referenced_journal_rows_do_not_cross_a_log_identity() {
        const SID: &str = "s-local";
        let (lane, mut bridge) = FakeVerbLane::bridge(&[(1, SID)]);
        lane.answer_hello_with(&format!("OK lane={LINK_LOG_LANE} build=test\n"));
        bridge.introduce();
        assert!(bridge.refresh_sessions().is_ok());
        let (a, _broker_a, _server_a) = serve_log(&lane, "journal-a");
        bridge.cfg.broker = a;
        assert!(bridge.attach_broker());
        let task = format!("/f/f/in/{}/{SID}/h-writer/task", bridge.node);
        let ack = format!("/f/f/in/{}/{SID}/{}/ack", bridge.node, bridge.node);
        let note = format!("/f/f/in/{}/{SID}/{}/note", bridge.node, bridge.node);
        let (mut writer, _) = bridge.connect().unwrap();
        let (old, _) = writer.publish(81, 1, &task, b"v=1 t=1 text=old").unwrap();
        let mut receipt = Body::new(1);
        receipt.re = Some(old);
        receipt.from = Some(SID.to_string());
        receipt.verdict = Some("handled".to_string());
        receipt.text = "old receipt".to_string();
        let published = leave_journaled_publications(
            &mut bridge,
            &[
                (ack.clone(), receipt.encode()),
                (note.clone(), b"v=1 t=1 text=keep-me".to_vec()),
            ],
        );

        bridge.close_subscriptions();
        bridge.conn = None;
        let (b, _broker_b, _server_b) = serve_log(&lane, "journal-b");
        bridge.cfg.broker = b;
        let (mut writer, _) = bridge.connect().unwrap();
        for seq in 0..old {
            writer
                .publish(91, seq + 1, "/f/f/x/history", b"v=1")
                .unwrap();
        }
        let (reused, _) = writer
            .publish(91, old + 1, &task, b"v=1 t=2 text=different")
            .unwrap();
        assert_eq!(reused, old);
        *lane.log_answer.lock().unwrap() = "ERR busy\n".to_string();
        assert!(!bridge.attach_broker());
        assert_eq!(
            bridge.state.in_flight().unwrap().len(),
            2,
            "no clock ack, no disposal"
        );
        *lane.log_answer.lock().unwrap() = "OK forgot=1\n".to_string();
        assert!(bridge.attach_broker());
        let (acks, _) = writer.fetch(0, &ack, 8).unwrap();
        assert!(
            acks.is_empty(),
            "A's receipt now answers B's different task: {acks:?}"
        );
        let (notes, _) = writer.fetch(0, &note, 8).unwrap();
        assert_eq!(notes.len(), 1, "no-reference publication must recover");
        assert_eq!(Body::decode(&notes[0].2).0.text, "keep-me");
        assert!(bridge.state.in_flight().unwrap().is_empty());
        assert!(
            StateDir::open(bridge.state.root())
                .unwrap()
                .in_flight()
                .unwrap()
                .is_empty(),
            "nothing in flight on disk either: {published:?}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn referenced_journal_rows_recover_on_the_same_identified_log() {
        let (lane, mut bridge) = FakeVerbLane::bridge(&[]);
        let (socket, _broker, _server) = serve_log(&lane, "journal-same");
        bridge.cfg.broker = socket;
        assert!(bridge.attach_broker());
        let subject = format!("/f/f/in/{}/s-local/{}/ack", bridge.node, bridge.node);
        let body = b"v=1 t=1 re=0 verdict=handled text=same".to_vec();
        let published = leave_journaled_publications(&mut bridge, &[(subject.clone(), body)]);
        bridge.close_subscriptions();
        bridge.conn = None;
        assert!(bridge.attach_broker());
        let (rows, _) = bridge.conn_now().unwrap().fetch(0, &subject, 8).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(
            rows[0].0, published[0].1,
            "recovery dedups the same publication"
        );
        assert!(bridge.state.in_flight().unwrap().is_empty());
        assert!(bridge.state.self_acked().contains(&rows[0].0));
    }

    #[cfg(unix)]
    #[test]
    fn referenced_journal_rows_without_a_log_witness_remain_uncertain() {
        let (lane, mut bridge) = FakeVerbLane::bridge(&[]);
        let (socket, _broker, _server) = serve_log(&lane, "journal-legacy");
        bridge.cfg.broker = socket;
        assert!(bridge.attach_broker());
        let subject = format!("/f/f/in/{}/s-local/{}/ack", bridge.node, bridge.node);
        let published = leave_journaled_publications(
            &mut bridge,
            &[(
                subject,
                b"v=1 t=1 re=0 verdict=handled text=legacy".to_vec(),
            )],
        );
        let seq = published[0].0;
        let (_, entry) = bridge.state.in_flight().unwrap().remove(0);
        let legacy = entry
            .render()
            .lines()
            .take(3)
            .collect::<Vec<_>>()
            .join("\n");
        bridge.state.clear_in_flight(seq).unwrap();
        let root = bridge.state.root().to_path_buf();
        std::fs::create_dir_all(root.join("inflight")).unwrap();
        let path = root.join("inflight").join(seq.to_string());
        std::fs::write(&path, &legacy).unwrap();
        bridge.state = StateDir::open(&root).unwrap();
        bridge.close_subscriptions();
        bridge.conn = None;
        assert!(
            !bridge.attach_broker(),
            "an older build's reference was assigned this log without evidence"
        );
        assert_eq!(
            bridge.state.in_flight().unwrap(),
            vec![(seq, InFlight { log: None, ..entry })],
            "uncertainty is retained"
        );

        std::fs::write(&path, format!("{legacy}\nlog=not-an-identity")).unwrap();
        assert!(
            StateDir::open(&root).unwrap().in_flight().is_err(),
            "malformed identity is never legacy absence"
        );
    }

    #[cfg(unix)]
    #[test]
    fn referenced_journal_rows_need_the_current_connection_log_identity() {
        let (lane, mut bridge) = FakeVerbLane::bridge(&[]);
        let (socket, broker, _server) = serve_log(&lane, "journal-unconfirmed");
        bridge.cfg.broker = socket;
        assert!(bridge.attach_broker());
        let subject = format!("/f/f/in/{}/s-local/{}/ack", bridge.node, bridge.node);
        leave_journaled_publications(
            &mut bridge,
            &[(
                subject.clone(),
                b"v=1 t=1 re=0 verdict=handled text=known".to_vec(),
            )],
        );
        bridge.conn = Some(bridge.connect().unwrap().0.into());
        assert_eq!(
            bridge.bus_log,
            Some(broker.log_identity()),
            "the old witness is retained"
        );
        assert_eq!(bridge.conn.as_ref().unwrap().seen_log_identity(), None);
        assert!(
            !bridge.settle_in_flight(),
            "a remembered witness was assigned to a silent connection"
        );
        assert_eq!(bridge.state.in_flight().unwrap().len(), 1);
        assert_eq!(
            bridge.read_log_identity().unwrap(),
            Some(broker.log_identity())
        );
        assert!(
            bridge.settle_in_flight(),
            "the actual same-log answer permits recovery"
        );

        bridge.conn = Some(bridge.connect().unwrap().0.into());
        leave_journaled_publications(
            &mut bridge,
            &[(
                subject,
                b"v=1 t=1 re=0 verdict=handled text=unknown".to_vec(),
            )],
        );
        assert_eq!(bridge.state.in_flight().unwrap()[0].1.log, None);
    }

    #[cfg(unix)]
    #[test]
    fn a_publish_with_no_connection_journals_nothing_and_the_attach_goes_on() {
        let (lane, mut bridge) = FakeVerbLane::bridge(&[]);
        let (socket, broker, _server) = serve_log(&lane, "journal-offline");
        bridge.cfg.broker = socket;
        assert!(bridge.attach_broker());
        let subject = format!("/f/f/in/{}/s-local/{}/ack", bridge.node, bridge.node);
        let body = b"v=1 t=1 re=0 verdict=handled text=offline".to_vec();
        bridge.close_subscriptions();
        bridge.conn = None;
        let pin = plain_pin(bridge.next_seq().unwrap(), &subject, &body);
        let e = bridge
            .publish_journaled(&pin, &subject, &body)
            .expect_err("no broker to publish to");
        assert_eq!(e.kind(), io::ErrorKind::NotConnected);
        assert!(
            bridge.state.in_flight().unwrap().is_empty(),
            "an entry that names no log was journaled for a publish that could not be sent"
        );
        let post = QueuedPost {
            sid: "s-local".to_string(),
            id: 1,
            to: "s-local".to_string(),
            kind: "note".to_string(),
            re: Some(0),
            dl: None,
            via: None,
            key: None,
            body: b"offline reply".to_vec(),
        };
        let reply = b"v=1 t=1 re=0 text=offline%20reply".to_vec();
        let reply_pin = plain_pin(bridge.next_seq().unwrap(), &subject, &reply);
        assert!(
            !bridge.send_post(post, reply_pin, subject.clone(), reply),
            "the drain ends with the link"
        );
        assert!(bridge.state.in_flight().unwrap().is_empty());

        bridge.conn = Some(bridge.connect().unwrap().0.into());
        assert_eq!(
            bridge.read_log_identity().unwrap(),
            Some(broker.log_identity())
        );
        let witnessed = InFlight {
            pin: pin.clone(),
            subject: subject.clone(),
            body: body.clone(),
            log: Some(broker.log_identity()),
        };
        bridge.state.set_in_flight(&witnessed).unwrap();
        bridge.close_subscriptions();
        bridge.conn = None;
        assert!(bridge.publish_journaled(&pin, &subject, &body).is_err());
        assert_eq!(
            bridge.state.in_flight().unwrap(),
            vec![(pin.seq, witnessed)],
            "the retry left the entry as its own publish wrote it"
        );
        assert!(bridge.attach_broker(), "the witnessed entry is recovered");
        assert!(bridge.state.in_flight().unwrap().is_empty());
    }

    #[test]
    fn the_attach_relearns_its_own_in_flight_records_and_no_one_elses() {
        let (lane, mut bridge) = FakeVerbLane::bridge(&[]);
        let socket = lane.dir.path().join("broker.sock");
        let broker = crate::test_broker(lane.dir.path().join("broker.log")).unwrap();
        let _server = broker.serve(&socket).unwrap();
        bridge.cfg.broker = socket.to_string_lossy().into_owned();
        bridge.conn = Some(bridge.connect().unwrap().0.into());
        let (mut other, _) = bridge.connect().unwrap();
        let face = format!(
            "/f/{}/in/{}/s-b/{}/note",
            bridge.cfg.fleet, bridge.node, bridge.node
        );
        let in_flight = |bridge: &Bridge, body: &[u8]| {
            let seq = bridge.state.reserve_next_sequence(bridge.inc).unwrap();
            let entry = InFlight {
                pin: plain_pin(seq, &face, body),
                subject: face.clone(),
                body: body.to_vec(),
                log: None,
            };
            bridge.state.set_in_flight(&entry).unwrap();
            seq
        };
        let landed = in_flight(&bridge, b"v=1 t=1 text=landed");
        let (landed_off, _) = other
            .publish(bridge.producer_id, landed, &face, b"v=1 t=1 text=landed")
            .unwrap();
        in_flight(&bridge, b"v=1 t=2 text=never%20sent");
        let claimed = in_flight(&bridge, b"v=1 t=3 text=ours");
        let (claimed_off, _) = other
            .publish(bridge.producer_id, claimed, &face, b"v=1 t=3 text=theirs")
            .unwrap();

        assert!(
            !bridge.settle_in_flight(),
            "the collision drops the connection, so the attach stops"
        );
        let (rows, _) = other.fetch(0, &face, 16).unwrap();
        let offs: Vec<(u64, String)> = rows
            .iter()
            .map(|(off, _, body)| (*off, String::from_utf8_lossy(body).into_owned()))
            .collect();
        assert_eq!(offs.len(), 3, "one record per sequence: {offs:#?}");
        let sent_off = offs
            .iter()
            .find(|(_, body)| body.contains("never%20sent"))
            .map(|(off, _)| *off)
            .expect("the record that never left is published by the re-learn");
        for off in [landed_off, sent_off] {
            assert!(bridge.self_acked.contains(&off), "@{off} is ours");
            assert!(bridge.state.self_acked().contains(&off), "@{off} on disk");
        }
        assert!(
            !bridge.self_acked.contains(&claimed_off)
                && !bridge.state.self_acked().contains(&claimed_off),
            "another writer's record is never whitelisted"
        );
        assert_eq!(bridge.collided, Some((claimed, claimed_off)));
        assert!(
            bridge.state.in_flight().unwrap().is_empty(),
            "every entry is settled"
        );
    }

    #[test]
    fn a_reply_entry_cut_before_its_witness_is_set_aside_and_the_next_boot_attaches() {
        for sweep in [true, false] {
            let (lane, mut bridge) = FakeVerbLane::bridge(&[]);
            bridge.attach_past_an_earlier_boots_run = !sweep;
            let socket = lane.dir.path().join("broker.sock");
            let broker = crate::test_broker(lane.dir.path().join("broker.log")).unwrap();
            let _server = broker.serve(&socket).unwrap();
            bridge.cfg.broker = socket.to_string_lossy().into_owned();
            bridge.simulate_boot(crate::durability::test_boot(1), 0);
            bridge.begin_run();
            assert!(bridge.attach_broker(), "boot 1");
            let face = format!(
                "/f/{}/in/{}/s-b/{}/ack",
                bridge.cfg.fleet, bridge.node, bridge.node
            );
            let body = b"v=1 t=1 re=1 verdict=handled".to_vec();
            let pin = plain_pin(bridge.next_seq().unwrap(), &face, &body);
            let text = InFlight {
                pin: pin.clone(),
                subject: face.clone(),
                body,
                log: Some(broker.log_identity()),
            }
            .render();
            bridge.close_subscriptions();
            bridge.conn = None;
            let root = bridge.state.root().to_path_buf();
            std::fs::create_dir_all(root.join("inflight")).unwrap();
            let entry = root.join(format!("inflight/{}", pin.seq));
            let cut = &text[..text.find("log=").expect("its witness")];
            std::fs::write(&entry, cut).unwrap();
            bridge.state = StateDir::open(&root).unwrap();
            bridge.simulate_boot(crate::durability::test_boot(2), 0);
            if sweep {
                bridge.set_aside_torn_state();
                assert_eq!(bridge.set_aside, 1);
            }
            assert_eq!(
                bridge.attach_broker(),
                sweep,
                "sweep={sweep}: boot 2's attach"
            );
            if sweep {
                assert!(bridge.state.in_flight().unwrap().is_empty());
                assert_eq!(
                    std::fs::read_to_string(
                        bridge
                            .state
                            .root()
                            .join(format!("quarantine/inflight.{}", pin.seq))
                    )
                    .unwrap(),
                    cut,
                    "its exact bytes, kept"
                );
            }
            bridge.close_subscriptions();
        }
    }

    #[test]
    fn the_quarantine_count_is_told_and_told_again_as_the_owner_empties_it() {
        for endpoint_lane in [aterm_types::control_verbs::BRIDGE_LANE, 4] {
            let (lane, mut bridge) = FakeVerbLane::bridge(&[]);
            bridge.endpoint_lane = endpoint_lane;
            let reads = endpoint_lane >= LINK_QUARANTINE_LANE;
            let root = bridge.state.root().to_path_buf();
            bridge.simulate_boot(crate::durability::test_boot(1), 0);
            bridge.begin_run();
            std::fs::create_dir_all(root.join("inflight")).unwrap();
            std::fs::write(root.join("inflight/8"), b"v=1 cut").unwrap();
            bridge.simulate_boot(crate::durability::test_boot(2), 0);
            bridge.set_aside_torn_state();
            bridge.tell_set_aside();
            bridge.tell_quarantine();
            let told = |n: usize| lane.links()[n..].to_vec();
            if !reads {
                assert_eq!(told(0), ["link set-aside files=1"], "lane {endpoint_lane}");
                assert!(!bridge.watches_quarantine());
                continue;
            }
            assert_eq!(
                told(0),
                ["link set-aside files=1", "link quarantine torn=1"]
            );
            assert!(bridge.watches_quarantine(), "a count above 0 is watched");
            bridge.tell_quarantine();
            assert_eq!(told(2), Vec::<String>::new(), "nothing moved, nothing said");
            std::fs::remove_file(root.join("quarantine/inflight.8")).unwrap();
            assert!(bridge.watches_quarantine());
            bridge.tell_quarantine();
            assert_eq!(told(2), ["link quarantine torn=0"], "the owner emptied it");
            assert!(
                !bridge.watches_quarantine(),
                "and it is no longer looked at"
            );
            bridge.quarantine_said = None;
            bridge.tell_quarantine();
            assert_eq!(told(3), ["link quarantine torn=0"], "said at every start");
            std::fs::remove_dir(root.join("quarantine")).unwrap();
            bridge.quarantine_said = None;
            bridge.tell_quarantine();
            assert_eq!(
                told(4),
                ["link quarantine torn=0"],
                "a quarantine removed whole holds none"
            );
        }
    }

    #[test]
    fn the_quarantine_emptied_is_told_while_the_broker_is_away() {
        let (lane, mut bridge) = FakeVerbLane::bridge(&[]);
        bridge.endpoint_lane = aterm_types::control_verbs::BRIDGE_LANE;
        let root = bridge.state.root().to_path_buf();
        bridge.simulate_boot(crate::durability::test_boot(1), 0);
        bridge.begin_run();
        std::fs::create_dir_all(root.join("inflight")).unwrap();
        std::fs::write(root.join("inflight/8"), b"v=1 cut").unwrap();
        bridge.simulate_boot(crate::durability::test_boot(2), 0);
        bridge.set_aside_torn_state();
        bridge.tell_set_aside();
        bridge.tell_quarantine();
        assert_eq!(
            lane.links(),
            ["link set-aside files=1", "link quarantine torn=1"]
        );
        std::fs::remove_file(root.join("quarantine/inflight.8")).unwrap();
        let quarantine_said = |lane: &FakeVerbLane| {
            lane.links()
                .into_iter()
                .filter(|l| l.starts_with("link quarantine"))
                .collect::<Vec<_>>()
        };
        let mut backoff = Backoff::default();
        bridge.roster_due = bridge.fx.now();
        assert!(bridge.pass(&mut backoff).is_none(), "the bridge runs on");
        assert!(bridge.conn.is_none(), "no broker was reached");
        assert!(
            lane.links().iter().any(|l| l.starts_with("link down")),
            "the attach failed: {:?}",
            lane.links()
        );
        assert_eq!(
            quarantine_said(&lane),
            ["link quarantine torn=1", "link quarantine torn=0"],
            "the owner emptied it, and aterm heard it with no broker"
        );
        assert!(!bridge.watches_quarantine());
        bridge.roster_due = bridge.fx.now();
        assert!(bridge.pass(&mut backoff).is_none());
        assert_eq!(
            quarantine_said(&lane).len(),
            2,
            "a count no longer watched is not looked at again"
        );
        bridge.close_subscriptions();
    }

    /// A QUIT WHOSE HAND-OFF THE DISK REFUSES IS NO CLEAN STOP (B19 review): the journal keeps a
    /// record an older build would not find, so the run record stays open, and the next bridge
    /// of this build reads the record from the journal, which names that run. CONTROL: the same
    /// quit with its hand-off written stops cleanly, and the next bridge takes the file back.
    #[test]
    fn a_quit_whose_hand_off_the_disk_refuses_leaves_the_run_open() {
        for refused in [true, false] {
            let (_lane, mut bridge) = FakeVerbLane::bridge(&[]);
            bridge.simulate_boot(crate::durability::test_boot(1), 0);
            bridge.begin_run();
            let pin = plain_pin(7, "/f/f/in/n-1/s-b/n-1/note", b"v=1 t=1");
            bridge.state.set_post_pin("s-a", 1, &pin).unwrap();
            if refused {
                bridge.state.refuse_writes_of(Some("sent/s-a.1"));
            }
            bridge.bye = Some(Bye::Quit);
            bridge.end_run();
            bridge.state.refuse_writes_of(None);
            let run = bridge.state.last_run().unwrap().expect("a run record");
            assert_eq!(run.clean, !refused, "refused={refused}");
            let next = StateDir::open(bridge.state.root()).unwrap();
            assert_eq!(
                next.post_pin("s-a", 1).unwrap(),
                Some(pin),
                "refused={refused}"
            );
        }
    }

    #[test]
    fn a_handoff_leaves_the_run_to_its_successor_and_a_quit_stops_it_cleanly() {
        for bye in [Bye::Handoff, Bye::Quit] {
            let (_lane, mut bridge) = FakeVerbLane::bridge(&[]);
            bridge.simulate_boot(crate::durability::test_boot(1), 0);
            bridge.begin_run();
            let pin = plain_pin(7, "/f/f/in/n-1/s-b/n-1/note", b"v=1 t=1");
            bridge.state.set_post_pin("s-a", 1, &pin).unwrap();
            bridge.bye = Some(bye);
            bridge.end_run();
            let run = bridge.state.last_run().unwrap().expect("a run record");
            assert_eq!(
                run.clean,
                bye == Bye::Quit,
                "{bye:?}: a handoff leaves the run open"
            );
            let successor = StateDir::open(bridge.state.root()).unwrap();
            successor.set_inc_seam(crate::state::IncSeam {
                boot: Some(Some(crate::durability::test_boot(1))),
                kill_at: None,
                mutant: 0,
                span: None,
                off_volume: false,
            });
            let began = successor.begin_run().unwrap();
            let stopped = successor.end_run_clean().unwrap();
            match bye {
                Bye::Handoff => {
                    assert_eq!(
                        (began.ordered, began.full),
                        (0, 2),
                        "the successor admits OPEN independently, in full"
                    );
                    assert_eq!(stopped.files, 1, "the reservation, inherited and flushed");
                }
                Bye::Quit => {
                    assert_eq!(
                        (began.ordered, began.full),
                        (0, 2),
                        "CONTROL: the record written again, in full over a clean stop"
                    );
                    assert_eq!(stopped.files, 0, "CONTROL: nothing inherited");
                }
            }
        }
    }

    #[test]
    fn an_in_flight_entry_of_an_earlier_boot_is_retired_and_its_attach_publishes_nothing() {
        for reboot in [true, false] {
            let (lane, mut bridge) = FakeVerbLane::bridge(&[]);
            let socket = lane.dir.path().join("broker.sock");
            let broker = crate::test_broker(lane.dir.path().join("broker.log")).unwrap();
            let _server = broker.serve(&socket).unwrap();
            bridge.cfg.broker = socket.to_string_lossy().into_owned();
            bridge.simulate_boot(crate::durability::test_boot(1), 0);
            bridge.begin_run();
            assert!(bridge.attach_broker(), "boot 1");
            let face = format!(
                "/f/{}/in/{}/s-b/{}/note",
                bridge.cfg.fleet, bridge.node, bridge.node
            );
            let body = b"v=1 t=1 from=s-a text=landed%20then%20lost".to_vec();
            let pin = plain_pin(bridge.next_seq().unwrap(), &face, &body);
            bridge
                .state
                .set_in_flight(&InFlight {
                    pin: pin.clone(),
                    subject: face.clone(),
                    body,
                    log: Some(broker.log_identity()),
                })
                .unwrap();
            bridge.close_subscriptions();
            bridge.conn = None;
            let boot = if reboot { 2 } else { 1 };
            bridge.simulate_boot(crate::durability::test_boot(boot), 0);
            bridge.set_aside_torn_state();
            bridge.retire_in_flight_state();
            bridge.begin_run();
            assert!(bridge.attach_broker(), "boot {boot}'s attach");
            let (mut reader, _) = bridge.connect().unwrap();
            let (rows, _) = reader.fetch(0, &face, 16).unwrap();
            assert!(
                bridge.state.in_flight().unwrap().is_empty(),
                "boot {boot}: the entry is settled either way"
            );
            let retired = bridge
                .state
                .root()
                .join(format!("quarantine/retired.inflight.{}", pin.seq));
            if reboot {
                assert!(rows.is_empty(), "a retired entry was published: {rows:#?}");
                assert!(retired.exists(), "its bytes are kept in the quarantine");
            } else {
                assert_eq!(
                    rows.len(),
                    1,
                    "CONTROL: within one boot the attach publishes it: {rows:#?}"
                );
                assert!(!retired.exists(), "nothing is retired within one boot");
            }
            bridge.close_subscriptions();
        }
    }

    #[cfg(unix)]
    #[test]
    fn no_attach_runs_while_run_names_an_earlier_boot_so_a_relaunch_retires_nothing_live() {
        for reopen in [false, true] {
            let (lane, mut bridge) = FakeVerbLane::bridge(&[]);
            let (socket, _broker, _server) = serve_log(&lane, "open-fence");
            bridge.cfg.broker = socket;
            bridge.simulate_boot(crate::durability::test_boot(1), 0);
            bridge.begin_run();
            bridge.state.end_run_clean().unwrap();
            let root = bridge.state.root().to_path_buf();
            let refusal = (
                root.as_path(),
                aterm_durable::Flush::Durable,
                io::ErrorKind::PermissionDenied,
            );
            bridge.state.refuse_directory_flush(Some(refusal));
            bridge.begin_run();
            assert!(
                !bridge.state.last_run().unwrap().unwrap().clean,
                "OPEN is visible"
            );
            if reopen {
                bridge.state = StateDir::open(&root).unwrap();
                bridge.simulate_boot(crate::durability::test_boot(1), 0);
            }
            bridge.state.refuse_directory_flush(Some(refusal));
            assert!(!bridge.attach_broker(), "unadmitted OPEN, reopen={reopen}");
            assert!(bridge.conn.is_none());
            assert_eq!(bridge.state.incarnation(), 0);
            let (mut reader, _) = bridge.connect().unwrap();
            let (rows, _) = reader
                .fetch(0, &format!("/f/{}/>", bridge.cfg.fleet), 64)
                .unwrap();
            assert!(rows.is_empty(), "a refused admission published {rows:?}");
            bridge.state.refuse_directory_flush(None);
            assert!(bridge.attach_broker(), "publication retry, reopen={reopen}");
            bridge.close_subscriptions();
        }
        for (wait, on_volume) in [(true, false), (false, false), (false, true)] {
            let case = format!("wait={wait} on_volume={on_volume}");
            let (lane, mut bridge) = FakeVerbLane::bridge(&[]);
            let (socket, _broker, _server) = serve_log(&lane, "f2-run");
            bridge.cfg.broker = socket;
            bridge.attach_past_an_earlier_boots_run = !wait;
            let in_boot = |bridge: &Bridge, n| {
                bridge.state.set_inc_seam(crate::state::IncSeam {
                    boot: Some(Some(crate::durability::test_boot(n))),
                    off_volume: !on_volume,
                    kill_at: None,
                    mutant: 0,
                    span: None,
                });
            };
            in_boot(&bridge, 1);
            bridge.begin_run();
            in_boot(&bridge, 2);
            bridge.state.refuse_writes_of(Some("run"));
            bridge.set_aside_torn_state();
            bridge.retire_in_flight_state();
            bridge.begin_run();
            if wait {
                assert!(
                    !bridge.attach_broker(),
                    "{case}: an attach ran while `run` named boot 1's unclean stop"
                );
                assert!(bridge.conn.is_none(), "{case}: no connection is kept");
                assert_eq!(bridge.state.incarnation(), 0, "{case}: no incarnation");
                let (mut reader, _) = bridge.connect().unwrap();
                let (rows, _) = reader
                    .fetch(0, &format!("/f/{}/>", bridge.cfg.fleet), 64)
                    .unwrap();
                assert!(rows.is_empty(), "{case}: the attach wrote: {rows:#?}");
                bridge.state.refuse_writes_of(None);
            }
            assert!(bridge.attach_broker(), "{case}: boot 2's attach");
            if wait {
                assert_eq!(
                    bridge.state.last_run().unwrap(),
                    Some(crate::state::LastRun {
                        boot: Some(crate::durability::test_boot(2)),
                        clean: false,
                    }),
                    "{case}: the attach wrote `run` before anything else"
                );
            }
            let subject = format!(
                "/f/{}/in/{}/s-local/{}/note",
                bridge.cfg.fleet, bridge.node, bridge.node
            );
            let published = leave_journaled_publications(
                &mut bridge,
                &[(subject, b"v=1 t=1 text=live".to_vec())],
            );
            let (seq, off) = published[0];
            bridge.close_subscriptions();
            bridge.conn = None;
            bridge.self_acked = bridge.state.self_acked();
            bridge.set_aside_torn_state();
            bridge.retire_in_flight_state();
            bridge.begin_run();
            assert!(bridge.attach_broker(), "{case}: the relaunch's attach");
            let retired = bridge
                .state
                .root()
                .join(format!(
                    "{}/{}inflight.{seq}",
                    crate::state::QUARANTINE,
                    crate::state::RETIRED_PREFIX
                ))
                .exists();
            let relearned = bridge.state.self_acked().contains(&off);
            let accused = bridge.is_forged_self(off);
            if wait || on_volume {
                assert!(
                    !retired && relearned && !accused,
                    "{case}: retired={retired} relearned={relearned} accused={accused}"
                );
            } else {
                assert!(
                    retired && !relearned && accused,
                    "{case}: NEGATIVE CONTROL: retired={retired} relearned={relearned} \
                     accused={accused}"
                );
            }
            bridge.close_subscriptions();
        }
    }

    #[cfg(unix)]
    #[test]
    fn no_attach_runs_under_a_clean_stops_record_so_the_next_boot_sweeps_what_it_wrote() {
        for widened in [true, false] {
            let case = format!("widened={widened}");
            let (lane, mut bridge) = FakeVerbLane::bridge(&[]);
            let (socket, broker, _server) = serve_log(&lane, "s7-run");
            bridge.cfg.broker = socket;
            bridge.attach_past_an_earlier_boots_run = !widened;
            bridge.simulate_boot(crate::durability::test_boot(1), 0);
            bridge.begin_run();
            bridge.state.end_run_clean().unwrap();
            bridge.state.refuse_writes_of(Some("run"));
            if bridge.recover() {
                bridge.begin_run();
            }
            let clean_one = Some(crate::state::LastRun {
                boot: Some(crate::durability::test_boot(1)),
                clean: true,
            });
            assert_eq!(bridge.state.last_run().unwrap(), clean_one, "{case}");
            if widened {
                assert!(
                    !bridge.attach_broker(),
                    "{case}: an attach ran under a clean stop's record"
                );
                assert!(bridge.conn.is_none(), "{case}: no connection is kept");
                assert_eq!(bridge.state.incarnation(), 0, "{case}: no incarnation");
                bridge.state.refuse_writes_of(None);
            }
            assert!(bridge.attach_broker(), "{case}: boot 1's attach");
            let run = bridge.state.last_run().unwrap();
            if widened {
                assert_eq!(
                    run,
                    Some(crate::state::LastRun {
                        boot: Some(crate::durability::test_boot(1)),
                        clean: false,
                    }),
                    "{case}: the attach wrote `run` before anything else"
                );
            } else {
                assert_eq!(run, clean_one, "{case}: CONTROL: attached under it");
            }
            let face = format!(
                "/f/{}/in/{}/s-b/{}/note",
                bridge.cfg.fleet, bridge.node, bridge.node
            );
            let body = b"v=1 t=1 text=live".to_vec();
            let pin = plain_pin(bridge.next_seq().unwrap(), &face, &body);
            bridge
                .state
                .set_in_flight(&InFlight {
                    pin: pin.clone(),
                    subject: face,
                    body,
                    log: Some(broker.log_identity()),
                })
                .unwrap();
            bridge.close_subscriptions();
            bridge.conn = None;
            let root = bridge.state.root().to_path_buf();
            let journal = root.join(crate::state::journal::FILE);
            let mut torn = std::fs::read(&journal).unwrap();
            let last = torn.len() - 1;
            torn[last] ^= 0x01;
            std::fs::write(&journal, &torn).unwrap();
            assert_eq!(
                crate::state::journal::read(&torn).end,
                crate::state::journal::End::Torn,
                "PRECONDITION: a tear inside the last record's bytes, as a power cut leaves"
            );
            bridge.state = StateDir::open(&root).unwrap();
            bridge.attach_past_an_earlier_boots_run = false;
            bridge.simulate_boot(crate::durability::test_boot(2), 0);
            if bridge.recover() {
                bridge.begin_run();
            }
            assert_eq!(
                bridge.attach_broker(),
                widened,
                "{case}: boot 2's attach (the torn journal set aside, or refused)"
            );
            assert_eq!(
                root.join("quarantine/journal.posts").exists(),
                widened,
                "{case}: the torn journal is in the quarantine"
            );
            bridge.close_subscriptions();
        }
    }

    #[cfg(unix)]
    #[test]
    fn a_post_routes_on_a_tofu_pin_only_once_it_is_on_the_media() {
        for settle in [true, false] {
            let (_lane, mut bridge) = FakeVerbLane::bridge(&[]);
            bridge.route_on_a_pin_that_reads = !settle;
            let root = bridge.state.root().to_path_buf();
            bridge.state.refuse_directory_flush(Some((
                root.as_path(),
                aterm_durable::Flush::Durable,
                io::ErrorKind::Other,
            )));
            assert!(
                bridge.state.pin("s-x", "n-far").is_err(),
                "the first sight's flush failed"
            );
            bridge.state.refuse_directory_flush(None);
            assert_eq!(
                bridge.state.pins().get("s-x").map(String::as_str),
                Some("n-far"),
                "and it reads"
            );
            assert!(!bridge.state.on_the_media("pins"));
            let _ = bridge.pinned_node_for("s-x");
            assert_eq!(
                bridge.state.on_the_media("pins"),
                settle,
                "settle={settle}: the table written again before the route"
            );
            assert_eq!(
                bridge.state.pins().get("s-x").map(String::as_str),
                Some("n-far")
            );
        }
    }

    #[test]
    fn a_bridge_whose_aterm_is_gone_writes_nothing_and_never_redials() {
        let (lane, mut bridge) = FakeVerbLane::bridge(&[]);
        let socket = lane.dir.path().join("broker.sock");
        let broker = crate::test_broker(lane.dir.path().join("broker.log")).unwrap();
        let _server = broker.serve(&socket).unwrap();
        bridge.cfg.broker = socket.to_string_lossy().into_owned();
        let (mut reader, _) = bridge.connect().unwrap();
        let state = bridge.state.root().to_path_buf();
        bridge.mailbox.push_aterm_closed();

        assert!(
            !bridge.attach_broker(),
            "an attach for an aterm that is gone completed"
        );
        assert_eq!(
            StateDir::open(&state).unwrap().incarnation(),
            0,
            "the attach took an incarnation"
        );
        let (rows, _) = reader.fetch(0, "/f/f/>", 64).unwrap();
        assert!(rows.is_empty(), "the attach wrote to the bus: {rows:#?}");

        let quiet = lane.dir.path().join("quiet.sock");
        let listener = std::os::unix::net::UnixListener::bind(&quiet).unwrap();
        listener.set_nonblocking(true).unwrap();
        bridge.cfg.broker = quiet.to_string_lossy().into_owned();
        bridge.run().expect("a bridge with no aterm ends cleanly");
        assert_eq!(
            listener.accept().map(|_| ()).map_err(|e| e.kind()),
            Err(io::ErrorKind::WouldBlock),
            "the loop redialled for an aterm that is gone"
        );
        let (rows, _) = reader.fetch(0, "/f/f/>", 64).unwrap();
        assert!(rows.is_empty(), "the loop wrote to the bus: {rows:#?}");
    }

    fn rig_with_broker(
        sessions: &[(u64, &str)],
    ) -> (FakeVerbLane, Bridge, astream_broker::BrokerHandle) {
        let (lane, bridge, server, _) = rig_with_kept_broker(sessions, false);
        (lane, bridge, server)
    }

    fn rig_with_kept_broker(
        sessions: &[(u64, &str)],
        retained: bool,
    ) -> (
        FakeVerbLane,
        Bridge,
        astream_broker::BrokerHandle,
        astream_broker::Broker,
    ) {
        rig_retained_by(sessions, retained.then(crate::broker_rules::default_rules))
    }

    /// [`rig_with_kept_broker`] over a broker retained by `rules` (none: unretained), set as
    /// aterm sets its own at the open.
    fn rig_retained_by(
        sessions: &[(u64, &str)],
        rules: Option<astream_broker::Retention>,
    ) -> (
        FakeVerbLane,
        Bridge,
        astream_broker::BrokerHandle,
        astream_broker::Broker,
    ) {
        let (lane, mut bridge) = FakeVerbLane::bridge(sessions);
        let socket = lane.dir.path().join("broker.sock");
        let broker = crate::test_broker(lane.dir.path().join("broker.log")).unwrap();
        let retained = rules.is_some();
        if let Some(rules) = rules {
            broker.set_retention(rules).unwrap();
        }
        let server = broker.serve(&socket).unwrap();
        bridge.cfg.broker = socket.to_string_lossy().into_owned();
        let (conn, _) = bridge.connect().unwrap();
        bridge.conn = Some(conn.into());
        bridge.note_ack(Duration::from_millis(1));
        assert!(bridge.link.up);
        if retained {
            bridge.read_log_identity().unwrap();
            bridge.note_broker_retention();
        }
        (lane, bridge, server, broker)
    }

    #[test]
    fn a_face_at_its_bound_pauses_and_resumes_with_every_record_once_in_order() {
        use crate::mailbox::QUEUE_ROWS_MAX;
        let (lane, mut bridge, _server) = rig_with_broker(&[]);
        let inc = bridge.inc;
        let links = lane.links.lock().unwrap().len();
        let (mut publisher, _) = bridge.connect().unwrap();
        let lane_subject = format!("/f/f/in/{}/s-a/n-x/note", bridge.node);
        let total = QUEUE_ROWS_MAX + QUEUE_ROWS_MAX / 2 + 7;
        let published: Vec<u64> = publisher
            .publish_pipelined(77, &lane_subject, &vec![b"v=1".as_slice(); total], 64)
            .unwrap()
            .into_iter()
            .map(|(off, deduped)| {
                assert!(!deduped);
                off
            })
            .collect();

        assert!(bridge.subscribe_inbox());
        let deadline = Instant::now() + aterm_spec::wait::MUST_HAPPEN;
        while bridge.mailbox.paused(Source::Inbox).is_none() {
            assert!(Instant::now() < deadline, "the inbox reader never paused");
            std::thread::yield_now();
        }
        assert_eq!(
            bridge.mailbox.paused(Source::Inbox),
            Some(published[QUEUE_ROWS_MAX - 1] + 1),
            "it pauses at the record that fills the queue, to resume after it"
        );

        let mut taken = Vec::new();
        let mut resumes = 0;
        while taken.len() < total {
            match bridge.mailbox.take(aterm_spec::wait::MUST_HAPPEN) {
                Some(Item::Inbox((off, _, _))) => taken.push(off),
                Some(Item::Resume(Source::Inbox, from)) => {
                    resumes += 1;
                    bridge.resume_face(Source::Inbox, from);
                    assert!(bridge.conn.is_some(), "a resume is not a reconnect");
                }
                other => panic!("after {} records: {other:?}", taken.len()),
            }
        }
        assert_eq!(taken, published, "every record once, in log order");
        assert!(resumes >= 1);
        assert_eq!(
            lane.links.lock().unwrap().len(),
            links,
            "a pause tells the endpoint nothing"
        );
        assert!(bridge.link.up);
        assert_eq!(bridge.inc, inc, "no new incarnation");
        bridge.close_subscriptions();
    }

    #[test]
    fn a_duty_resumes_a_paused_fleet_face_before_the_next_halt() {
        use crate::mailbox::QUEUE_ROWS_MAX;

        const SID: &str = "s-busy";
        let (lane, mut bridge, _server) = rig_with_broker(&[(1, SID)]);
        bridge.refresh_sessions().unwrap();
        let inc = bridge.inc;
        let links = lane.links.lock().unwrap().len();
        let (mut writer, _) = bridge.connect().unwrap();
        let filled = writer
            .publish_pipelined(
                77,
                "/f/f/fleet/h-fill/barrier",
                &[b"v=1".as_slice(); QUEUE_ROWS_MAX],
                64,
            )
            .unwrap();
        assert!(filled.iter().all(|&(_, deduped)| !deduped));
        assert!(bridge.subscribe_fleet(0));
        aterm_spec::wait::until_within(
            aterm_spec::wait::MUST_HAPPEN,
            "the fleet reader to pause at its mailbox bound",
            || bridge.mailbox.paused(Source::Fleet).map(|_| ()),
        );

        bridge.service_urgent();
        let pending = bridge.mailbox.take(Duration::ZERO);
        assert!(
            pending.is_none(),
            "the duty left fleet work for the outer loop: {pending:?}"
        );
        writer
            .publish(
                91,
                1,
                "/f/f/fleet/h-t/halt",
                b"v=1 t=3 state=on reason=stop",
            )
            .unwrap();
        aterm_spec::wait::until_within(
            aterm_spec::wait::MUST_HAPPEN,
            "the resumed fleet reader to queue the next halt",
            || (bridge.mailbox.replay_pressure() == ReplayPressure::Urgent).then_some(()),
        );
        bridge.service_urgent();
        assert!(bridge.halt_applied);
        assert!(lane
            .transcript()
            .contains(&format!("hold {SID} on reason=stop origin=fleet")));
        assert_eq!(bridge.inc, inc, "resuming the face is not a reconnect");
        assert!(lane.links.lock().unwrap()[links..]
            .iter()
            .all(|line| !line.starts_with("link down")));
        assert!(bridge.conn.is_some() && bridge.link.up);
        bridge.close_subscriptions();
    }

    #[test]
    fn a_cut_subscription_is_named_by_its_face_and_a_dead_broker_is_closed() {
        let (lane, mut bridge, mut server) = rig_with_broker(&[]);
        let last_link = |lane: &FakeVerbLane| lane.links.lock().unwrap().last().cloned();

        bridge.subscription_lost(Source::Say);
        assert!(bridge.conn.is_none(), "the set reconnects either way");
        assert_eq!(
            last_link(&lane).as_deref(),
            Some("link down reason=cut-say")
        );

        let (conn, _) = bridge.connect().unwrap();
        bridge.conn = Some(conn.into());
        bridge.note_ack(Duration::from_millis(1));
        server.shutdown();
        bridge.subscription_lost(Source::Inbox);
        assert!(bridge.conn.is_none());
        assert_eq!(last_link(&lane).as_deref(), Some("link down reason=closed"));
    }

    #[test]
    fn the_broadcast_face_is_held_only_while_a_session_has_a_topic() {
        let (lane, mut bridge, _server) = rig_with_broker(&[(1, "s-a")]);
        bridge.refresh_sessions().unwrap();
        let (mut publisher, _) = bridge.connect().unwrap();
        let body = Body::new(0).encode();
        let mut shout = |seq: u64| {
            publisher
                .publish(77, seq, "/f/f/pub/n-src/s-src/say/t", &body)
                .unwrap()
                .0
        };
        let topic = |bridge: &mut Bridge, words: &str| {
            bridge.topic_change("s-a", &mut words.split_whitespace());
            assert!(bridge.sync_say_face());
        };
        let take_say =
            |bridge: &mut Bridge| match bridge.mailbox.take(aterm_spec::wait::MUST_HAPPEN) {
                Some(Item::Say(r)) => {
                    bridge.on_say_record(&r);
                    r.0
                }
                other => panic!("{other:?}"),
            };

        assert!(bridge.sync_say_face());
        assert!(
            !bridge.say_face_held(),
            "no topic, no fleet-wide subscription"
        );
        let before = shout(1);
        topic(&mut bridge, &format!("add t since=@{before}"));
        assert!(bridge.say_face_held());
        let live = shout(2);
        assert_eq!(
            (take_say(&mut bridge), take_say(&mut bridge)),
            (before, live)
        );
        assert_eq!(lane.delivery_offsets(), [before, live]);

        let generation = bridge.mailbox.broker_generation(Source::Say);
        topic(&mut bridge, "drop t");
        assert!(!bridge.say_face_held(), "the last topic closes the face");
        assert_ne!(bridge.mailbox.broker_generation(Source::Say), generation);
        let unheard = shout(3);

        topic(&mut bridge, "add t since=head");
        assert!(bridge.say_face_held());
        let next = shout(4);
        assert_eq!(take_say(&mut bridge), next);
        assert_eq!(lane.delivery_offsets(), [before, live, next]);
        assert!(!lane.delivery_offsets().contains(&unheard));
        assert!(bridge.conn.is_some() && bridge.link.up);
        assert!(
            bridge.mailbox.take(Duration::ZERO).is_none(),
            "the closed face's reader ended as no closure"
        );
        bridge.close_subscriptions();
    }

    #[test]
    fn a_new_head_topic_after_a_shorter_log_hears_its_first_new_broadcast() {
        const SID: &str = "s-a";
        let (lane, mut bridge) = FakeVerbLane::bridge(&[(1, SID)]);
        bridge.refresh_sessions().unwrap();
        let (a, _broker_a, _server_a) = serve_log(&lane, "a");
        bridge.cfg.broker = a;
        let (mut writer_a, _) = bridge.connect().unwrap();
        for seq in 1..=64 {
            writer_a.publish(77, seq, "/f/f/x/history", b"v=1").unwrap();
        }
        lane.opt_in(SID, &["topic t since=head"]);
        assert!(bridge.attach_broker());
        assert_eq!(bridge.attachment, Attachment::Inherited);
        assert!(bridge.sync_say_face());
        let body = Body::new(0).encode();
        let (off, _) = writer_a
            .publish(77, 65, "/f/f/pub/n-src/s-src/say/t", &body)
            .unwrap();
        match bridge.mailbox.take(aterm_spec::wait::MUST_HAPPEN) {
            Some(Item::Say(r)) => {
                assert_eq!(r.0, off);
                bridge.on_say_record(&r);
            }
            other => panic!("the old log's broadcast: {other:?}"),
        }
        lane.opt_in(SID, &[]);
        bridge.resample_topics();
        assert!(bridge.sync_say_face());
        assert!(bridge.per_sid.topics.is_empty() && !bridge.say_face_held());
        let old_drain = bridge.say_drained_to;
        bridge.close_subscriptions();
        bridge.conn = None;

        let (b, _broker_b, _server_b) = serve_log(&lane, "b");
        bridge.cfg.broker = b;
        lane.opt_in(SID, &["topic t since=head"]);
        assert!(bridge.attach_broker());
        let (mut writer_b, _) = bridge.connect().unwrap();
        let (_, (_, head)) = writer_b.fetch(0, &subject::say_filter("f"), 0).unwrap();
        assert!(head < old_drain, "the new log must be shorter");
        assert_eq!(
            bridge.per_sid.topics[SID]["t"].next, head,
            "an empty catch-up must not skip the next record at the new head"
        );
        assert!(bridge.sync_say_face());
        let before = lane.delivery_offsets().len();
        let (off, _) = writer_b
            .publish(78, 1, "/f/f/pub/n-src/s-src/say/t", &body)
            .unwrap();
        assert_eq!(off, head);
        match bridge.mailbox.take(aterm_spec::wait::MUST_HAPPEN) {
            Some(Item::Say(r)) => bridge.on_say_record(&r),
            other => panic!("the new log's first broadcast: {other:?}"),
        }
        assert_eq!(&lane.delivery_offsets()[before..], &[off]);
        assert_eq!(bridge.per_sid.topics[SID]["t"].next, off + 1);
        assert_eq!(bridge.state.topics(SID), bridge.per_sid.topics[SID]);
        bridge.close_subscriptions();
    }

    #[cfg(unix)]
    fn fan_in_next_say(bridge: &mut Bridge) -> u64 {
        match bridge.mailbox.take(aterm_spec::wait::MUST_HAPPEN) {
            Some(Item::Say(r)) => {
                bridge.on_say_record(&r);
                r.0
            }
            other => panic!("a broadcast: {other:?}"),
        }
    }

    #[cfg(unix)]
    #[test]
    fn a_broadcast_cursor_waits_for_its_refill_floor_to_be_persisted() {
        const SID: &str = "s-kept";
        let (lane, mut bridge) = FakeVerbLane::bridge(&[(1, SID)]);
        let state = bridge.state.root().to_path_buf();
        let (bus, _broker, _server) = serve_log(&lane, "bus");
        bridge.cfg.broker = bus.clone();
        lane.opt_in(SID, &["topic t since=@0"]);
        assert!(bridge.attach_broker());
        assert!(bridge.sync_say_face());
        let before = bridge.per_sid.topics[SID]["t"].next;
        let (mut writer, _) = bridge.connect().unwrap();
        let (off, _) = writer
            .publish(77, 1, "/f/f/pub/n-src/s-src/say/t", &Body::new(0).encode())
            .unwrap();
        std::fs::write(state.join("unhandled"), b"owned write obstruction").unwrap();
        assert_eq!(fan_in_next_say(&mut bridge), off);
        assert_eq!(lane.delivery_offsets(), [off], "the endpoint took the row");
        assert_eq!(
            bridge.per_sid.topics[SID]["t"].next, before,
            "memory must wait"
        );
        assert_eq!(bridge.state.topics(SID)["t"].next, before, "disk must wait");
        for _ in 0..2 {
            assert!(!bridge.catch_up_topic_page(SID));
            assert_eq!(bridge.per_sid.topics[SID]["t"].next, before);
            assert_eq!(bridge.state.topics(SID)["t"].next, before);
            assert_eq!(lane.delivery_offsets(), [off], "retry dedups the ring row");
        }

        std::fs::remove_file(state.join("unhandled")).unwrap();
        assert!(bridge.catch_up_topic_page(SID));
        assert_eq!(bridge.per_sid.topics[SID]["t"].next, off + 1);
        assert_eq!(bridge.state.topics(SID)["t"].next, off + 1);
        assert_eq!(
            bridge.state.topic_floors(SID).unwrap(),
            BTreeMap::from([("t".to_string(), off)])
        );
        assert_eq!(lane.delivery_offsets(), [off]);
        bridge.close_subscriptions();
        drop(bridge);

        let (successor_lane, mut successor) =
            FakeVerbLane::bridge_with_state(&[(1, SID)], Some(&state));
        successor.cfg.broker = bus;
        successor_lane.opt_in(SID, &["topic t since=@0"]);
        assert!(successor.attach_broker());
        assert!(successor.sync_say_face());
        assert_eq!(fan_in_next_say(&mut successor), off);
        assert_eq!(
            successor_lane.delivery_offsets(),
            [off],
            "the empty successor refills"
        );
        successor.close_subscriptions();
    }

    #[cfg(unix)]
    #[test]
    fn an_uncertain_broadcast_refill_floor_refuses_attach_until_repaired() {
        const SID: &str = "s-kept";
        for damage in ["directory", "dangling", "utf8", "malformed"] {
            let (lane, mut first) = FakeVerbLane::bridge(&[(1, SID)]);
            let state = first.state.root().to_path_buf();
            let (bus, _broker, _server) = serve_log(&lane, "bus");
            first.cfg.broker = bus.clone();
            lane.opt_in(SID, &["topic t since=@0"]);
            assert!(first.attach_broker());
            assert!(first.sync_say_face());
            let (mut writer, _) = first.connect().unwrap();
            let (off, _) = writer
                .publish(77, 1, "/f/f/pub/n-src/s-src/say/t", &Body::new(0).encode())
                .unwrap();
            assert_eq!(fan_in_next_say(&mut first), off);
            first.close_subscriptions();
            drop(first);

            let floor = state.join("unhandled").join(SID);
            let saved = std::fs::read(&floor).unwrap();
            let cursors = std::fs::read(state.join("topics").join(SID)).unwrap();
            std::fs::remove_file(&floor).unwrap();
            match damage {
                "directory" => std::fs::create_dir(&floor).unwrap(),
                "dangling" => {
                    std::os::unix::fs::symlink(state.join("absent-floor"), &floor).unwrap()
                }
                "utf8" => std::fs::write(&floor, [0xff]).unwrap(),
                "malformed" => std::fs::write(&floor, format!("t {off}\nother invalid\n")).unwrap(),
                _ => unreachable!(),
            }
            let damaged_bytes = std::fs::read(&floor).ok();
            let (next_lane, mut next) = FakeVerbLane::bridge_with_state(&[(1, SID)], Some(&state));
            next.cfg.broker = bus;
            next_lane.opt_in(SID, &["topic t since=@0"]);
            assert!(
                !next.attach_broker(),
                "{damage} is uncertainty, not an empty floor"
            );
            assert!(next_lane.delivery_offsets().is_empty());
            assert_eq!(
                std::fs::read(state.join("topics").join(SID)).unwrap(),
                cursors
            );
            assert_eq!(std::fs::read(&floor).ok(), damaged_bytes, "retain {damage}");
            match damage {
                "directory" => std::fs::remove_dir(&floor).unwrap(),
                _ => std::fs::remove_file(&floor).unwrap(),
            }
            std::fs::write(&floor, saved).unwrap();
            assert!(
                next.attach_broker(),
                "{damage}: repaired on the same bridge"
            );
            assert!(next.sync_say_face());
            assert_eq!(fan_in_next_say(&mut next), off);
            assert_eq!(next_lane.delivery_offsets(), [off]);
            next.close_subscriptions();
        }
    }

    #[cfg(unix)]
    #[test]
    fn a_broadcast_refill_floor_without_a_cursor_is_replayed() {
        const SID: &str = "s-kept";
        let (lane, mut first) = FakeVerbLane::bridge(&[(1, SID)]);
        let state = first.state.root().to_path_buf();
        let (bus, _broker, _server) = serve_log(&lane, "bus");
        first.cfg.broker = bus.clone();
        lane.opt_in(SID, &["topic t since=@0"]);
        assert!(first.attach_broker());
        assert!(first.sync_say_face());
        let (mut writer, _) = first.connect().unwrap();
        let (off, _) = writer
            .publish(77, 1, "/f/f/pub/n-src/s-src/say/t", &Body::new(0).encode())
            .unwrap();
        assert_eq!(fan_in_next_say(&mut first), off);
        first.close_subscriptions();
        drop(first);
        std::fs::remove_file(state.join("topics").join(SID)).unwrap();
        let (next_lane, mut next) = FakeVerbLane::bridge_with_state(&[(1, SID)], Some(&state));
        next.cfg.broker = bus;
        next_lane.opt_in(SID, &["topic t since=head"]);
        assert!(next.attach_broker());
        assert_eq!(
            next.per_sid.topics[SID]["t"].next, off,
            "the valid floor is recovery data"
        );
        assert!(next.sync_say_face());
        assert_eq!(fan_in_next_say(&mut next), off);
        assert_eq!(next_lane.delivery_offsets(), [off]);
        next.close_subscriptions();
    }

    #[cfg(unix)]
    #[test]
    fn an_update_offers_the_successor_every_broadcast_its_session_had_not_handled() {
        const SID: &str = "s-kept";
        let (old_lane, mut old) = FakeVerbLane::bridge(&[(1, SID)]);
        let state = old.state.root().to_path_buf();
        let (bus, _broker, _server) = serve_log(&old_lane, "bus");
        old.cfg.broker = bus.clone();
        old_lane.opt_in(SID, &["topic t since=@0"]);
        assert!(old.attach_broker());
        assert!(old.sync_say_face());
        let (mut god, _) = old.connect().unwrap();
        let body = Body::new(0).encode();
        let mut shout = |seq: u64| {
            god.publish(77, seq, "/f/f/pub/n-src/s-src/say/t", &body)
                .unwrap()
                .0
        };
        let floors = |st: &StateDir| {
            st.topic_floors(SID)
                .unwrap()
                .into_iter()
                .collect::<Vec<_>>()
        };

        let b: Vec<u64> = (1..=4).map(&mut shout).collect();
        for want in &b {
            assert_eq!(fan_in_next_say(&mut old), *want);
        }
        assert_eq!(old_lane.delivery_offsets(), b, "ring ids 1 to 4");
        assert_eq!(floors(&old.state), [("t".to_string(), b[0])]);

        old.on_event(&format!("EVENT 1 inbox-seen 2 off={} floor=- seen=2", b[1]));
        assert_eq!(floors(&old.state), [("t".to_string(), b[2])]);
        old.on_event(&format!("EVENT 1 inbox-seen 1 off={} floor=- seen=2", b[0]));
        assert_eq!(floors(&old.state), [("t".to_string(), b[2])]);

        let overlap = shout(5);
        assert_eq!(fan_in_next_say(&mut old), overlap);
        old.fault = Fault::LoseAtermBeforeDeliver;
        let fenced = shout(6);
        assert_eq!(fan_in_next_say(&mut old), fenced);
        let mut taken = b.clone();
        taken.push(overlap);
        assert_eq!(old_lane.delivery_offsets(), taken);
        assert_eq!(old.state.topics(SID)["t"].next, fenced, "the cursor waits");
        assert_eq!(floors(&old.state), [("t".to_string(), b[2])]);
        old.close_subscriptions();
        drop(old);

        let (new_lane, mut new) = FakeVerbLane::bridge_with_state(&[(1, SID)], Some(&state));
        new.cfg.broker = bus;
        new_lane.opt_in(SID, &["topic t since=@0"]);
        assert!(new.attach_broker());
        assert!(new.sync_say_face());
        assert_eq!(new.state.topics(SID)["t"].next, b[2], "rewound, on disk");
        let want = [b[2], b[3], overlap, fenced];
        for off in want {
            assert_eq!(fan_in_next_say(&mut new), off);
        }
        assert_eq!(new_lane.delivery_offsets(), want, "in order, once each");
        assert_eq!(new.state.topics(SID)["t"].next, fenced + 1);
        assert_eq!(
            floors(&new.state),
            [("t".to_string(), b[2])],
            "re-learned from the rows it offered"
        );
        new.on_event(&format!("EVENT 1 inbox-seen 4 off={fenced} floor=- seen=4"));
        assert!(floors(&new.state).is_empty(), "handled in the successor");
        new.close_subscriptions();
    }

    #[cfg(unix)]
    #[test]
    fn a_restarted_bridge_learns_what_its_session_handled_while_it_was_away() {
        const SID: &str = "s-kept";
        for reported in [false, true] {
            let (lane, mut first) = FakeVerbLane::bridge(&[(1, SID)]);
            let state = first.state.root().to_path_buf();
            let (bus, _broker, _server) = serve_log(&lane, "bus");
            first.cfg.broker = bus.clone();
            lane.opt_in(SID, &["topic t since=@0"]);
            assert!(first.attach_broker());
            assert!(first.sync_say_face());
            let (mut god, _) = first.connect().unwrap();
            let body = Body::new(0).encode();
            let b: Vec<u64> = (1..=3)
                .map(|seq| {
                    god.publish(77, seq, "/f/f/pub/n-src/s-src/say/t", &body)
                        .unwrap()
                        .0
                })
                .collect();
            for want in &b {
                assert_eq!(fan_in_next_say(&mut first), *want);
            }
            assert_eq!(first.state.topic_floors(SID).unwrap()["t"], b[0]);
            first.close_subscriptions();
            drop(first);

            let (kept, mut second) = FakeVerbLane::bridge_with_state(&[(1, SID)], Some(&state));
            *kept.delivered.lock().unwrap() = b.clone();
            kept.opt_in(SID, &["topic t since=@0"]);
            if reported {
                kept.report_handled(SID, 3);
            }
            second.cfg.broker = bus;
            assert!(second.attach_broker());
            assert!(second.sync_say_face());
            for want in &b {
                assert_eq!(fan_in_next_say(&mut second), *want);
            }
            assert_eq!(
                kept.delivery_offsets(),
                b,
                "each row once: reported={reported}"
            );
            assert_eq!(second.state.topics(SID)["t"].next, b[2] + 1);
            let floor = second.state.topic_floors(SID).unwrap().get("t").copied();
            assert_eq!(
                floor,
                (!reported).then_some(b[0]),
                "the floor goes once the watermark says the rows are handled"
            );
            second.close_subscriptions();
        }
    }

    #[test]
    fn the_unhandled_set_is_released_by_the_watermark_and_bounded_oldest_first() {
        const SID: &str = "s-a";
        let (_lane, mut bridge) = FakeVerbLane::bridge(&[(1, SID)]);
        let on_disk = |bridge: &Bridge| bridge.state.topic_floors(SID).unwrap();
        let floors = |pairs: &[(&str, u64)]| -> BTreeMap<String, u64> {
            pairs.iter().map(|(t, o)| ((*t).to_string(), *o)).collect()
        };
        bridge.note_broadcast_taken(SID, "a", 1, 100);
        bridge.note_broadcast_taken(SID, "b", 2, 101);
        bridge.note_broadcast_taken(SID, "a", 3, 102);
        assert_eq!(on_disk(&bridge), floors(&[("a", 100), ("b", 101)]));
        bridge.note_broadcast_taken(SID, "a", 1, 100);
        bridge.note_broadcast_taken(SID, "a", 0, 99);
        assert_eq!(
            bridge.per_sid.unhandled[SID]["a"].len(),
            2,
            "covered, and `OK 0`"
        );

        bridge.note_handled(SID, 1);
        assert_eq!(on_disk(&bridge), floors(&[("a", 102), ("b", 101)]));
        bridge.note_handled(SID, 0);
        bridge.note_broadcast_taken(SID, "a", 1, 100);
        assert_eq!(
            on_disk(&bridge),
            floors(&[("a", 102), ("b", 101)]),
            "handled ids"
        );

        bridge.forget_unhandled_topic(SID, "b");
        assert_eq!(on_disk(&bridge), floors(&[("a", 102)]));

        let keep = u64::try_from(UNHANDLED_KEEP).unwrap();
        for i in 0..keep {
            bridge.note_broadcast_taken(SID, "c", 10 + i, 1000 + i);
        }
        assert_eq!(
            on_disk(&bridge),
            floors(&[("c", 1000)]),
            "a's row was the session's oldest"
        );
        bridge.note_broadcast_taken(SID, "c", 10 + keep, 1000 + keep);
        assert_eq!(on_disk(&bridge), floors(&[("c", 1001)]));
        assert_eq!(bridge.per_sid.unhandled[SID]["c"].len(), UNHANDLED_KEEP);

        bridge.note_handled(SID, 10 + keep);
        assert!(on_disk(&bridge).is_empty());
        assert!(!bridge.per_sid.unhandled.contains_key(SID));
    }

    fn receipt_collision_with_unclearable_pin(restart: bool) {
        use std::os::unix::fs::PermissionsExt as _;

        struct RestorePermissions(std::path::PathBuf, std::fs::Permissions);
        impl Drop for RestorePermissions {
            fn drop(&mut self) {
                let _ = std::fs::set_permissions(&self.0, self.1.clone());
            }
        }

        let (lane, mut bridge) = FakeVerbLane::bridge(&[]);
        let state = bridge.state.root().to_path_buf();
        let socket = lane.dir.path().join("broker.sock");
        let broker = crate::test_broker(lane.dir.path().join("broker.log")).unwrap();
        let _server = broker.serve(&socket).unwrap();
        bridge.cfg.broker = socket.to_string_lossy().into_owned();
        bridge.conn = Some(bridge.connect().unwrap().0.into());
        let r = OwedReceipt {
            sid: "s-host".to_string(),
            rid: 1,
            off: 7,
            verdict: "handled".to_string(),
            kind: "task".to_string(),
            from: format!("s-sender@{}", bridge.node),
            earlier_log: false,
        };
        let seq = bridge.next_seq().unwrap();
        bridge.state.set_receipt_seq(&r.sid, r.rid, seq).unwrap();
        let acks = state.join("acks");
        let _restore = RestorePermissions(
            acks.clone(),
            std::fs::metadata(&acks).unwrap().permissions(),
        );
        std::fs::set_permissions(&acks, std::fs::Permissions::from_mode(0o500)).unwrap();
        let (mut other, _) = bridge.connect().unwrap();
        let (off, deduped) = other
            .publish(bridge.producer_id, seq, "/f/f/x/decoy", b"another writer")
            .unwrap();
        assert!(!deduped);
        assert_eq!(
            bridge.send_receipt(&r).unwrap_err().kind(),
            io::ErrorKind::AlreadyExists
        );
        assert_eq!(
            bridge.state.receipt_seq(&r.sid, r.rid),
            Some(seq),
            "the read-only directory prevented invalidation"
        );

        let reopened_lane;
        let endpoint = if restart {
            drop(bridge);
            (reopened_lane, bridge) = FakeVerbLane::bridge_with_state(&[], Some(&state));
            bridge.cfg.broker = socket.to_string_lossy().into_owned();
            &reopened_lane
        } else {
            &lane
        };
        bridge.conn = Some(bridge.connect().unwrap().0.into());
        let retry = bridge.send_receipt(&r);
        assert!(
            retry.is_err() && !bridge.self_acked.contains(&off) && endpoint.delivery_offsets().is_empty(),
            "a collided pin must stay owed, restart={restart}: retry={retry:?}, foreign_offset_remembered={}, retired={:?}",
            bridge.self_acked.contains(&off), endpoint.delivery_offsets()
        );
    }

    #[test]
    fn a_receipt_collision_cannot_reuse_a_pin_that_failed_to_clear() {
        for restart in [false, true] {
            receipt_collision_with_unclearable_pin(restart);
        }
    }

    #[test]
    fn a_pinned_lost_ack_retries_its_original_face_after_restart() {
        let (lane, mut bridge) = FakeVerbLane::bridge(&[]);
        let state = bridge.state.root().to_path_buf();
        let socket = lane.dir.path().join("broker.sock");
        let broker = crate::test_broker(lane.dir.path().join("broker.log")).unwrap();
        let _server = broker.serve(&socket).unwrap();
        bridge.cfg.broker = socket.to_string_lossy().into_owned();
        bridge.conn = Some(bridge.connect().unwrap().0.into());
        let original = format!("/f/f/in/p/h-a/{}/note", bridge.node);
        let moved = format!("/f/f/in/p/h-b/{}/note", bridge.node);
        let mut body = Body::new(1);
        body.from = Some("s-host".into());
        body.text = "lost ack".into();
        let pin = PublishPin {
            seq: bridge.next_seq().unwrap(),
            via_key: false,
            identity: Some(publication_identity(&original, &body.encode())),
            original: None,
            original_off: None,
            owner: None,
        };
        bridge.state.set_post_pin("s-host", 1, &pin).unwrap();
        let (off, deduped, _) = bridge
            .publish_reserved(&pin, &original, &body.encode())
            .unwrap();
        assert!(!deduped);
        drop(bridge);
        let (_second, mut bridge) = FakeVerbLane::bridge_with_state(&[], Some(&state));
        bridge.cfg.broker = socket.to_string_lossy().into_owned();
        bridge.conn = Some(bridge.connect().unwrap().0.into());
        body.t = 99;
        let pin = bridge.state.post_pin("s-host", 1).unwrap().unwrap();
        let (again, deduped, actual) = bridge
            .publish_reserved(&pin, &moved, &body.encode())
            .unwrap();
        assert_eq!(again, off);
        assert!(deduped);
        assert_eq!(actual, pin.identity);
        assert!(
            bridge
                .conn_now()
                .unwrap()
                .fetch(0, &moved, 8)
                .unwrap()
                .0
                .is_empty(),
            "route movement does not append elsewhere"
        );
        body.text = "incompatible replay".into();
        let refused = bridge.publish_reserved(&pin, &moved, &body.encode());
        assert!(
            refused.is_err_and(|e| e.kind() == io::ErrorKind::InvalidData)
                && bridge.collided.is_none(),
            "a payload the pin cannot reconstruct stays queued, not a collision"
        );
    }

    #[test]
    fn a_pinned_dedup_after_restart_requires_its_payload_and_legacy_ambiguity_stays_queued() {
        for modern in [false, true] {
            let (lane, mut bridge) = FakeVerbLane::bridge(&[]);
            let state = bridge.state.root().to_path_buf();
            let socket = lane.dir.path().join("broker.sock");
            let broker = crate::test_broker(lane.dir.path().join("broker.log")).unwrap();
            let _server = broker.serve(&socket).unwrap();
            bridge.cfg.broker = socket.to_string_lossy().into_owned();
            bridge.conn = Some(bridge.connect().unwrap().0.into());
            let face = format!("/f/f/in/p/h-a/{}/note", bridge.node);
            let mut intended = Body::new(1);
            intended.from = Some("s-host".into());
            intended.text = "the requested payload".into();
            let pin = PublishPin {
                seq: bridge.next_seq().unwrap(),
                via_key: !modern,
                identity: modern.then(|| publication_identity(&face, &intended.encode())),
                original: None,
                original_off: None,
                owner: None,
            };
            bridge.state.set_post_pin("s-host", 1, &pin).unwrap();
            let mut other_body = intended.clone();
            other_body.text = "a different original payload".into();
            let (mut other, _) = bridge.connect().unwrap();
            let (off, _) = other
                .publish(bridge.producer_id, pin.seq, &face, &other_body.encode())
                .unwrap();
            drop(bridge);
            let (_second, mut bridge) = FakeVerbLane::bridge_with_state(&[], Some(&state));
            bridge.cfg.broker = socket.to_string_lossy().into_owned();
            bridge.conn = Some(bridge.connect().unwrap().0.into());
            let pin = bridge.state.post_pin("s-host", 1).unwrap().unwrap();
            let error = bridge
                .publish_reserved(&pin, &face, &intended.encode())
                .unwrap_err();
            assert!(!bridge.self_acked.contains(&off));
            if modern {
                assert_eq!(error.kind(), io::ErrorKind::AlreadyExists);
                assert_eq!(bridge.collided, Some((pin.seq, off)));
            } else {
                assert_eq!(error.kind(), io::ErrorKind::InvalidData);
                assert!(
                    bridge.collided.is_none(),
                    "an older keyed body is not proof of a collision"
                );
                assert!(
                    bridge.state.post_pin("s-host", 1).unwrap().is_some(),
                    "an unverifiable reservation is not reseeded"
                );
            }
        }
    }

    #[test]
    fn a_legacy_keys_fresh_append_recovers_ownership_after_reconciliation_and_restart() {
        let (lane, mut bridge) = FakeVerbLane::bridge(&[(1, "s-host")]);
        let state = bridge.state.root().to_path_buf();
        let socket = lane.dir.path().join("broker.sock");
        let broker = crate::test_broker(lane.dir.path().join("broker.log")).unwrap();
        let _server = broker.serve(&socket).unwrap();
        bridge.cfg.broker = socket.to_string_lossy().into_owned();
        bridge.conn = Some(bridge.connect().unwrap().0.into());
        let face = format!("/f/f/in/p/h-a/{}/ask", bridge.node);
        let mut body = Body::new(1);
        body.from = Some("s-host".into());
        body.dl = Some(30_000);
        body.text = "new legacy attempt".into();
        let seq = bridge.next_seq().unwrap();
        bridge.state.set_key_seq("s-host", "k-legacy", seq).unwrap();
        let pin = PublishPin {
            seq,
            via_key: true,
            identity: Some(publication_identity(&face, &body.encode())),
            original: None,
            original_off: None,
            owner: None,
        };
        bridge.state.set_post_pin("s-host", 1, &pin).unwrap();
        let (off, deduped, actual) = bridge
            .publish_reserved(&pin, &face, &body.encode())
            .unwrap();
        assert!(!deduped, "the legacy winner was already retained away");
        bridge
            .state
            .reconcile_key(
                "s-host",
                "k-legacy",
                seq,
                actual.as_ref().unwrap(),
                off,
                Some(1),
            )
            .unwrap();
        bridge.remember_self_ack(off).unwrap();
        drop(bridge);
        let (second, mut bridge) = FakeVerbLane::bridge_with_state(&[(1, "s-host")], Some(&state));
        bridge.cfg.broker = socket.to_string_lossy().into_owned();
        bridge.conn = Some(bridge.connect().unwrap().0.into());
        *second.outbox.lock().unwrap() = Some(format!(
            "post sid=s-host id=1 to=h-a kind=ask key=k-legacy dl=30000 len={}\n{}",
            body.text.len(),
            body.text
        ));
        bridge.drain_outbox();
        assert_eq!(second.sent.lock().unwrap().len(), 1);
        assert!(!second.sent.lock().unwrap()[0].contains("dup=1"));
        assert_eq!(bridge.asked.get(&off).map(String::as_str), Some("h-a"));
        assert!(bridge.deadlines.contains_key(&off));
        assert_eq!(
            bridge
                .conn_now()
                .unwrap()
                .fetch(0, &face, 8)
                .unwrap()
                .0
                .len(),
            1
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_replayed_record_waits_for_its_posts_retirement_so_a_move_delivers_it_once() {
        for hold in [true, false] {
            let case = format!("hold={hold}");
            let sessions = [(1, "s-a"), (2, "s-b")];
            let (lane, mut first) = FakeVerbLane::bridge(&sessions);
            let state = first.state.root().to_path_buf();
            let (a, _broker_a, _server_a) = serve_log(&lane, "replay-a");
            first.cfg.broker = a.clone();
            assert!(first.attach_broker(), "{case}: boot's first attach");
            lane.refuse_retirements(true);
            let post = "post sid=s-a id=1 to=@s-b kind=note len=4\nonce".to_string();
            *lane.outbox.lock().unwrap() = Some(post.clone());
            first.drain_outbox();
            let pin = first
                .state
                .post_pin("s-a", 1)
                .unwrap()
                .expect("the post's pin stays with its retirement refused");
            let face = pin.identity.as_ref().unwrap().subject.clone();
            let (rows, _) = first.conn_now().unwrap().fetch(0, &face, 1).unwrap();
            let (on_a, subject, bytes) = rows.into_iter().next().expect("the note on A");
            first
                .state
                .set_in_flight(&InFlight {
                    pin: pin.clone(),
                    subject,
                    body: bytes,
                    log: first.conn.as_ref().and_then(|c| c.seen_log_identity()),
                })
                .unwrap();
            first.close_subscriptions();
            drop(first);

            let (second, mut bridge) = FakeVerbLane::bridge_with_state(&sessions, Some(&state));
            bridge.deliver_replay_at_once = !hold;
            bridge.cfg.broker = a;
            second.retire_from_outbox();
            *second.outbox.lock().unwrap() = Some(post.clone());
            bridge
                .state
                .refuse_writes_of(Some(&format!("inflight/{}", pin.seq)));
            assert_eq!(
                bridge.attach_broker(),
                !hold,
                "{case}: the attach on A, which meets the held record (hold) or delivers it"
            );
            assert_eq!(
                second.delivery_offsets().contains(&on_a),
                !hold,
                "{case}: the replayed note delivered on A before its post was retired"
            );
            bridge.close_subscriptions();
            bridge.conn = None;
            bridge.state.refuse_writes_of(None);

            let (b, _broker_b, _server_b) = serve_log(&second, "replay-b");
            bridge.cfg.broker = b;
            let (mut padding, _) = bridge.connect().unwrap();
            for seq in 1..=32 {
                padding.publish(92, seq, "/f/f/x/padding", b"v=1").unwrap();
            }
            drop(padding);
            assert!(bridge.attach_broker(), "{case}: the attach on B");
            bridge.drain_outbox();
            let (mut writer, _) = bridge.connect().unwrap();
            let sentinel_face = format!(
                "/f/{}/in/{}/s-b/h-writer/note",
                bridge.cfg.fleet, bridge.node
            );
            let (sentinel, _) = writer
                .publish(81, 1, &sentinel_face, b"v=1 t=1 text=sentinel")
                .unwrap();
            loop {
                match bridge.mailbox.take(aterm_spec::wait::MUST_HAPPEN) {
                    Some(Item::Inbox(rec)) => {
                        let at = rec.0;
                        bridge.on_inbox_record(&rec);
                        if at == sentinel {
                            break;
                        }
                    }
                    Some(_) => {}
                    None => panic!("{case}: the live tail ended before the sentinel"),
                }
            }
            assert!(
                bridge.state.in_flight().unwrap().is_empty(),
                "{case}: the entry settled on B"
            );
            let (notes, _) = bridge.conn_now().unwrap().fetch(0, &face, 8).unwrap();
            let on_b: Vec<u64> = notes.iter().map(|(off, _, _)| *off).collect();
            assert_eq!(on_b.len(), 1, "{case}: the note on B once: {on_b:?}");
            let reached: BTreeSet<String> = second
                .deliveries()
                .iter()
                .filter(|l| l.starts_with("deliver s-b ") && l.ends_with("text=once"))
                .filter_map(|l| l.split_whitespace().find(|w| w.starts_with("off=")))
                .map(str::to_string)
                .collect();
            let want: BTreeSet<String> = if hold {
                [format!("off={}", on_b[0])].into()
            } else {
                [format!("off={on_a}"), format!("off={}", on_b[0])].into()
            };
            assert_eq!(
                reached, want,
                "{case}: once from B (hold), or from A and again from B (the control)"
            );
            bridge.close_subscriptions();
        }
    }

    #[test]
    fn a_key_winner_republished_on_another_log_does_not_become_a_later_ask() {
        const SID: &str = "s-host";
        const KEY: &str = "k-log";
        let (lane, mut bridge) = FakeVerbLane::bridge(&[(1, SID)]);
        let (a, _broker_a, _server_a) = serve_log(&lane, "key-a");
        bridge.cfg.broker = a;
        assert!(bridge.attach_broker());

        *lane.outbox.lock().unwrap() = Some(format!(
            "post sid={SID} id=1 to=@{SID} kind=note key={KEY} len=8\noriginal"
        ));
        bridge.drain_outbox();
        assert_eq!(lane.sent.lock().unwrap().len(), 1);
        let winner = bridge.state.key_pin(SID, KEY).unwrap().unwrap();
        let old_off = winner.original_off.unwrap();
        let original = winner.identity.as_ref().unwrap();
        let (rows, _) = bridge
            .conn_now()
            .unwrap()
            .fetch(old_off, &original.subject, 1)
            .unwrap();
        let (at, face, bytes) = rows.into_iter().next().unwrap();
        assert_eq!(at, old_off);
        assert_eq!(winner.owner, Some(1));
        bridge
            .state
            .set_in_flight(&InFlight {
                pin: winner.clone(),
                subject: face,
                body: bytes,
                log: None,
            })
            .unwrap();

        let ask_face = format!("{}/ask", original.subject.strip_suffix("/note").unwrap());
        let mut ask = Body::new(0);
        ask.from = Some(SID.into());
        ask.dl = Some(30_000);
        ask.text = "later ask".into();
        bridge
            .state
            .set_post_pin(
                SID,
                2,
                &PublishPin {
                    seq: winner.seq,
                    via_key: true,
                    identity: Some(publication_identity(&ask_face, &ask.encode())),
                    original: winner.identity.clone(),
                    original_off: Some(old_off),
                    owner: winner.owner,
                },
            )
            .unwrap();
        *lane.outbox.lock().unwrap() = Some(format!(
            "post sid={SID} id=2 to=@{SID} kind=ask key={KEY} dl=30000 len=9\nlater ask"
        ));

        bridge.close_subscriptions();
        bridge.conn = None;
        let (b, _broker_b, _server_b) = serve_log(&lane, "key-b");
        bridge.cfg.broker = b;
        let (mut padding, _) = bridge.connect().unwrap();
        for seq in 1..=32 {
            padding.publish(92, seq, "/f/f/x/padding", b"v=1").unwrap();
        }
        drop(padding);
        assert!(bridge.attach_broker());
        assert!(bridge.state.in_flight().unwrap().is_empty());
        let (notes, _) = bridge
            .conn_now()
            .unwrap()
            .fetch(0, &original.subject, 8)
            .unwrap();
        assert_eq!(notes.len(), 1, "the journal recovered exactly one note");
        let landed = notes[0].0;
        assert_ne!(landed, old_off, "offsets belong to their broker log");
        assert_eq!(publication_identity(&notes[0].1, &notes[0].2), *original);

        assert_eq!(lane.sent.lock().unwrap().len(), 2);
        assert!(
            bridge.asked.is_empty() && bridge.deadlines.is_empty(),
            "a deduped note became a later ask at @{landed}: asked={:?}, deadlines={:?}",
            bridge.asked,
            bridge.deadlines
        );
        assert!(lane.sent.lock().unwrap()[1].contains(&format!("off={landed} dup=1")));
        let reconciled = bridge.state.key_pin(SID, KEY).unwrap().unwrap();
        assert_eq!(reconciled.seq, winner.seq);
        assert_eq!(reconciled.identity, winner.identity);
        assert_eq!(reconciled.owner, Some(1));
        assert_eq!(reconciled.original_off, Some(landed));
        assert!(bridge
            .conn_now()
            .unwrap()
            .fetch(0, &ask_face, 8)
            .unwrap()
            .0
            .is_empty());
    }

    #[test]
    fn keyed_reposts_keep_the_winner_and_reconcile_a_retained_away_winner_before_retirement() {
        use std::os::unix::fs::PermissionsExt as _;
        for same in [false, true] {
            let (initial_lane, mut bridge) = FakeVerbLane::bridge(&[(1, "s-host")]);
            let state = bridge.state.root().to_path_buf();
            let socket = initial_lane.dir.path().join("broker.sock");
            let broker = crate::test_broker(initial_lane.dir.path().join("broker.log")).unwrap();
            let _server = broker.serve(&socket).unwrap();
            bridge.cfg.broker = socket.to_string_lossy().into_owned();
            bridge.conn = Some(bridge.connect().unwrap().0.into());
            let recovered;
            let lane = if same {
                let mut original = Body::new(0);
                original.from = Some("s-host".into());
                original.dl = Some(30_000);
                original.text = "original".into();
                let face = format!("/f/f/in/p/h-a/{}/ask", bridge.node);
                let key = PublishPin {
                    seq: bridge.next_seq().unwrap(),
                    via_key: false,
                    identity: Some(publication_identity(&face, &original.encode())),
                    original: None,
                    original_off: None,
                    owner: Some(1),
                };
                bridge.state.set_key_pin("s-host", "k-first", &key).unwrap();
                drop(bridge);
                (recovered, bridge) =
                    FakeVerbLane::bridge_with_state(&[(1, "s-host")], Some(&state));
                bridge.cfg.broker = socket.to_string_lossy().into_owned();
                bridge.conn = Some(bridge.connect().unwrap().0.into());
                &recovered
            } else {
                &initial_lane
            };
            let queue = |lane: &FakeVerbLane, id: u64, to: &str, kind: &str, text: &str| {
                *lane.outbox.lock().unwrap() = Some(format!(
                "post sid=s-host id={id} to={to} kind={kind} key=k-first dl=30000 len={}\n{text}",
                text.len()
            ));
            };
            queue(
                lane,
                1,
                "h-a",
                if same { "ask" } else { "note" },
                "original",
            );
            bridge.drain_outbox();
            assert_eq!(lane.sent.lock().unwrap().len(), 1);
            let first = bridge.state.key_pin("s-host", "k-first").unwrap().unwrap();
            let first_off = lane.sent.lock().unwrap()[0]
                .split_whitespace()
                .find_map(|word| word.strip_prefix("off="))
                .unwrap()
                .parse::<u64>()
                .unwrap();
            queue(
                lane,
                2,
                "h-b",
                "ask",
                "different destination, kind and text",
            );
            bridge.drain_outbox();
            assert_eq!(lane.sent.lock().unwrap().len(), 2);
            assert!(lane.sent.lock().unwrap()[1].contains(&format!("off={first_off} dup=1")));
            assert_eq!(
                bridge
                    .state
                    .key_pin("s-host", "k-first")
                    .unwrap()
                    .unwrap()
                    .identity,
                first.identity
            );

            broker.retain_before_carrying_state(first_off + 1).unwrap();
            let keys = state.join("keys");
            let permissions = std::fs::metadata(&keys).unwrap().permissions();
            std::fs::set_permissions(&keys, std::fs::Permissions::from_mode(0o500)).unwrap();
            let (target, text) = if same {
                ("h-a", "original")
            } else {
                ("h-c", "after retention")
            };
            queue(lane, 3, target, "ask", text);
            bridge.drain_outbox();
            std::fs::set_permissions(&keys, permissions).unwrap();
            assert_eq!(
                lane.sent.lock().unwrap().len(),
                2,
                "no retirement before durable reconciliation"
            );
            let pending = bridge.state.post_pin("s-host", 3).unwrap().unwrap();
            assert_eq!(pending.identity == pending.original, same);
            assert_eq!(pending.original_off, Some(first_off));
            drop(bridge);

            let (second, mut bridge) =
                FakeVerbLane::bridge_with_state(&[(1, "s-host")], Some(&state));
            bridge.cfg.broker = socket.to_string_lossy().into_owned();
            bridge.conn = Some(bridge.connect().unwrap().0.into());
            queue(&second, 3, target, "ask", text);
            bridge.drain_outbox();
            assert_eq!(second.sent.lock().unwrap().len(), 1);
            let landed = second.sent.lock().unwrap()[0]
                .split_whitespace()
                .find_map(|word| word.strip_prefix("off="))
                .unwrap()
                .parse::<u64>()
                .unwrap();
            assert_ne!(landed, first_off);
            assert!(
                !second.sent.lock().unwrap()[0].contains("dup=1"),
                "this post's own crash retry"
            );
            assert_eq!(bridge.asked.get(&landed).map(String::as_str), Some(target));
            assert!(
                bridge.deadlines.contains_key(&landed),
                "the appended ask still expires, same={same}"
            );
            assert_eq!(
                bridge
                    .state
                    .key_pin("s-host", "k-first")
                    .unwrap()
                    .unwrap()
                    .identity,
                pending.identity
            );
            let face = &pending.identity.unwrap().subject;
            assert_eq!(
                bridge
                    .conn_now()
                    .unwrap()
                    .fetch(first_off + 1, face, 8)
                    .unwrap()
                    .0
                    .len(),
                1,
                "lost reconciliation creates no duplicate"
            );
        }
    }

    #[test]
    fn a_broadcast_with_no_topics_advances_the_head_without_decoding_its_body() {
        let (lane, mut bridge) = FakeVerbLane::bridge(&[(1, "s-a")]);
        let subject = "/f/f/pub/n-src/s-src/say/quiet".to_string();
        bridge.say_drained_to = 5;
        bridge.say_bus_head = 5;
        let decoded = crate::body::decode_calls();

        bridge.on_say_record(&(5, subject.clone(), vec![b'x'; 1024 * 1024]));
        assert_eq!(crate::body::decode_calls(), decoded);
        assert_eq!((bridge.say_drained_to, bridge.say_bus_head), (6, 6));
        assert_eq!(bridge.say_checkpoint_tick, 1);
        assert!(lane.delivery_offsets().is_empty());

        bridge.per_sid.topics.insert(
            "s-a".to_string(),
            BTreeMap::from([("quiet".to_string(), TopicCursor { next: 6 })]),
        );
        bridge.on_say_record(&(6, subject, Body::new(0).encode()));
        assert_eq!(crate::body::decode_calls(), decoded + 1);
        assert_eq!(lane.delivery_offsets(), [6]);
        assert_eq!(bridge.per_sid.topics["s-a"]["quiet"].next, 7);
    }

    #[test]
    fn a_refused_broadcast_is_read_past_by_every_cursor_at_the_frontier() {
        let (lane, mut bridge) = FakeVerbLane::bridge(&[(1, "s-a"), (2, "s-b")]);
        bridge.say_drained_to = 5;
        bridge.say_bus_head = 5;
        for sid in ["s-a", "s-b"] {
            bridge.per_sid.topics.insert(
                sid.to_string(),
                BTreeMap::from([("news".to_string(), TopicCursor { next: 5 })]),
            );
        }
        let mut relayed = Body::new(0);
        relayed.via = Some("notaprincipal".to_string());
        let refused = [
            (5, "/f/f/pub/n-x/s-1/say/news/x", Body::new(0)),
            (6, "/f/f/pub/n-x/s-1/say/NEWS", Body::new(0)),
            (7, "/f/f/pub/n-x/s-1/say/news", relayed),
        ];
        for (off, subject, body) in refused {
            bridge.on_say_record(&(off, subject.to_string(), body.encode()));
            for sid in ["s-a", "s-b"] {
                assert_eq!(
                    bridge.per_sid.topics[sid]["news"].next,
                    off + 1,
                    "{sid} was left behind by {subject}"
                );
            }
        }
        assert!(lane.delivery_offsets().is_empty());

        let live = "/f/f/pub/n-x/s-1/say/news".to_string();
        bridge.on_say_record(&(8, live, Body::new(0).encode()));
        assert_eq!(lane.transcript(), ["deliver 8", "deliver 8"]);
        for sid in ["s-a", "s-b"] {
            assert_eq!(bridge.per_sid.topics[sid]["news"].next, 9);
        }
    }

    #[cfg(unix)]
    #[test]
    fn a_catch_up_page_walks_one_topic_past_every_other_topics_traffic() {
        const SID: &str = "s-late";
        let (lane, mut bridge) = FakeVerbLane::bridge(&[(1, SID)]);
        let socket = lane.dir.path().join("broker.sock");
        let broker = crate::test_broker(lane.dir.path().join("broker.log")).unwrap();
        let _server = broker.serve(&socket).unwrap();
        bridge.cfg.broker = socket.to_string_lossy().into_owned();
        let (conn, _) = bridge.connect().unwrap();
        bridge.conn = Some(conn.into());

        let (mut god, _) = bridge.connect().unwrap();
        let note = Body::new(0).encode();
        let mut publish = |producer: u64, topic: &str, n: usize| -> Vec<u64> {
            let subject = format!("/f/f/pub/n-x/s-p{producer}/say/{topic}");
            let bodies = vec![note.as_slice(); n];
            god.publish_pipelined(producer, &subject, &bodies, 64)
                .unwrap()
                .into_iter()
                .map(|(off, deduped)| {
                    assert!(!deduped);
                    off
                })
                .collect()
        };
        let noise = publish(1, "noise", 300);
        let a1 = publish(2, "alpha", 1)[0];
        let beta = publish(3, "beta", 300);
        let a2 = publish(4, "alpha", 1)[0];
        let drained = a2 + 1;
        bridge.say_drained_to = drained;
        bridge.say_bus_head = drained;
        bridge.per_sid.topics.insert(
            SID.to_string(),
            BTreeMap::from([
                ("alpha".to_string(), TopicCursor { next: noise[0] }),
                ("beta".to_string(), TopicCursor { next: beta[0] }),
            ]),
        );
        let delivered = |offs: &[u64]| -> Vec<String> {
            offs.iter().map(|off| format!("deliver {off}")).collect()
        };

        assert!(bridge.catch_up_topic_page(SID));
        assert_eq!(lane.transcript(), delivered(&[a1, a2]));
        assert_eq!(bridge.per_sid.topics[SID]["alpha"].next, drained);
        assert_eq!(bridge.per_sid.topics[SID]["beta"].next, beta[0]);

        assert!(bridge.catch_up_topic_page(SID));
        assert_eq!(bridge.per_sid.topics[SID]["beta"].next, beta[255] + 1);
        assert!(bridge.catch_up_topic_page(SID));
        assert_eq!(bridge.per_sid.topics[SID]["beta"].next, drained);
        let mut want = vec![a1, a2];
        want.extend(&beta);
        assert_eq!(lane.transcript(), delivered(&want));
        assert_eq!(bridge.state.topics(SID), bridge.per_sid.topics[SID]);

        assert!(bridge.catch_up_topic_page(SID));
        assert_eq!(lane.transcript().len(), want.len());
    }

    #[cfg(unix)]
    #[test]
    fn a_live_record_does_not_cancel_a_backlog_still_owed() {
        const SID: &str = "s-late";
        const PAGE: usize = 8;
        let (lane, mut bridge) = FakeVerbLane::bridge(&[(1, SID)]);
        bridge.say_replay.page = u32::try_from(PAGE).expect("a small page");
        let socket = lane.dir.path().join("broker.sock");
        let broker = crate::test_broker(lane.dir.path().join("broker.log")).unwrap();
        let _server = broker.serve(&socket).unwrap();
        bridge.cfg.broker = socket.to_string_lossy().into_owned();
        let (conn, _) = bridge.connect().unwrap();
        bridge.conn = Some(conn.into());

        let (mut god, _) = bridge.connect().unwrap();
        let note = Body::new(0).encode();
        let old = "/f/f/pub/n-x/s-old/say/deep";
        let bodies = vec![note.as_slice(); SAY_REPLAY_PAGES * PAGE + 1];
        let backlog: Vec<u64> = god
            .publish_pipelined(1, old, &bodies, 64)
            .unwrap()
            .into_iter()
            .map(|(off, deduped)| {
                assert!(!deduped);
                off
            })
            .collect();
        let drained = backlog[backlog.len() - 1] + 1;
        bridge.say_drained_to = drained;
        bridge.say_bus_head = drained;
        bridge.per_sid.topics.insert(
            SID.to_string(),
            BTreeMap::from([("deep".to_string(), TopicCursor { next: backlog[0] })]),
        );
        let shout = "/f/f/pub/n-y/s-live/say/deep";
        let delivered = |offs: &[u64]| -> Vec<String> {
            offs.iter().map(|off| format!("deliver {off}")).collect()
        };
        let round = |bridge: &mut Bridge| {
            bridge.say_replay.refill_at = Instant::now();
            bridge.replay_topics(true);
        };

        let (live, _) = god.publish(2, 1, shout, &note).unwrap();
        bridge.on_say_record(&(live, shout.to_string(), note.clone()));
        assert!(
            lane.transcript().is_empty(),
            "a behind cursor is no live recipient"
        );
        assert_eq!(
            bridge.per_sid.topics[SID]["deep"].next, backlog[0],
            "the live record moved a cursor that owes the backlog"
        );
        assert_eq!(bridge.say_drained_to, live + 1);

        round(&mut bridge);
        let first = SAY_REPLAY_PAGES * PAGE;
        assert_eq!(lane.transcript(), delivered(&backlog[..first]));
        assert_eq!(
            bridge.per_sid.topics[SID]["deep"].next,
            backlog[first - 1] + 1
        );

        let (later, _) = god.publish(2, 2, shout, &note).unwrap();
        bridge.on_say_record(&(later, shout.to_string(), note.clone()));
        assert_eq!(lane.transcript().len(), first);
        assert_eq!(
            bridge.per_sid.topics[SID]["deep"].next,
            backlog[first - 1] + 1
        );

        round(&mut bridge);
        let mut want = backlog.clone();
        want.extend([live, later]);
        assert_eq!(lane.transcript(), delivered(&want));
        assert_eq!(bridge.per_sid.topics[SID]["deep"].next, later + 1);
        assert_eq!(bridge.state.topics(SID), bridge.per_sid.topics[SID]);

        let (next, _) = god.publish(2, 3, shout, &note).unwrap();
        bridge.on_say_record(&(next, shout.to_string(), note.clone()));
        want.push(next);
        assert_eq!(lane.transcript(), delivered(&want));
        assert_eq!(bridge.per_sid.topics[SID]["deep"].next, next + 1);
    }

    #[test]
    fn a_remote_node_cannot_speak_for_a_local_session_or_answer_anothers_ask() {
        let (_lane, mut bridge) = FakeVerbLane::bridge(&[(1, "s-mgr")]);
        bridge
            .per_sid
            .epochs
            .insert("s-w".to_string(), "e".to_string());
        let local = bridge.node.clone();
        let addr = |src: &str, kind: &str| subject::InAddr {
            node: local.clone(),
            sid: "s-mgr".to_string(),
            src: src.to_string(),
            kind: kind.to_string(),
        };
        let mut body = Body::new(0);
        body.from = Some("s-w".to_string());
        assert_eq!(
            bridge.render_from(&addr(&local, "report"), &body),
            format!("s-w@{local}")
        );
        assert_eq!(
            bridge.render_from(&addr("n-evil", "report"), &body),
            "n-evil"
        );
        body.from = Some("s-far".to_string());
        assert_eq!(
            bridge.render_from(&addr("n-far", "report"), &body),
            "s-far@n-far"
        );
        bridge.state.pin("s-far", "n-far").expect("pin");
        assert_eq!(
            bridge.render_from(&addr("n-far", "report"), &body),
            "s-far@n-far"
        );
        assert_eq!(
            bridge.render_from(&addr("n-evil", "report"), &body),
            "n-evil"
        );

        bridge.asked.insert(91, local.clone());
        let mut reply = Body::new(0);
        reply.re = Some(91);
        for kind in ["answer", "report", "ack"] {
            assert_eq!(
                bridge.classify_delivery(&addr("n-evil", kind), &reply),
                ("note".to_string(), Some(kind.to_string()), true),
                "a stray {kind}"
            );
            assert_eq!(
                bridge.classify_delivery(&addr(&local, kind), &reply),
                (kind.to_string(), None, false),
                "the recipient's {kind}"
            );
        }
        assert_eq!(
            bridge.classify_delivery(&addr("n-evil", "expired"), &reply),
            ("note".to_string(), Some("expired".to_string()), true),
            "a third node's expired"
        );
        assert_eq!(
            bridge.classify_delivery(&addr(&local, "expired"), &reply),
            ("expired".to_string(), None, false),
            "this node's own expired"
        );
    }

    #[cfg(unix)]
    fn seeded_broker(
        lane: &FakeVerbLane,
        bridge: &mut Bridge,
        sids: &[&str],
        known: &[String],
        held: u64,
    ) -> (astream_broker::Broker, astream_broker::BrokerHandle) {
        let path = lane.dir.path().join("budget.log");
        {
            let mut log =
                astream_broker::BrokerLog::open_with(&path, astream_broker::Durability::Relaxed)
                    .unwrap();
            let mut subjects: Vec<String> = ["presence", "ev", "ack"]
                .iter()
                .map(|face| subject::node_face("f", &bridge.node, face))
                .collect();
            for sid in sids {
                for face in ["presence", "ev"] {
                    subjects.push(format!("/f/f/pub/{}/{sid}/{face}", bridge.node));
                }
            }
            subjects.extend(known.iter().cloned());
            let mut filler = 0;
            while (subjects.len() as u64) < held {
                subjects.push(format!("/f/f/pub/{}/node/filler/{filler}", bridge.node));
                filler += 1;
            }
            for (seq, s) in subjects.into_iter().enumerate() {
                log.publish(bridge.producer_id, seq as u64 + 1, s, b"v=1".to_vec())
                    .unwrap();
            }
        }
        let give_up = std::time::Instant::now() + aterm_spec::wait::MUST_HAPPEN;
        let broker = loop {
            match crate::test_broker(&path) {
                Ok(broker) => break broker,
                Err(e)
                    if e.kind() == std::io::ErrorKind::AlreadyExists
                        && std::time::Instant::now() < give_up =>
                {
                    std::thread::sleep(std::time::Duration::from_millis(10));
                }
                Err(e) => panic!("the seeded log did not open: {e}"),
            }
        };
        let socket = lane.dir.path().join("budget.sock");
        let server = broker.serve(&socket).unwrap();
        bridge.cfg.broker = socket.to_string_lossy().into_owned();
        assert!(
            bridge.attach_broker(),
            "attached without spending a subject"
        );
        (broker, server)
    }

    #[cfg(unix)]
    #[test]
    fn a_new_lane_past_the_subject_budget_is_retired_and_the_queue_behind_it_lands() {
        let (lane, mut bridge) = FakeVerbLane::bridge(&[(1, "s-a"), (2, "s-b")]);
        assert!(bridge.refresh_sessions().is_ok());
        let task_lane = format!("/f/f/in/{0}/s-b/{0}/task", bridge.node);
        let ask_lane = format!("/f/f/in/{0}/s-b/{0}/ask", bridge.node);
        let max = astream_broker::store::MAX_SUBJECTS_PER_PRODUCER as u64;
        let (_broker, _server) =
            seeded_broker(&lane, &mut bridge, &["s-a", "s-b"], &[task_lane], max);
        bridge.indexed_subjects.insert(ask_lane.clone());
        *lane.outbox.lock().unwrap() = Some(
            "post sid=s-a id=1 to=@s-b kind=note len=5\nfirst\
             post sid=s-a id=2 to=@s-b kind=ask len=5\nstale\
             post sid=s-a id=3 to=@s-b kind=task len=6\nsecond\
             receipt sid=s-b rid=1 off=3 verdict=handled kind=ask from=s-a\n"
                .to_string(),
        );
        bridge.drain_outbox();
        let sent = lane.sent.lock().unwrap().clone();
        for refused in [1, 2] {
            assert!(
                sent.contains(&format!(
                    "outbox sent s-a {refused} off=- reason=subject-budget"
                )),
                "post {refused}: {sent:#?}"
            );
        }
        assert!(
            !bridge.indexed_subjects.contains(&ask_lane),
            "a subject the broker refused as new is not believed indexed"
        );
        assert!(
            sent.iter()
                .any(|l| l.starts_with("outbox sent s-a 3 off=") && !l.contains("off=-")),
            "the task behind the refused posts did not land: {sent:#?}"
        );
        let (mut reader, _) = bridge.connect().unwrap();
        let ev_of = |reader: &mut Conn, face: String| -> Vec<String> {
            let (rows, _) = reader.fetch(0, &face, 4096).unwrap();
            rows.iter()
                .map(|(_, _, b)| crate::pct::decode(&String::from_utf8_lossy(b)))
                .collect()
        };
        let node_ev = ev_of(&mut reader, subject::node_face("f", &bridge.node, "ev"));
        assert!(
            node_ev
                .iter()
                .any(|e| e.contains("ev=undeliverable to=@s-b reason=subject-budget")),
            "{node_ev:#?}"
        );
        let b_ev = ev_of(&mut reader, format!("/f/f/pub/{}/s-b/ev", bridge.node));
        assert!(
            b_ev.iter()
                .any(|e| e.contains("unacked re=3 verdict=handled reason=subject-budget")),
            "{b_ev:#?}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_broadcast_opens_a_subject_only_above_the_reserve() {
        let (lane, mut bridge) = FakeVerbLane::bridge(&[(1, "s-a"), (2, "s-b")]);
        assert!(bridge.refresh_sessions().is_ok());
        let known = format!("/f/f/pub/{}/s-a/say/known", bridge.node);
        let max = astream_broker::store::MAX_SUBJECTS_PER_PRODUCER as u64;
        let held = max - max / SAY_RESERVE_DIVISOR;
        let (_broker, _server) = seeded_broker(
            &lane,
            &mut bridge,
            &["s-a", "s-b"],
            std::slice::from_ref(&known),
            held,
        );
        *lane.outbox.lock().unwrap() = Some(
            "post sid=s-a id=1 to=say:fresh kind=note len=3\nnew\
             post sid=s-a id=2 to=say:known kind=note len=3\nold\
             post sid=s-b id=1 to=@s-a kind=note len=4\nmail"
                .to_string(),
        );
        bridge.drain_outbox();
        let sent = lane.sent.lock().unwrap().clone();
        assert!(
            sent.contains(&"outbox sent s-a 1 off=- reason=subject-budget".to_string()),
            "{sent:#?}"
        );
        for landed in ["outbox sent s-a 2 off=", "outbox sent s-b 1 off="] {
            assert!(
                sent.iter()
                    .any(|l| l.starts_with(landed) && !l.contains("off=-")),
                "{landed}: {sent:#?}"
            );
        }
        let (mut reader, _) = bridge.connect().unwrap();
        let fresh = format!("/f/f/pub/{}/s-a/say/fresh", bridge.node);
        assert!(
            reader.fetch(0, &fresh, 8).unwrap().0.is_empty(),
            "nothing spent"
        );
        let budget = reader
            .subject_budget(bridge.producer_id)
            .unwrap()
            .expect("a broker that reads budgets");
        assert_eq!(budget.subjects, held + 1, "only the directed lane opened");
    }

    #[cfg(unix)]
    #[test]
    fn a_new_post_id_under_a_winning_key_dedups_before_the_subject_budget_door() {
        let (lane, mut bridge) = FakeVerbLane::bridge(&[(1, "s-a"), (2, "s-b")]);
        bridge.refresh_sessions().unwrap();
        let note = format!("/f/f/in/{0}/s-b/{0}/note", bridge.node);
        let ask = format!("/f/f/in/{0}/s-b/{0}/ask", bridge.node);
        let max = astream_broker::store::MAX_SUBJECTS_PER_PRODUCER as u64;
        let (_broker, _server) = seeded_broker(
            &lane,
            &mut bridge,
            &["s-a", "s-b"],
            std::slice::from_ref(&note),
            max,
        );
        *lane.outbox.lock().unwrap() =
            Some("post sid=s-a id=1 to=@s-b kind=note key=k-first len=8\noriginal".into());
        bridge.drain_outbox();
        let winner = bridge.state.key_pin("s-a", "k-first").unwrap().unwrap();
        let off = winner.original_off.unwrap();
        assert!(lane
            .sent
            .lock()
            .unwrap()
            .contains(&format!("outbox sent s-a 1 off={off}")));

        *lane.outbox.lock().unwrap() = Some(
            "post sid=s-a id=2 to=@s-b kind=ask key=k-first dl=30000 len=5\nlater\
             post sid=s-a id=3 to=@s-b kind=ask len=5\nfresh"
                .into(),
        );
        bridge.drain_outbox();
        let sent = lane.sent.lock().unwrap().clone();
        assert!(
            sent.contains(&format!("outbox sent s-a 2 off={off} dup=1")),
            "the winning key was judged as a new subject: {sent:#?}"
        );
        assert!(sent.contains(&"outbox sent s-a 3 off=- reason=subject-budget".into()));
        assert!(bridge.asked.is_empty() && bridge.deadlines.is_empty());
        let kept = bridge.state.key_pin("s-a", "k-first").unwrap().unwrap();
        assert_eq!(
            (kept.seq, kept.owner, kept.original_off),
            (winner.seq, Some(1), Some(off))
        );
        let (mut reader, _) = bridge.connect().unwrap();
        assert!(reader.fetch(0, &ask, 8).unwrap().0.is_empty());
        let notes = reader.fetch(0, &note, 8).unwrap().0;
        assert_eq!(
            notes.len(),
            2,
            "one seeded row and the original, no duplicate"
        );
        assert_eq!(notes.last().unwrap().0, off);
        assert_eq!(
            reader
                .subject_budget(bridge.producer_id)
                .unwrap()
                .unwrap()
                .subjects,
            max
        );
    }

    #[cfg(unix)]
    #[test]
    fn unknown_post_or_key_reservation_state_cannot_retire_at_the_budget_door() {
        for (blocked, dangling) in [
            (crate::state::journal::FILE, false),
            ("keys/s-a", false),
            (crate::state::journal::FILE, true),
            ("keys/s-a", true),
        ] {
            let (lane, mut bridge) = FakeVerbLane::bridge(&[(1, "s-a"), (2, "s-b")]);
            bridge.refresh_sessions().unwrap();
            let max = astream_broker::store::MAX_SUBJECTS_PER_PRODUCER as u64;
            let (_broker, _server) = seeded_broker(&lane, &mut bridge, &["s-a", "s-b"], &[], max);
            let root = bridge.state.root().to_path_buf();
            let path = root.join(blocked);
            std::fs::create_dir_all(path.parent().expect("reservation parent")).unwrap();
            let _ = std::fs::remove_file(&path);
            if dangling {
                std::os::unix::fs::symlink("absent-reservation-target", &path)
                    .expect("make the existing reservation target unreadable");
            } else {
                std::fs::create_dir(&path).expect("make the required reservation read fail");
            }
            bridge.state = StateDir::open(&root).unwrap();
            *lane.outbox.lock().unwrap() =
                Some("post sid=s-a id=1 to=@s-b kind=ask key=k-first len=5\nfresh".into());
            bridge.drain_outbox();
            let retired = || {
                lane.sent
                    .lock()
                    .unwrap()
                    .iter()
                    .any(|s| s.starts_with("outbox sent s-a 1 "))
            };
            assert!(
                !retired(),
                "unreadable {blocked} authorized a terminal retirement"
            );
            let metadata = std::fs::symlink_metadata(&path).expect("preserved witness");
            assert_eq!(metadata.file_type().is_symlink(), dangling);

            if dangling {
                std::fs::remove_file(&path).unwrap();
            } else {
                std::fs::remove_dir(&path).unwrap();
            }
            bridge.drain_outbox();
            assert!(lane
                .sent
                .lock()
                .unwrap()
                .contains(&"outbox sent s-a 1 off=- reason=subject-budget".into()));
            let (mut reader, _) = bridge.connect().unwrap();
            let ask = format!("/f/f/in/{0}/s-b/{0}/ask", bridge.node);
            assert!(reader.fetch(0, &ask, 8).unwrap().0.is_empty());
        }
    }

    #[cfg(unix)]
    #[test]
    fn directed_mail_opens_a_lane_only_to_a_node_the_fleet_has_shown_and_above_its_reserve() {
        let (lane, mut bridge) = FakeVerbLane::bridge(&[(1, "s-a")]);
        assert!(bridge.refresh_sessions().is_ok());
        let node = bridge.node.clone();
        let far = subject::node_face("f", "n-far", "presence");
        let known = format!("/f/f/in/n-far/s-x/{node}/note");
        let max = astream_broker::store::MAX_SUBJECTS_PER_PRODUCER as u64;
        let held = max - max / LANE_RESERVE_DIVISOR - 1;
        let (_broker, _server) = seeded_broker(&lane, &mut bridge, &["s-a"], &[far, known], held);
        // n-far advertises the sessions mailed there; its rows are its producer's, not this node's.
        let (mut far_node, _) = bridge.connect().unwrap();
        for (seq, sid) in (1..).zip(["s-x", "s-y", "s-z"]) {
            far_node
                .publish(
                    9_000,
                    seq,
                    &subject::session_face("f", "n-far", sid, "presence"),
                    b"v=1 t=1 inc=1 state=live",
                )
                .unwrap();
        }
        *lane.outbox.lock().unwrap() = Some(
            "post sid=s-a id=1 to=@s-x@n-nowhere kind=note len=1\n1\
             post sid=s-a id=2 to=@s-y@n-far kind=note len=1\n2\
             post sid=s-a id=3 to=@s-z@n-far kind=note len=1\n3\
             post sid=s-a id=4 to=h-made-up kind=note len=1\n4\
             post sid=s-a id=5 to=@s-x@n-far kind=note len=1\n5"
                .to_string(),
        );
        bridge.drain_outbox();
        let sent = lane.sent.lock().unwrap().clone();
        let retired = |id: u64, why: &str| {
            assert!(
                sent.contains(&format!("outbox sent s-a {id} off=- reason={why}")),
                "post {id} is retired {why}: {sent:#?}"
            );
        };
        let landed = |id: u64| {
            assert!(
                sent.iter()
                    .any(|l| l.starts_with(&format!("outbox sent s-a {id} off="))
                        && !l.contains("off=-")),
                "post {id} landed: {sent:#?}"
            );
        };
        retired(1, "unroutable");
        landed(2);
        retired(3, SUBJECT_BUDGET);
        retired(4, SUBJECT_BUDGET);
        landed(5);
        let (mut reader, _) = bridge.connect().unwrap();
        for unminted in [
            format!("/f/f/in/n-nowhere/s-x/{node}/note"),
            format!("/f/f/in/n-far/s-z/{node}/note"),
            format!("/f/f/in/p/h-made-up/{node}/note"),
        ] {
            assert!(
                reader.fetch(0, &unminted, 8).unwrap().0.is_empty(),
                "{unminted} was minted"
            );
        }
        let node_ev: Vec<String> = reader
            .fetch(0, &subject::node_face("f", &node, "ev"), 4096)
            .unwrap()
            .0
            .iter()
            .map(|(_, _, b)| crate::pct::decode(&String::from_utf8_lossy(b)))
            .collect();
        assert!(
            node_ev
                .iter()
                .any(|e| e.contains("ev=undeliverable to=@s-x@n-nowhere reason=unroutable")),
            "the refusal is said where the node's operator reads: {node_ev:#?}"
        );
        let budget = reader
            .subject_budget(bridge.producer_id)
            .unwrap()
            .expect("a broker that reads budgets");
        assert_eq!(budget.subjects, held + 1, "only @s-y@n-far's lane opened");
        assert_eq!(
            budget.remaining(),
            max / LANE_RESERVE_DIVISOR,
            "the faces' reserve is whole"
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_strangers_mail_spends_a_bounded_share_of_this_nodes_budget() {
        const SID: &str = "s-host";
        let (lane, mut bridge) = FakeVerbLane::bridge(&[(1, SID)]);
        let socket = lane.dir.path().join("broker.sock");
        let broker = crate::test_broker(lane.dir.path().join("broker.log")).unwrap();
        let _server = broker.serve(&socket).unwrap();
        bridge.cfg.broker = socket.to_string_lossy().into_owned();
        let (conn, _) = bridge.connect().unwrap();
        bridge.conn = Some(conn.into());
        assert!(bridge.refresh_sessions().is_ok());
        let node = bridge.node.clone();

        for (off, ghost) in [(10, "s-ghost1"), (11, "s-ghost2")] {
            let subject = format!("/f/f/in/{node}/{ghost}/n-evil/note");
            let outcome = bridge.deliver_record(off, &subject, b"v=1 t=1 text=x");
            assert_eq!(outcome, Delivery::Accounted);
        }
        let known = format!("/f/f/in/n-evil/s-known/{node}");
        bridge
            .publish(&format!("{known}/note"), b"v=1 t=1 text=hi")
            .unwrap();
        let refuse = |bridge: &mut Bridge, off: u64, from: &str| {
            let subject = format!("/f/f/in/{node}/{SID}/n-evil/note");
            let raw = format!("v=1 t=1 from={from} via=NOT-A-PRINCIPAL text=x");
            let outcome = bridge.deliver_record(off, &subject, raw.as_bytes());
            assert_eq!(outcome, Delivery::Accounted);
        };
        refuse(&mut bridge, 20, "s-first");
        for i in 1..NEW_LANES_PER_NODE {
            let fill = format!("/f/f/in/n-evil/s-fill{i}/{node}");
            assert!(bridge.may_answer_on("n-evil", &fill).unwrap(), "{fill}");
        }
        refuse(&mut bridge, 21, "s-x");
        refuse(&mut bridge, 22, "s-known");
        for (rid, from) in [(1, "s-y@n-evil"), (2, "s-first@n-evil"), (3, "s-z@n-other")] {
            let owed = OwedReceipt {
                sid: SID.to_string(),
                rid,
                off: 30 + rid,
                verdict: "handled".to_string(),
                kind: "ask".to_string(),
                from: from.to_string(),
                earlier_log: false,
            };
            bridge.send_receipt(&owed).unwrap();
        }
        for i in 0..=NEW_LANES_PER_NODE {
            let mine = format!("/f/f/in/{node}/s-local{i}/{node}");
            assert!(bridge.may_answer_on(&node, &mine).unwrap(), "{mine}");
        }

        let (mut reader, _) = bridge.connect().unwrap();
        let (rows, _) = reader.last_all("/f/f/>").unwrap();
        let subjects: Vec<&str> = rows.iter().map(|(_, s, _)| s.as_str()).collect();
        assert!(
            !subjects.iter().any(|s| s.contains("s-ghost")),
            "a sid this node does not host became a subject: {subjects:#?}"
        );
        let lanes_on = |peer: &str| -> BTreeSet<&str> {
            let prefix = format!("/f/f/in/{peer}/");
            subjects
                .iter()
                .copied()
                .filter(|s| s.starts_with(&prefix))
                .collect()
        };
        let first = format!("/f/f/in/n-evil/s-first/{node}");
        assert_eq!(
            lanes_on("n-evil"),
            BTreeSet::from([
                format!("{first}/undeliverable").as_str(),
                format!("{first}/ack").as_str(),
                format!("{known}/note").as_str(),
                format!("{known}/undeliverable").as_str(),
            ]),
            "the first contact and the held lane, and no lane past the bound"
        );
        assert_eq!(
            lanes_on("n-other"),
            BTreeSet::from([format!("/f/f/in/n-other/s-z/{node}/ack").as_str()]),
            "another node's first contact draws on its own allowance"
        );

        let ev_of = |face: &str, reader: &mut Conn| -> Vec<String> {
            let (rows, _) = reader.fetch(0, face, 256).unwrap();
            rows.iter()
                .filter(|(_, s, _)| s == face)
                .filter_map(|(_, _, raw)| {
                    let ev = Body::decode(raw).0.unknown.get("ev")?.clone();
                    Some(crate::pct::decode(&ev))
                })
                .collect()
        };
        let node_ev = ev_of(&subject::node_face("f", &node, "ev"), &mut reader);
        for (off, ghost) in [(10, "s-ghost1"), (11, "s-ghost2")] {
            let want = format!("undeliverable off={off} reason=not-hosted sid={ghost}");
            assert!(node_ev.contains(&want), "{want} in {node_ev:#?}");
        }
        let host_ev = ev_of(&subject::session_face("f", &node, SID, "ev"), &mut reader);
        for want in [
            "unnotified off=21 src=n-evil reason=lane-cap",
            "unacked re=31 verdict=handled reason=lane-cap",
        ] {
            assert!(
                host_ev.iter().any(|e| e == want),
                "past the bound the answer is recorded unsent: {want} in {host_ev:#?}"
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn a_failed_lane_read_keeps_the_receipt_owed_and_sends_no_verdict() {
        const SID: &str = "s-host";
        let (lane, mut bridge) = FakeVerbLane::bridge(&[(1, SID)]);
        let socket = lane.dir.path().join("broker.sock");
        let broker = crate::test_broker(lane.dir.path().join("broker.log")).unwrap();
        let _server = broker.serve(&socket).unwrap();
        bridge.cfg.broker = socket.to_string_lossy().into_owned();
        let owed = OwedReceipt {
            sid: SID.to_string(),
            rid: 1,
            off: 31,
            verdict: "handled".to_string(),
            kind: "ask".to_string(),
            from: "s-y@n-evil".to_string(),
            earlier_log: false,
        };
        let addr = subject::InAddr {
            node: bridge.node.clone(),
            sid: SID.to_string(),
            src: "n-evil".to_string(),
            kind: "note".to_string(),
        };
        let mut body = Body::new(0);
        body.from = Some("s-y".to_string());

        let (conn, closer) = bridge.connect().unwrap();
        bridge.conn = Some(conn.into());
        closer.close();
        assert!(
            bridge.send_receipt(&owed).is_err(),
            "the receipt stays owed"
        );
        assert!(bridge.conn.is_none(), "a failed read discards the socket");
        let (conn, closer) = bridge.connect().unwrap();
        bridge.conn = Some(conn.into());
        closer.close();
        assert_eq!(
            bridge.sender_lane(&addr, &body),
            Err(NoLane::Undrained),
            "no verdict, and no claim that the cap refused it"
        );

        let far = OwedReceipt {
            rid: 2,
            from: "s-far".to_string(),
            ..owed.clone()
        };
        let (conn, closer) = bridge.connect().unwrap();
        bridge.conn = Some(conn.into());
        closer.close();
        assert!(
            bridge.send_receipt(&far).is_err(),
            "a bare sender the roster could not place stays owed"
        );
        assert!(
            bridge.resolve_to("@s-far", Some(SID)).is_err(),
            "no connection at all is no answer either"
        );
        assert!(!bridge.state.pins().contains_key("s-far"));
        let (conn, _) = bridge.connect().unwrap();
        bridge.conn = Some(conn.into());
        assert_eq!(
            bridge.resolve_to("@s-far", Some(SID)).unwrap(),
            Route::Unroutable
        );
        assert!(!bridge.state.pins().contains_key("s-far"));

        bridge.send_receipt(&owed).unwrap();
        assert_eq!(lane.delivery_offsets().len(), 1, "retired with its offset");
    }

    #[cfg(unix)]
    #[test]
    fn the_routing_read_folds_in_the_records_its_walk_omitted() {
        use astream_broker::proto::{decode_request, encode_response, read_frame, write_frame};
        use astream_broker::{Request, Response};
        let scripted = |fetch_next: u64| -> Conn {
            let (ours, theirs) = std::os::unix::net::UnixStream::pair().unwrap();
            std::thread::spawn(move || {
                let mut s = theirs;
                let row = |offset: u64, node: &str, state: &str| Response::Delivery {
                    offset,
                    subject: format!("/f/f/pub/{node}/s-x/presence"),
                    body: format!("v=1 t=1 state={state}").into_bytes(),
                };
                while let Ok(Some(frame)) = read_frame(&mut s) {
                    let (rows, next, head) = match decode_request(&frame) {
                        Some(Request::Last { .. }) => {
                            (vec![row(3, "n-a", "live"), row(5, "n-b", "live")], 10, 10)
                        }
                        Some(Request::Fetch {
                            from_offset: 10, ..
                        }) => (
                            vec![row(11, "n-b", "exited"), row(12, "n-c", "live")],
                            fetch_next,
                            13,
                        ),
                        _ => return,
                    };
                    for r in rows.iter().chain([&Response::Mark {
                        next,
                        head,
                        resume: String::new(),
                        durable: None,
                        oldest: None,
                    }]) {
                        if write_frame(&mut s, &encode_response(r)).is_err() {
                            return;
                        }
                    }
                }
            });
            Conn::from_stream(Box::new(ours))
        };
        let (_lane, mut bridge) = FakeVerbLane::bridge(&[]);
        bridge.conn = Some(scripted(13).into());
        let live = bridge.advertisers("s-x").unwrap();
        assert_eq!(
            live,
            BTreeSet::from(["n-a".to_string(), "n-c".to_string()]),
            "n-b's `exited` row past the walk is no advertiser"
        );
        bridge.conn = Some(scripted(12).into());
        assert!(
            bridge.advertisers("s-x").is_err(),
            "a stretch read short of its head is unknown"
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_presence_walk_services_a_halt_between_pages_and_stops_on_disconnect() {
        use astream_broker::proto::{decode_request, encode_response, read_frame, write_frame};
        use astream_broker::{Request, Response};
        const SID: &str = "s-local";
        for closed in [false, true] {
            let (lane, mut bridge) = FakeVerbLane::bridge(&[(1, SID)]);
            bridge.refresh_sessions().unwrap();
            let mailbox = Arc::clone(&bridge.mailbox);
            let generation = mailbox.broker_generation(Source::Fleet);
            let transcript = Arc::clone(&lane.transcript);
            let (ours, mut peer) = std::os::unix::net::UnixStream::pair().unwrap();
            for socket in [&ours, &peer] {
                socket
                    .set_read_timeout(Some(aterm_spec::wait::MUST_HAPPEN))
                    .unwrap();
                socket
                    .set_write_timeout(Some(aterm_spec::wait::MUST_HAPPEN))
                    .unwrap();
            }
            let server = std::thread::spawn(move || {
                let row = |offset, node: &str, state| Response::Delivery {
                    offset,
                    subject: format!("/f/f/pub/{node}/s-x/presence"),
                    body: format!("v=1 t=1 state={state}").into_bytes(),
                };
                while let Ok(Some(frame)) = read_frame(&mut peer) {
                    let responses = match decode_request(&frame).unwrap() {
                        Request::Last { after, .. } => {
                            transcript.lock().unwrap().push(format!("last {after}"));
                            if after.is_empty() {
                                let _ = mailbox.push(
                                    Source::Fleet,
                                    (
                                        7,
                                        "/f/f/fleet/h-t/halt".into(),
                                        b"v=1 t=1 state=on reason=stop".to_vec(),
                                    ),
                                    generation,
                                );
                                if closed {
                                    let inbox = mailbox.broker_generation(Source::Inbox);
                                    mailbox.push_closed(Source::Inbox, inbox);
                                }
                                vec![Response::Mark {
                                    next: 10,
                                    head: 10,
                                    resume: "cursor".into(),
                                    durable: None,
                                    oldest: None,
                                }]
                            } else {
                                assert_eq!(after, "cursor");
                                vec![
                                    row(3, "n-a", "live"),
                                    row(12, "n-b", "live"),
                                    Response::Mark {
                                        next: 20,
                                        head: 20,
                                        resume: String::new(),
                                        durable: None,
                                        oldest: None,
                                    },
                                ]
                            }
                        }
                        Request::Fetch { max: 0, .. } if closed => vec![Response::Mark {
                            next: 10,
                            head: 10,
                            resume: String::new(),
                            durable: None,
                            oldest: None,
                        }],
                        Request::Fetch { from_offset, .. } => {
                            assert_eq!(from_offset, 10, "the first page anchors the stretch");
                            vec![
                                row(21, "n-b", "exited"),
                                row(22, "n-c", "live"),
                                Response::Mark {
                                    next: 30,
                                    head: 30,
                                    resume: String::new(),
                                    durable: None,
                                    oldest: None,
                                },
                            ]
                        }
                        Request::Publish { .. } => vec![Response::PublishAck {
                            offset: 14,
                            deduped: false,
                        }],
                        other => panic!("unexpected request: {other:?}"),
                    };
                    for response in responses {
                        write_frame(&mut peer, &encode_response(&response)).unwrap();
                    }
                }
            });
            bridge.conn = Some(Conn::from_stream(Box::new(ours)).into());
            let answer = bridge.advertisers("s-x");
            if closed {
                assert!(
                    answer.is_err(),
                    "a disconnected walk is not a complete face"
                );
                assert!(bridge.conn.is_none());
            } else {
                assert_eq!(
                    answer.unwrap(),
                    BTreeSet::from(["n-a".into(), "n-c".into()])
                );
            }
            let transcript = lane.transcript();
            let held = transcript
                .iter()
                .position(|line| line == &format!("hold {SID} on reason=stop origin=fleet"))
                .unwrap();
            if closed {
                assert!(!transcript.iter().any(|line| line == "last cursor"));
            } else {
                let next = transcript
                    .iter()
                    .position(|line| line == "last cursor")
                    .unwrap();
                assert!(held < next, "halt waited for later pages: {transcript:#?}");
            }
            assert!(bridge.halt_applied);
            bridge.conn = None;
            server.join().unwrap();
        }
    }

    #[cfg(unix)]
    struct ReadTap {
        inner: std::os::unix::net::UnixStream,
        pending: Vec<u8>,
        reads: Arc<ReadCounts>,
    }

    #[cfg(unix)]
    #[derive(Default)]
    struct ReadCounts {
        last: std::sync::atomic::AtomicUsize,
        fetch: std::sync::atomic::AtomicUsize,
        fetched_from: std::sync::Mutex<Vec<u64>>,
    }

    #[cfg(unix)]
    impl ReadCounts {
        fn take(&self) -> (usize, usize) {
            use std::sync::atomic::Ordering::SeqCst;
            (self.last.swap(0, SeqCst), self.fetch.swap(0, SeqCst))
        }

        fn last_fetched_from(&self) -> Option<u64> {
            self.fetched_from.lock().unwrap().last().copied()
        }
    }

    #[cfg(unix)]
    impl std::io::Read for ReadTap {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            self.inner.read(buf)
        }
    }

    #[cfg(unix)]
    impl std::io::Write for ReadTap {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            use astream_broker::proto::{decode_request, read_frame};
            use astream_broker::Request;
            use std::sync::atomic::Ordering::SeqCst;
            let n = self.inner.write(buf)?;
            self.pending.extend_from_slice(&buf[..n]);
            loop {
                let mut cursor = std::io::Cursor::new(&self.pending[..]);
                let Ok(Some(frame)) = read_frame(&mut cursor) else {
                    break;
                };
                let used = usize::try_from(cursor.position()).unwrap();
                match decode_request(&frame) {
                    Some(Request::Last { .. }) => {
                        self.reads.last.fetch_add(1, SeqCst);
                    }
                    Some(Request::Fetch { from_offset, .. }) => {
                        self.reads.fetch.fetch_add(1, SeqCst);
                        self.reads.fetched_from.lock().unwrap().push(from_offset);
                    }
                    _ => {}
                }
                self.pending.drain(..used);
            }
            Ok(n)
        }

        fn flush(&mut self) -> io::Result<()> {
            self.inner.flush()
        }
    }

    #[cfg(unix)]
    #[test]
    fn a_bare_post_to_a_remote_peer_walks_once_while_the_presence_view_is_live() {
        use crate::presence_view::Phase as ViewPhase;
        const SID: &str = "s-near";
        let (lane, mut bridge) = FakeVerbLane::bridge(&[(1, SID)]);
        let socket = lane.dir.path().join("broker.sock");
        let broker = crate::test_broker(lane.dir.path().join("broker.log")).unwrap();
        let _server = broker.serve(&socket).unwrap();
        bridge.cfg.broker = socket.to_string_lossy().into_owned();
        let reads = Arc::new(ReadCounts::default());
        let tap = ReadTap {
            inner: std::os::unix::net::UnixStream::connect(&socket).unwrap(),
            pending: Vec::new(),
            reads: Arc::clone(&reads),
        };
        bridge.conn = Some(Conn::from_stream(Box::new(tap)).into());
        let node = bridge.node.clone();
        let to = |far: &str, sid: &str| Route::To(format!("/f/f/in/{far}/{sid}/{node}"));
        let (mut god, _) = bridge.connect().unwrap();
        let (mut reader, _) = bridge.connect().unwrap();
        let mut seq = 0;
        let mut publish = |subject: &str, body: &str| -> u64 {
            seq += 1;
            god.publish(7, seq, subject, body.as_bytes()).unwrap().0
        };
        let view = Arc::clone(&bridge.presence_view);
        let caught_up = |off: u64| {
            assert!(
                view.wait_for(aterm_spec::wait::MUST_HAPPEN, |phase, reached| {
                    phase == ViewPhase::Live && reached > off
                }),
                "the presence view never showed offset {off}"
            );
        };
        let counted = |bridge: &mut Bridge, to: &str| {
            reads.take();
            let route = bridge.resolve_to(to, Some(SID)).unwrap();
            let (last, fetch) = reads.take();
            (route, last, fetch)
        };
        let walked = |(_, last, fetch): &(Route, usize, usize)| *last >= 1 && *fetch >= 1;
        const LIVE: &str = "v=1 t=1 state=live";
        const EXITED: &str = "v=1 t=1 state=exited";
        let row = |far: &str, sid: &str| format!("/f/f/pub/{far}/{sid}/presence");

        let off = publish(&row("n-far", "s-far"), LIVE);
        assert_eq!(view.phase(), ViewPhase::Closed);
        let first = counted(&mut bridge, "@s-far");
        assert_eq!(first.0, to("n-far", "s-far"));
        assert!(walked(&first), "a Last and its stretch's Fetch: {first:?}");
        assert_eq!(bridge.state.pins()["s-far"], "n-far");
        caught_up(off);
        assert_eq!(counted(&mut bridge, "@s-far"), (to("n-far", "s-far"), 0, 1));

        publish(&row("n-a", "s-moved"), LIVE);
        publish(&row("n-a", "s-moved"), EXITED);
        let moved = publish(&row("n-b", "s-moved"), LIVE);
        caught_up(moved);
        assert_eq!(
            counted(&mut bridge, "@s-moved"),
            (to("n-b", "s-moved"), 0, 1)
        );
        assert_eq!(
            bridge.state.pins()["s-moved"],
            "n-b",
            "first sight, through the view"
        );

        publish(&row("n-far", "s-far"), EXITED);
        let off = publish(&row("n-c", "s-far"), LIVE);
        caught_up(off);
        let contested = counted(&mut bridge, "@s-far");
        assert_eq!(contested.0, Route::Ambiguous);
        assert!(
            walked(&contested),
            "the verdict that retires is the walk's: {contested:?}"
        );

        let miss = counted(&mut bridge, "@s-nobody");
        assert_eq!(miss.0, Route::Unroutable);
        assert!(
            walked(&miss),
            "the unroutable verdict is the walk's: {miss:?}"
        );

        view.cut();
        assert!(
            view.wait_for(aterm_spec::wait::MUST_HAPPEN, |phase, _| phase
                == ViewPhase::Dead),
            "a cut subscription is a dead view"
        );
        bridge.presence_view_retry_at = Some(Instant::now() + aterm_spec::wait::MUST_HAPPEN);
        let dead = counted(&mut bridge, "@s-moved");
        assert_eq!(dead.0, to("n-b", "s-moved"));
        assert!(walked(&dead), "a dead view is a walk: {dead:?}");
        assert_eq!(
            view.phase(),
            ViewPhase::Dead,
            "not redialled inside the bound"
        );
        bridge.presence_view_retry_at = None;
        assert!(walked(&counted(&mut bridge, "@s-moved")));
        caught_up(off);
        assert_eq!(
            counted(&mut bridge, "@s-moved"),
            (to("n-b", "s-moved"), 0, 1)
        );

        bridge.presence_view.close();
        let g = view.open();
        assert!(view.apply(
            g,
            "f",
            moved,
            "/f/f/pub/n-b/s-moved/presence",
            b"v=1 t=1 state=live"
        ));
        assert!(view.mark(g, off + 1, off + 1));
        let exact = || view.snapshot("s-moved").expect("live").exact_to;

        for i in 0..20 {
            publish(&format!("/f/f/term/n-z/s-z/{i}"), "busy");
        }
        let head = reader.fetch(0, "/f/f/>", 0).unwrap().1 .1;
        assert!(exact() < head, "the view is behind the traffic");
        assert_eq!(
            counted(&mut bridge, "@s-moved"),
            (to("n-b", "s-moved"), 0, 1)
        );
        assert_eq!(reads.last_fetched_from(), Some(off + 1));
        assert_eq!(exact(), head, "the confirmation moved the view to the head");
        assert_eq!(
            counted(&mut bridge, "@s-moved"),
            (to("n-b", "s-moved"), 0, 1)
        );
        assert_eq!(
            reads.last_fetched_from(),
            Some(head),
            "the next confirmation scans nothing the last one did"
        );

        let rival_off = publish(&row("n-d", "s-moved"), LIVE);
        assert_eq!(
            view.snapshot("s-moved").map(|s| s.nodes),
            Some(std::collections::BTreeSet::from(["n-b".to_string()])),
            "NEGATIVE CONTROL: unconfirmed, the view routes it to the pinned node"
        );
        let rival = counted(&mut bridge, "@s-moved");
        assert_eq!(
            rival.0,
            Route::Ambiguous,
            "a claim on the log before the post is the post's verdict"
        );
        assert!(
            rival.1 >= 1 && rival.2 >= 2,
            "the confirmation, then the walk: {rival:?}"
        );
        assert_eq!(
            exact(),
            rival_off,
            "exact up to the presence record it found, which it has not applied"
        );

        let g = view.open();
        assert!(view.apply(
            g,
            "f",
            moved,
            "/f/f/pub/n-b/s-moved/presence",
            b"v=1 t=1 state=live"
        ));
        assert!(view.mark(g, rival_off, rival_off));
        assert!(view.mark(g, rival_off, rival_off + CONFIRM_SCAN_MAX + 1));
        let long = counted(&mut bridge, "@s-moved");
        assert_eq!(long.0, Route::Ambiguous);
        assert_eq!((long.1, long.2), (1, 1), "the walk alone: {long:?}");
        let g = view.open();
        assert!(view.apply(
            g,
            "f",
            moved,
            "/f/f/pub/n-b/s-moved/presence",
            b"v=1 t=1 state=live"
        ));
        assert!(view.mark(g, rival_off, rival_off));
        let bus_high = std::mem::replace(&mut bridge.bus_high, rival_off + CONFIRM_SCAN_MAX + 1);
        let long = counted(&mut bridge, "@s-moved");
        assert_eq!((long.1, long.2), (1, 1), "the walk alone: {long:?}");
        bridge.bus_high = bus_high;

        let cut_a = publish(&row("n-cut-a", "s-cut"), LIVE);
        let solo = publish(&row("n-solo", "s-solo"), LIVE);
        let g = view.open();
        assert!(view.apply(g, "f", cut_a, &row("n-cut-a", "s-cut"), LIVE.as_bytes()));
        assert!(view.apply(g, "f", solo, &row("n-solo", "s-solo"), LIVE.as_bytes()));
        let before_cut = solo + 1;
        assert!(view.mark(g, before_cut, before_cut));
        let rival = publish(&row("n-cut-b", "s-cut"), LIVE);
        let floor = publish("/f/f/term/n-z/s-z/after-cut", "busy");
        assert!(before_cut <= rival && rival < floor);
        broker.retain_before_carrying_state(floor).expect("the cut");
        let filter = crate::presence_view::presence_filter("f");
        let refused = reader.fetch(before_cut, &filter, CONFIRM_ROWS).unwrap_err();
        assert_eq!(transport::retained_past(&refused, before_cut), Some(floor));
        let ((suffix, (next, head)), crossed) =
            transport::fetch_kept(&mut reader, before_cut, &filter, CONFIRM_ROWS).unwrap();
        assert_eq!(crossed, Some(floor));
        assert!(
            suffix.is_empty() && next == head,
            "Fetch cannot see the rival"
        );
        let (last, _) = reader.last(&filter, "", CONFIRM_ROWS).unwrap();
        for far in ["n-cut-a", "n-cut-b"] {
            assert!(
                last.iter()
                    .any(|(_, subject, _)| subject == &row(far, "s-cut")),
                "Last preserves the actual claim of {far}"
            );
        }
        assert_eq!(
            view.snapshot("s-cut").unwrap().nodes,
            BTreeSet::from(["n-cut-a".to_string()]),
            "NEGATIVE CONTROL: the stale view alone would route to n-cut-a"
        );
        let cut = counted(&mut bridge, "@s-cut");
        assert_eq!(
            cut.0,
            Route::Ambiguous,
            "the retained rival is still a rival"
        );
        assert!(walked(&cut), "the cut requires Last: {cut:?}");
        assert_eq!(view.snapshot("s-cut").unwrap().exact_to, before_cut);
        let solo = counted(&mut bridge, "@s-solo");
        assert_eq!(solo.0, to("n-solo", "s-solo"));
        assert!(walked(&solo), "a unique route survives the cut: {solo:?}");
        assert_eq!(view.snapshot("s-solo").unwrap().exact_to, before_cut);

        bridge.close_subscriptions();
        assert_eq!(view.phase(), ViewPhase::Closed);
        assert!(bridge.presence_view_retry_at.is_none());
    }

    #[cfg(unix)]
    #[test]
    fn the_presence_view_is_dialled_off_the_bridges_loop() {
        use crate::presence_view::Phase as ViewPhase;
        use std::io::Read as _;
        let (lane, mut bridge) = FakeVerbLane::bridge(&[(1, "s-near")]);
        let socket = lane.dir.path().join("mute.sock");
        let mute = std::os::unix::net::UnixListener::bind(&socket).unwrap();
        mute.set_nonblocking(true).unwrap();
        bridge.cfg.broker = socket.to_string_lossy().into_owned();
        bridge.caps = crate::enable::node_grants("f", &bridge.node)
            .into_iter()
            .map(|grant| Cap {
                grant,
                tag: [0; 32],
            })
            .collect();
        let (publisher, _peer) = std::os::unix::net::UnixStream::pair().unwrap();
        bridge.conn = Some(Conn::from_stream(Box::new(publisher)).into());

        bridge.want_presence_view();
        let view = Arc::clone(&bridge.presence_view);
        assert_eq!(
            view.phase(),
            ViewPhase::Seeding,
            "the ask returned before any attach was answered"
        );
        let (mut dialled, _) = aterm_spec::wait::until_within(
            aterm_spec::wait::MUST_HAPPEN,
            "the view's reader to dial",
            || mute.accept().ok(),
        );
        dialled.set_nonblocking(false).unwrap();
        dialled
            .set_read_timeout(Some(aterm_spec::wait::MUST_HAPPEN))
            .unwrap();
        let mut first = [0u8; 1];
        dialled
            .read_exact(&mut first)
            .expect("the reader's attach reaches the broker");
        drop(dialled);
        assert!(
            view.wait_for(aterm_spec::wait::MUST_HAPPEN, |phase, _| phase
                == ViewPhase::Dead),
            "a broker that hung up on the dial is a dead view"
        );
        assert!(bridge.conn.is_some(), "and nothing of the bridge's link");
    }

    #[cfg(unix)]
    fn presence_view_under_the_guard(
        serve: impl FnOnce(
            &astream_broker::Broker,
            &std::path::Path,
        ) -> (astream_broker::BrokerHandle, String),
    ) {
        use crate::presence_view::Phase as ViewPhase;
        const FAR: &str = "n-00000000000000fa";
        let secret = vec![0x5a_u8; 32];
        let (lane, mut bridge) = FakeVerbLane::bridge(&[(1, "s-near")]);
        let broker = astream_broker::Broker::open_guarded(
            lane.dir.path().join("guarded.log"),
            secret.clone(),
        )
        .unwrap();
        let (_server, endpoint) = serve(&broker, lane.dir.path());
        bridge.cfg.broker = endpoint.clone();
        let ring = |node: &str, keep: &dyn Fn(&str) -> bool| -> Vec<Cap> {
            crate::enable::node_grants("f", node)
                .into_iter()
                .filter(|grant| keep(grant))
                .map(|grant| {
                    let c = astream_cap::mint(&secret, &grant).expect("mint");
                    Cap {
                        grant: c.filter,
                        tag: c.tag,
                    }
                })
                .collect()
        };
        bridge.caps = ring(&bridge.node, &|_| true);
        bridge.conn = Some(bridge.connect().expect("the publisher attaches").0.into());
        let (mut far, _) = transport::connect(&endpoint, ACK_DEADLINE).unwrap();
        transport::attach_ring(&mut far, &ring(FAR, &|_| true)).unwrap();
        let subject = format!("/f/f/pub/{FAR}/s-far/presence");
        let off = far
            .publish(
                astream_cap::producer_id_of(FAR),
                1,
                &subject,
                b"v=1 t=1 state=live",
            )
            .unwrap()
            .0;
        let to = Route::To(format!("/f/f/in/{FAR}/s-far/{}", bridge.node));

        assert_eq!(bridge.resolve_to("@s-far", Some("s-near")).unwrap(), to);
        let view = Arc::clone(&bridge.presence_view);
        assert!(
            view.wait_for(aterm_spec::wait::MUST_HAPPEN, |phase, reached| {
                matches!(phase, ViewPhase::Live | ViewPhase::Dead)
                    && (phase == ViewPhase::Dead || reached > off)
            }),
            "the view never settled"
        );
        assert_eq!(
            view.phase(),
            ViewPhase::Live,
            "the ring covers the view's seed, its subscription and its idle marks"
        );
        let head = far.fetch(0, "/f/f/pub/>", 0).unwrap().1 .1;
        assert_eq!(bridge.resolve_to("@s-far", Some("s-near")).unwrap(), to);
        assert!(bridge.conn.is_some());
        assert_eq!(
            view.snapshot("s-far").map(|s| s.exact_to),
            Some(head),
            "the confirmation answered, through to the head"
        );

        bridge.presence_view.close();
        bridge.presence_view_retry_at = None;
        bridge.caps = ring(&bridge.node, &|grant| grant != "ro:/f/f/pub/>");
        bridge.want_presence_view();
        assert!(
            view.wait_for(aterm_spec::wait::MUST_HAPPEN, |phase, _| phase
                == ViewPhase::Dead),
            "a ring without the presence read is refused the view"
        );
    }

    #[cfg(unix)]
    #[test]
    fn the_presence_view_goes_live_under_a_nodes_ring_on_a_guarded_broker() {
        presence_view_under_the_guard(|broker, dir| {
            let socket = dir.join("g.sock");
            let server = broker.serve(&socket).expect("serve");
            (server, socket.to_string_lossy().into_owned())
        });
    }

    #[cfg(unix)]
    #[test]
    fn dropping_a_bridge_closes_its_live_presence_reader() {
        use crate::presence_view::Phase;

        struct ObservedStream {
            inner: Option<Box<dyn transport::Stream>>,
            ended: std::sync::mpsc::Sender<()>,
        }
        impl std::io::Read for ObservedStream {
            fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
                self.inner.as_mut().expect("live stream").read(buf)
            }
        }
        impl std::io::Write for ObservedStream {
            fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
                self.inner.as_mut().expect("live stream").write(buf)
            }

            fn flush(&mut self) -> io::Result<()> {
                self.inner.as_mut().expect("live stream").flush()
            }
        }
        impl Drop for ObservedStream {
            fn drop(&mut self) {
                drop(self.inner.take());
                let _ = self.ended.send(());
            }
        }

        let (lane, mut bridge) = FakeVerbLane::bridge(&[(1, "s-near")]);
        let socket = lane.dir.path().join("broker.sock");
        let broker = crate::test_broker(lane.dir.path().join("broker.log")).unwrap();
        let _server = broker.serve(&socket).unwrap();
        bridge.cfg.broker = socket.to_string_lossy().into_owned();
        let (mut publisher, _) = bridge.connect().unwrap();
        let off = publisher
            .publish(7, 1, "/f/f/pub/n-far/s-far/presence", b"v=1 t=1 state=live")
            .unwrap()
            .0;
        bridge.conn = Some(publisher.into());

        let (conn, closer) = bridge.connect().unwrap();
        let (ended, stopped) = std::sync::mpsc::channel();
        let observed = ObservedStream {
            inner: Some(conn.into_stream()),
            ended,
        };
        let view = Arc::clone(&bridge.presence_view);
        let generation = view.open();
        crate::presence_view::spawn_reader(
            move || Ok((Conn::from_stream(Box::new(observed)), closer.into_os())),
            &bridge.cfg.fleet,
            Arc::clone(&view),
            generation,
        )
        .unwrap();
        assert!(
            view.wait_for(aterm_spec::wait::MUST_HAPPEN, |phase, reached| {
                phase == Phase::Live && reached > off
            })
        );
        assert_eq!(
            view.snapshot("s-far").map(|snapshot| snapshot.nodes),
            Some(BTreeSet::from(["n-far".to_string()]))
        );
        assert!(matches!(
            stopped.try_recv(),
            Err(std::sync::mpsc::TryRecvError::Empty)
        ));

        drop(bridge);
        assert_eq!(
            view.phase(),
            Phase::Closed,
            "the bridge's view outlived its owner"
        );
        stopped
            .recv_timeout(aterm_spec::wait::MUST_HAPPEN)
            .expect("the presence reader still owns its broker stream");
        assert!(
            !view.mark(generation, off + 1, off + 1),
            "the old reader stays fenced"
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_record_refused_twice_answers_its_sender_once() {
        const SID: &str = "s-host";
        let (lane, mut bridge) = FakeVerbLane::bridge(&[(1, SID)]);
        let socket = lane.dir.path().join("broker.sock");
        let broker = crate::test_broker(lane.dir.path().join("broker.log")).unwrap();
        let _server = broker.serve(&socket).unwrap();
        bridge.cfg.broker = socket.to_string_lossy().into_owned();
        let (conn, _) = bridge.connect().unwrap();
        bridge.conn = Some(conn.into());
        assert!(bridge.refresh_sessions().is_ok());
        let node = bridge.node.clone();
        let local = format!("/f/f/in/{node}/{SID}/{node}/note");
        let raw = b"v=1 t=1 from=s-gone via=NOT-A-PRINCIPAL text=x";
        let off = bridge.publish_own_off(&local, raw).unwrap();
        for _ in 0..2 {
            assert_eq!(bridge.deliver_record(off, &local, raw), Delivery::Accounted);
        }
        let (mut reader, _) = bridge.connect().unwrap();
        let notices = format!("/f/f/in/{node}/s-gone/{node}/undeliverable");
        let (rows, _) = reader.fetch(0, &notices, 16).unwrap();
        let answers: Vec<Option<u64>> = rows.iter().map(|(_, _, b)| Body::decode(b).0.re).collect();
        assert_eq!(answers, [Some(off)], "one notice for one refused record");
    }

    const SHIPPED_DELIVER_USAGE: &str = "ERR usage: deliver <sid> off=<n> from=<p> kind=<k> \
        trust=<t> [re=<n>] [dl=<ms>] [late=1] [demoted=<k>] [via=<p,...>] \
        [verdict=<handled|refused|deferred>] [len=<n>] [text=<pct>] | deliver <sid> \
        landed=<post-id> off=<n> | deliver <sid> fetched=<off> (err=<token> | from=<p> \
        kind=<k> trust=<t> [re=<n>] [dl=<ms>] [demoted=<k>] [via=<p,...>] [verdict=<v>] \
        [len=<n>] [at=<n>] [more=1] [text=<pct>]) | deliver <sid> receipt=<rid> off=<n|->\n";

    #[cfg(unix)]
    #[test]
    fn a_broadcast_opt_out_race_leaves_the_sessions_other_mail_deliverable() {
        const SID: &str = "s-optout";
        let (lane, mut bridge) = FakeVerbLane::bridge(&[(1, SID)]);
        let socket = lane.dir.path().join("broker.sock");
        let broker = crate::test_broker(lane.dir.path().join("broker.log")).unwrap();
        let _server = broker.serve(&socket).unwrap();
        bridge.cfg.broker = socket.to_string_lossy().into_owned();
        bridge.conn = Some(bridge.connect().unwrap().0.into());
        assert!(bridge.refresh_sessions().is_ok());
        bridge.per_sid.pending_admit.clear();
        bridge.per_sid.topics.insert(
            SID.to_string(),
            BTreeMap::from([
                ("declined".to_string(), TopicCursor { next: 20 }),
                ("wanted".to_string(), TopicCursor { next: 20 }),
            ]),
        );
        bridge.say_drained_to = 20;
        bridge.say_bus_head = 20;

        lane.opt_in(SID, &["topic wanted since=@20"]);
        lane.answer_deliveries_to(SID, "ERR not-subscribed\n");
        bridge.on_say_record(&(
            20,
            "/f/f/pub/n-peer/s-peer/say/declined".to_string(),
            b"v=1 t=1 text=declined".to_vec(),
        ));
        let read_past = bridge.per_sid.topics[SID]["declined"].next;

        lane.deliver_answers.lock().unwrap().remove(SID);
        bridge.topic_change(SID, &mut "drop declined".split_whitespace());
        bridge.on_say_record(&(
            21,
            "/f/f/pub/n-peer/s-peer/say/wanted".to_string(),
            b"v=1 t=2 text=wanted".to_vec(),
        ));
        bridge.on_inbox_record(&(
            22,
            format!("/f/f/in/{}/{SID}/n-peer/note", bridge.node),
            b"v=1 t=3 from=s-peer text=addressed".to_vec(),
        ));
        assert_eq!(
            (
                read_past,
                bridge.per_sid.topics[SID]["wanted"].next,
                lane.delivery_offsets(),
                bridge.committed,
                bridge.per_sid.lane_owed.contains_key(SID),
            ),
            (21, 22, vec![21, 22], 22, false),
            "declining one broadcast must not turn the session's lane into a fault"
        );
    }

    #[cfg(unix)]
    #[test]
    fn an_answer_that_is_no_verdict_keeps_the_record_owed_and_holds_back_only_its_session() {
        const HELD: &str = "s-held";
        const OTHER: &str = "s-other";
        let (lane, mut bridge) = FakeVerbLane::bridge(&[(1, HELD), (2, OTHER)]);
        let socket = lane.dir.path().join("broker.sock");
        let broker = crate::test_broker(lane.dir.path().join("broker.log")).unwrap();
        let _server = broker.serve(&socket).unwrap();
        bridge.cfg.broker = socket.to_string_lossy().into_owned();
        let (conn, _) = bridge.connect().unwrap();
        bridge.conn = Some(conn.into());
        assert!(bridge.refresh_sessions().is_ok());
        bridge.per_sid.pending_admit.clear();
        let node = bridge.node.clone();
        let mail = |sid: &str, off: u64| -> BrokerRecord {
            (
                off,
                format!("/f/f/in/{node}/{sid}/n-peer/note"),
                b"v=1 t=1 from=s-peer text=hi".to_vec(),
            )
        };

        lane.answer_deliveries_to(HELD, SHIPPED_DELIVER_USAGE);
        bridge.on_inbox_record(&mail(HELD, 20));
        bridge.on_inbox_record(&mail(HELD, 21));
        bridge.on_inbox_record(&mail(OTHER, 22));
        assert_eq!(
            lane.transcript(),
            ["deliver 20", "deliver 22"],
            "the held session is asked once, and its next record is not sent"
        );
        assert_eq!(lane.delivery_offsets(), [22]);
        assert_eq!(bridge.inbox_commit_floor(), Some(20));
        assert_eq!(
            bridge.committed, 19,
            "a later record commits the group up to the owed one, never past it"
        );

        lane.answer_deliveries_to(OTHER, "ERR quota\n");
        bridge.on_inbox_record(&mail(OTHER, 23));
        lane.answer_deliveries_to(OTHER, "ERR no such session\n");
        bridge.on_inbox_record(&mail(OTHER, 24));
        assert_eq!(
            bridge.committed, 19,
            "and they do not move it past the floor"
        );

        let (mut reader, _) = bridge.connect().unwrap();
        let notices = format!("/f/f/in/n-peer/s-peer/{node}/undeliverable");
        let (rows, _) = reader.fetch(0, &notices, 16).unwrap();
        let told: Vec<(Option<u64>, String)> = rows
            .iter()
            .map(|(_, _, b)| {
                let (body, _) = Body::decode(b);
                (body.re, body.text)
            })
            .collect();
        assert_eq!(
            told,
            [
                (Some(23), "not delivered: quota".to_string()),
                (Some(24), "not delivered: not-hosted".to_string()),
            ],
            "the sender hears the verdicts, and nothing about the owed records"
        );
        let ev_of = |sid: &str, reader: &mut Conn| -> Vec<String> {
            let face = subject::session_face("f", &node, sid, "ev");
            let (rows, _) = reader.fetch(0, &face, 256).unwrap();
            rows.iter()
                .filter(|(_, s, _)| *s == face)
                .filter_map(|(_, _, raw)| {
                    let ev = Body::decode(raw).0.unknown.get("ev")?.clone();
                    Some(crate::pct::decode(&ev))
                })
                .collect()
        };
        assert_eq!(
            ev_of(HELD, &mut reader),
            ["lane-fault off=20 verb=deliver reply=usage lane=0"],
            "one fault, said once, and no `undeliverable`"
        );
        assert_eq!(
            ev_of(OTHER, &mut reader),
            [
                "undeliverable off=23 reason=quota",
                "undeliverable off=24 reason=not-hosted"
            ]
        );
    }

    #[cfg(unix)]
    #[test]
    fn inbox_commit_debt_follows_live_sessions_and_survives_reconnects() {
        const FIRST: &str = "s-first";
        const SECOND: &str = "s-second";
        const HEALTHY: &str = "s-healthy";
        for unattributed in [None, Some(10)] {
            let (lane, mut bridge) = FakeVerbLane::bridge(&[(1, FIRST), (2, SECOND), (3, HEALTHY)]);
            let socket = lane.dir.path().join("broker.sock");
            let broker = crate::test_broker(lane.dir.path().join("broker.log")).unwrap();
            let _server = broker.serve(&socket).unwrap();
            bridge.cfg.broker = socket.to_string_lossy().into_owned();
            bridge.conn = Some(bridge.connect().unwrap().0.into());
            assert!(bridge.refresh_sessions().is_ok());
            bridge.per_sid.pending_admit.clear();
            bridge.commit_floor = unattributed;
            let node = bridge.node.clone();
            let mail = |sid: &str, off: u64| -> BrokerRecord {
                (
                    off,
                    format!("/f/f/in/{node}/{sid}/n-peer/note"),
                    b"v=1 t=1 from=s-peer text=mail".to_vec(),
                )
            };
            lane.answer_deliveries_to(FIRST, SHIPPED_DELIVER_USAGE);
            lane.answer_deliveries_to(SECOND, SHIPPED_DELIVER_USAGE);
            bridge.on_inbox_record(&mail(FIRST, 20));
            bridge.on_inbox_record(&mail(SECOND, 30));
            bridge.on_inbox_record(&mail(HEALTHY, 40));
            let mut committed = vec![bridge.committed];

            lane.sessions
                .lock()
                .unwrap()
                .retain(|(_, sid)| sid != FIRST);
            assert!(bridge.refresh_sessions().is_ok());
            bridge.on_inbox_record(&mail(HEALTHY, 41));
            committed.push(bridge.committed);

            bridge.conn = None;
            assert!(bridge.attach_broker());
            assert!(!bridge.per_sid.lane_owed.contains_key(FIRST));
            assert!(bridge.per_sid.lane_owed.contains_key(SECOND));
            bridge.on_inbox_record(&mail(SECOND, 31));
            bridge.on_inbox_record(&mail(HEALTHY, 42));
            committed.push(bridge.committed);

            lane.sessions
                .lock()
                .unwrap()
                .retain(|(_, sid)| sid != SECOND);
            assert!(bridge.refresh_sessions().is_ok());
            bridge.on_inbox_record(&mail(HEALTHY, 43));
            committed.push(bridge.committed);
            assert!(bridge.per_sid.lane_owed.is_empty());
            assert_eq!(
                committed,
                if unattributed.is_some() {
                    vec![9, 9, 9, 9]
                } else {
                    vec![19, 29, 29, 43]
                },
                "only departed, attributed debt is discharged: {unattributed:?}"
            );
            assert_eq!(
                lane.delivery_offsets(),
                [40, 41, 42, 43],
                "healthy mail keeps landing while the two incompatible lanes are frozen"
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn a_log_of_another_identity_owes_nothing_the_old_one_owed() {
        const FAULTED: &str = "s-faulted";
        const HEALTHY: &str = "s-healthy";
        let (lane, mut bridge) = FakeVerbLane::bridge(&[(1, FAULTED), (2, HEALTHY)]);
        assert!(bridge.refresh_sessions().is_ok());
        let (a, _broker_a, _server_a) = serve_log(&lane, "a");
        bridge.cfg.broker = a;
        assert!(bridge.attach_broker());
        let node = bridge.node.clone();
        let mail = |sid: &str, off: u64| -> BrokerRecord {
            (
                off,
                format!("/f/f/in/{node}/{sid}/n-peer/note"),
                b"v=1 t=1 from=s-peer text=mail".to_vec(),
            )
        };
        lane.answer_deliveries_to(FAULTED, SHIPPED_DELIVER_USAGE);
        bridge.on_inbox_record(&mail(FAULTED, 20));
        bridge.commit_floor = Some(10);
        assert_eq!(bridge.inbox_commit_floor(), Some(10));

        let (b, _broker_b, _server_b) = serve_log(&lane, "b");
        bridge.close_subscriptions();
        bridge.conn = None;
        bridge.cfg.broker = b;
        assert!(bridge.attach_broker());
        assert_eq!(
            bridge.inbox_commit_floor(),
            None,
            "A's debt is no floor on B"
        );
        assert_eq!(
            bridge.per_sid.lane_owed.get(FAULTED),
            Some(&None),
            "the faulted lane stays held, owing nothing on B yet"
        );
        bridge.on_inbox_record(&mail(HEALTHY, 40));
        assert_eq!(bridge.committed, 40, "B's mail commits past A's old debt");
        bridge.on_inbox_record(&mail(FAULTED, 50));
        bridge.on_inbox_record(&mail(HEALTHY, 60));
        assert_eq!(bridge.committed, 49, "B's own owed record bounds B's group");
    }

    #[test]
    fn a_broadcast_on_a_dropped_topic_is_the_sessions_verdict_and_holds_nothing_back() {
        let (lane, mut bridge) = FakeVerbLane::bridge(&[(1, "s-a")]);
        bridge.say_drained_to = 5;
        bridge.say_bus_head = 5;
        bridge.per_sid.topics.insert(
            "s-a".to_string(),
            BTreeMap::from([("news".to_string(), TopicCursor { next: 5 })]),
        );
        lane.answer_deliveries_to("s-a", "ERR not-subscribed\n");
        let live = "/f/f/pub/n-x/s-1/say/news".to_string();
        bridge.on_say_record(&(5, live, Body::new(0).encode()));
        assert_eq!(lane.transcript(), ["deliver 5"]);
        assert!(
            bridge.per_sid.lane_owed.is_empty(),
            "the session is not held back: {:?}",
            bridge.per_sid.lane_owed
        );
        assert_eq!(
            bridge.per_sid.topics["s-a"]["news"].next, 6,
            "its cursor moves past the record it declined"
        );
    }

    #[cfg(unix)]
    #[test]
    fn the_hello_reads_each_endpoints_lane_and_build_and_refuses_none() {
        let own = aterm_types::version::build_id();
        let lane_now = aterm_types::control_verbs::BRIDGE_LANE;
        let (lane, mut bridge) = FakeVerbLane::bridge(&[(1, "s-a")]);
        bridge.introduce();
        assert_eq!(
            lane.hellos(),
            [format!("hello lane={lane_now} build={own} {HELLO_REFILL}")]
        );
        assert_eq!(
            (bridge.endpoint_lane, bridge.endpoint_build.clone()),
            (0, None)
        );

        lane.answer_hello_with(&format!("OK lane={lane_now} build={own}\n"));
        bridge.introduce();
        assert_eq!(
            (bridge.endpoint_lane, bridge.endpoint_build.as_deref()),
            (lane_now, Some(own.as_str()))
        );

        lane.answer_hello_with("OK lane=7 build=9.9.0+g0123456789ab future=1\n");
        bridge.introduce();
        assert_eq!(
            (bridge.endpoint_lane, bridge.endpoint_build.as_deref()),
            (7, Some("9.9.0+g0123456789ab"))
        );
        assert!(bridge.first_roster());
        assert!(lane.links().is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn an_endpoint_without_the_bridge_roster_is_named_and_never_attached() {
        let (lane, mut bridge) = FakeVerbLane::bridge(&[(1, "s-a")]);
        lane.answer_roster_with("ERR usage: sessions\n");
        assert!(!bridge.first_roster(), "a grammar the bridge cannot read");
        assert_eq!(lane.links(), ["link down reason=endpoint-grammar"]);
        assert!(bridge.locals.is_empty() && bridge.per_sid.epochs.is_empty());

        let (lane, mut bridge) = FakeVerbLane::bridge(&[(1, "s-a")]);
        lane.refuse_roster(true);
        assert!(
            bridge.first_roster(),
            "a busy endpoint is asked again later"
        );
        assert!(lane.links().is_empty(), "and nothing is reported for it");
        lane.refuse_roster(false);
        assert!(bridge.first_roster());
        assert_eq!(bridge.locals.get(&1).map(String::as_str), Some("s-a"));
    }

    #[cfg(unix)]
    #[test]
    fn an_older_halt_record_on_the_tail_does_not_lift_a_newer_standing_halt() {
        let (_lane, mut bridge) = FakeVerbLane::bridge(&[(1, "s-a")]);
        let halt = |off: u64, state: &str| -> BrokerRecord {
            let body = format!("v=1 t=1 state={state} reason=stop");
            (off, "/f/f/fleet/h-b/halt".to_string(), body.into_bytes())
        };
        bridge.apply_fleet_halts(vec![halt(130, "on")], 100);
        assert!(bridge.halt_applied);
        bridge.on_fleet_record(&halt(120, "off"));
        assert!(bridge.halt_applied, "an older `off` lifted a standing halt");
        bridge.on_fleet_record(&halt(130, "on"));
        assert!(bridge.halt_applied);
        bridge.on_fleet_record(&halt(140, "off"));
        assert!(!bridge.halt_applied, "a newer `off` lifts it");
    }

    #[cfg(unix)]
    fn local_pair_world(
        name: &str,
    ) -> (
        FakeVerbLane,
        Bridge,
        String,
        String,
        (astream_broker::Broker, astream_broker::BrokerHandle),
    ) {
        let (lane, mut bridge) = FakeVerbLane::bridge(&[(1, "s-a"), (2, "s-b")]);
        let (socket, broker, server) = serve_log(&lane, name);
        bridge.cfg.broker = socket.clone();
        assert!(bridge.attach_broker());
        let face = format!(
            "/f/{}/in/{}/s-b/{}/note",
            bridge.cfg.fleet, bridge.node, bridge.node
        );
        (lane, bridge, socket, face, (broker, server))
    }

    #[cfg(unix)]
    fn retired_off(lane: &FakeVerbLane, id: u64) -> u64 {
        lane.sent
            .lock()
            .unwrap()
            .iter()
            .filter(|line| line.split_whitespace().nth(3) == Some(&id.to_string()))
            .find_map(|line| {
                line.split_whitespace()
                    .find_map(|word| word.strip_prefix("off="))
                    .and_then(|off| off.parse().ok())
            })
            .unwrap_or_else(|| panic!("post {id} was not retired as landed"))
    }

    #[cfg(unix)]
    fn own_lane_post_costs(per_drain: u64, drains: u64) -> crate::state::FileOps {
        const A: &str = "s-a";
        const B: &str = "s-b";
        let (lane, mut bridge, _socket, _face, _broker) = local_pair_world("costs");
        let frame = |ids: std::ops::Range<u64>| -> String {
            ids.map(|id| format!("post sid={A} id={id} to=@{B} kind=note len=4\nping"))
                .collect()
        };
        *lane.outbox.lock().unwrap() = Some(frame(1..2));
        bridge.drain_outbox();
        bridge.seq_block = None;
        bridge.next_seq().unwrap();
        bridge.unsent = None;
        let before = bridge.state.ops();
        let mut next = 2;
        for _ in 0..drains {
            *lane.outbox.lock().unwrap() = Some(frame(next..next + per_drain));
            next += per_drain;
            bridge.drain_outbox();
        }
        let spent = bridge.state.ops() - before;
        let acked = bridge.state.self_acked();
        for id in 1..next {
            let off = retired_off(&lane, id);
            assert!(acked.contains(&off), "@{off} is not in acked: {acked:?}");
        }
        assert!(bridge.state.in_flight().unwrap().is_empty());
        for id in 1..next {
            assert_eq!(
                bridge.state.post_pin(A, id).unwrap(),
                None,
                "a retired post left its reservation"
            );
        }
        assert!(
            !bridge.state.root().join("sent").exists()
                && !bridge.state.root().join("inflight").exists(),
            "no file a post"
        );
        spent
    }

    /// WHAT A LOCAL POST COSTS THE STATE DIR (B19): four records of the post journal (its
    /// reservation, its record in flight, that record landed, the reservation retired) and the
    /// landed offset appended to `acked`, and nothing renamed, unlinked or read.
    #[cfg(unix)]
    #[test]
    fn a_local_post_costs_the_state_dir_a_counted_set_of_operations() {
        for (per_drain, drains) in [(1, 8), (8, 1)] {
            let posts = per_drain * drains;
            let spent = own_lane_post_costs(per_drain, drains);
            assert_eq!(
                spent,
                crate::state::FileOps {
                    reads: 0,
                    replaces: 0,
                    renames: 0,
                    fsyncs: 0,
                    barriers: 0,
                    unlinks: 0,
                    appends: 5 * posts,
                },
                "{drains} drain(s) of {per_drain} post(s)"
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn the_tallys_state_dir_rows_count_what_the_dir_did_and_an_off_tally_counts_none() {
        use crate::wait_tally::{Row, Tally, FILE};
        const A: &str = "s-a";
        const B: &str = "s-b";
        for on in [true, false] {
            let (lane, mut bridge, _socket, _face, _broker) = local_pair_world("tally-dir");
            let path = bridge.state.root().join(FILE);
            if on {
                bridge.tally_to(&path);
            }
            let before = bridge.state.ops();
            let (drains, per_drain) = (3u64, 2u64);
            let mut next = 1;
            for _ in 0..drains {
                *lane.outbox.lock().unwrap() = Some(
                    (next..next + per_drain)
                        .map(|id| format!("post sid={A} id={id} to=@{B} kind=note len=4\nping"))
                        .collect(),
                );
                next += per_drain;
                bridge.drain_outbox();
            }
            let posts = drains * per_drain;
            let spent = bridge.state.ops() - before;
            assert_eq!(
                (spent.unlinks, spent.appends),
                (0, 5 * posts),
                "on={on}: what a local post costs the dir"
            );
            bridge.tally_idle();
            if !on {
                assert!(!path.exists(), "an off tally writes nothing");
                let mut kept = Tally::default();
                bridge.state.copy_dir_rows(&mut kept);
                assert_eq!(kept, Tally::default(), "and its dir counts nothing");
                continue;
            }
            let t = Tally::parse(&std::fs::read_to_string(&path).unwrap()).unwrap();
            let n = |r: Row| t.row(r).count;
            assert!(t.dir, "a test build counts the dir");
            assert_eq!(
                (n(Row::DirReplace), n(Row::DirUnlink), n(Row::DirAppend)),
                (spent.replaces, spent.unlinks, spent.appends),
                "each kind row is the dir's own count"
            );
            for row in [
                Row::SetPostPin,
                Row::SetInFlight,
                Row::ClearInFlight,
                Row::ClearPostSeq,
                Row::AppendSelfAcked,
            ] {
                assert_eq!(n(row), posts, "{row:?}: one a post");
            }
            assert_eq!(n(Row::LaneOutbox), drains, "a peek a drain");
            assert_eq!(n(Row::LaneSessions), drains, "a roster read a drain");
            assert_eq!(n(Row::LaneSent), posts, "a retirement a post");
            assert_eq!(
                (n(Row::DrainPosts), t.row(Row::DrainPosts).total),
                (drains, posts),
                "each peek's posts"
            );
            assert_eq!(
                n(Row::PostPublish),
                0,
                "no `post` event ran these drains: the path is an event's"
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn a_post_events_path_is_timed_to_each_publish_and_its_event_counted_as_a_wake_or_queued() {
        use crate::wait_tally::{Row, Tally, FILE};
        let (lane, mut bridge) = FakeVerbLane::bridge(&[(1, "s-a"), (2, "s-b")]);
        assert!(bridge.refresh_sessions().is_ok());
        let (socket, _broker, _server) = serve_log(&lane, "tally-path");
        bridge.cfg.broker = socket;
        bridge.conn = Some(bridge.connect().unwrap().0.into());
        let node = bridge.node.clone();
        bridge
            .indexed_subjects
            .insert(subject::post_lane("f", &node, "s-b", &node, "note"));
        lane.retire_from_outbox();
        let path = bridge.state.root().join(FILE);
        bridge.tally_to(&path);
        let outbox = |ids: std::ops::Range<u64>| {
            *lane.outbox.lock().unwrap() = Some(
                ids.map(|id| queued_note("s-a", id, "@s-b", "ping"))
                    .collect(),
            );
        };
        let tally = |bridge: &mut Bridge| {
            run_rounds(bridge);
            bridge.settle_acks();
            bridge.tally_idle();
            Tally::parse(&std::fs::read_to_string(&path).unwrap()).unwrap()
        };
        let rows = |t: &Tally| {
            [Row::PostPublish, Row::QueuedEvent, Row::WakeEvent].map(|row| t.row(row).count)
        };

        outbox(1..3);
        bridge.mailbox.push_event("EVENT 1 post 1".to_string());
        let queued = tally(&mut bridge);
        assert_eq!(
            rows(&queued),
            [2, 1, 0],
            "a path a fresh post, a queued event"
        );

        outbox(3..4);
        bridge.mailbox.push_event("EVENT 1 fetch off=9".to_string());
        let fetched = tally(&mut bridge);
        assert_eq!(rows(&fetched), [2, 2, 0], "a fetch's drain times no path");

        assert_eq!(
            bridge.conn.as_ref().map(Link::unacked),
            Some(0),
            "PRECONDITION: nothing owed, so the park is the whole wait"
        );
        outbox(4..5);
        let mailbox = bridge.mailbox_handle();
        let pusher = std::thread::spawn(move || {
            aterm_spec::wait::until_within(
                aterm_spec::wait::MUST_HAPPEN,
                "the loop parked on its mailbox",
                || mailbox.parked().then_some(()),
            );
            mailbox.push_event("EVENT 1 post 4".to_string());
        });
        assert!(bridge.round(aterm_spec::wait::MUST_HAPPEN));
        pusher.join().expect("the pusher");
        let woke = tally(&mut bridge);
        assert_eq!(rows(&woke), [3, 2, 1], "a wake, and its post's path");
        assert_eq!(
            retirements(&lane)
                .iter()
                .filter(|(_, _, off)| off.is_some())
                .count(),
            4,
            "every post landed: {:?}",
            retirements(&lane)
        );
        assert!(
            woke.row(Row::PostPublish).total > 0,
            "a path is timed, not only counted"
        );
    }

    #[cfg(unix)]
    #[test]
    fn acked_is_replaced_whole_after_a_failed_write_and_at_twice_its_keep() {
        let (lane, mut bridge, _socket, _face, _broker) = local_pair_world("acked-whole");
        let acked = bridge.state.root().join("acked");
        let state = bridge.state.root().to_path_buf();
        let post = |lane: &FakeVerbLane, bridge: &mut Bridge, id: u64| {
            *lane.outbox.lock().unwrap() = Some(format!(
                "post sid=s-a id={id} to=@s-b kind=note len=4\nping"
            ));
            let records = |bridge: &Bridge| {
                let journal = bridge.state.root().join(crate::state::journal::FILE);
                crate::state::journal::records_and_attaches(
                    &std::fs::read(journal).unwrap_or_default(),
                )
                .0
            };
            let (before, journaled) = (bridge.state.ops(), records(bridge));
            bridge.drain_outbox();
            let spent = bridge.state.ops() - before;
            let acked_appends = spent.appends - (records(bridge) - journaled);
            (retired_off(lane, id), acked_appends, spent.replaces)
        };
        let (first, appends, replaces) = post(&lane, &mut bridge, 1);
        assert_eq!((appends, replaces), (1, 0), "an ordinary append");

        std::fs::remove_file(&acked).unwrap();
        std::fs::create_dir(&acked).unwrap();
        let (second, _, _) = post(&lane, &mut bridge, 2);
        assert!(!bridge.acked_appendable, "the write failed");
        assert_eq!(
            bridge.state.in_flight().unwrap().len(),
            1,
            "the entry stays while acked does not hold its offset"
        );
        std::fs::remove_dir(&acked).unwrap();
        let (third, appends, replaces) = post(&lane, &mut bridge, 3);
        assert_eq!((appends, replaces), (0, 1), "replaced whole");
        let on_disk = bridge.state.self_acked();
        assert!(
            [first, second, third]
                .iter()
                .all(|off| on_disk.contains(off)),
            "{on_disk:?}"
        );

        bridge.acked_entries = 2 * SELF_ACK_KEEP;
        let (fourth, appends, replaces) = post(&lane, &mut bridge, 4);
        assert_eq!((appends, replaces), (0, 1), "compacted");
        assert_eq!(bridge.acked_entries, bridge.self_acked.len());
        assert!(bridge.state.self_acked().contains(&fourth));
        drop(bridge);

        let window: u64 = SELF_ACK_KEEP as u64;
        let mut log = String::new();
        for off in 0..2 * window {
            log.push_str(&format!("{off},"));
        }
        std::fs::write(&acked, log).unwrap();
        let (_second_lane, bridge) =
            FakeVerbLane::bridge_with_state(&[(1, "s-a"), (2, "s-b")], Some(&state));
        assert_eq!(
            bridge.self_acked,
            (window..2 * window).collect::<BTreeSet<u64>>(),
            "the newest window of a longer log"
        );
        assert_eq!(bridge.acked_entries, 2 * SELF_ACK_KEEP);
    }

    #[cfg(unix)]
    #[test]
    fn a_local_post_that_landed_is_not_published_again_on_the_next_log() {
        let (lane, mut bridge, _socket, face, _log_a) = local_pair_world("landed-a");
        let state = bridge.state.root().to_path_buf();
        *lane.outbox.lock().unwrap() =
            Some("post sid=s-a id=1 to=@s-b kind=note len=4\nping".to_string());
        bridge.drain_outbox();
        let off = retired_off(&lane, 1);
        assert!(
            bridge.state.self_acked().contains(&off),
            "acked holds the landed offset before anything else happens"
        );
        drop(bridge);

        let (_second, mut bridge) =
            FakeVerbLane::bridge_with_state(&[(1, "s-a"), (2, "s-b")], Some(&state));
        let (b, _broker_b, _server_b) = serve_log(&lane, "landed-b");
        bridge.cfg.broker = b;
        assert!(bridge.attach_broker());
        let (rows, _) = bridge.conn_now().unwrap().fetch(0, &face, 8).unwrap();
        assert!(
            rows.is_empty(),
            "a record that landed on the old log was published again: {rows:?}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn an_own_lane_posts_reservation_stays_readable_by_an_older_bridge() {
        let (lane, mut bridge, _socket, _face, _log) = local_pair_world("older-reader");
        lane.refuse_retirements(true);
        *lane.outbox.lock().unwrap() =
            Some("post sid=s-a id=1 to=@s-b kind=note len=4\nping".to_string());
        bridge.drain_outbox();
        assert!(lane.sent.lock().unwrap().is_empty(), "left listed");
        let pin = bridge.state.post_pin("s-a", 1).unwrap();
        assert!(pin.is_some(), "the journal holds the reservation");
        bridge.end_run();
        let sent = bridge.state.root().join("sent");
        let mut names = Vec::new();
        for entry in std::fs::read_dir(&sent).unwrap() {
            let entry = entry.unwrap();
            let name = entry.file_name().to_string_lossy().into_owned();
            if name.starts_with('.') {
                continue;
            }
            let text = std::fs::read_to_string(entry.path()).unwrap();
            if let Err(e) = PublishPin::parse(&text) {
                panic!("an older bridge cannot read sent/{name} ({e}): {text:?}");
            }
            names.push(name);
        }
        assert_eq!(names, ["s-a.1"], "the reservation is kept");
        let inflight = bridge.state.root().join("inflight");
        if let Ok(entries) = std::fs::read_dir(&inflight) {
            for entry in entries.map(Result::unwrap) {
                let name = entry.file_name().to_string_lossy().into_owned();
                if name.starts_with('.') {
                    continue;
                }
                let seq: u64 = name.parse().unwrap();
                let text = std::fs::read_to_string(entry.path()).unwrap();
                assert!(InFlight::parse(seq, &text).is_some(), "inflight/{name}");
            }
        }
        assert!(
            !bridge
                .state
                .root()
                .join(crate::state::journal::FILE)
                .exists(),
            "the stop unlinked the journal, so a later bridge of this build reads the files"
        );
        let next = StateDir::open(bridge.state.root()).unwrap();
        assert_eq!(next.post_pin("s-a", 1).unwrap(), pin, "and takes it back");
        assert!(!sent.join("s-a.1").exists());
    }

    #[test]
    fn a_legacy_pins_retry_after_a_kill_still_finds_its_evidence_in_acked() {
        let (lane, mut bridge) = FakeVerbLane::bridge(&[]);
        let state = bridge.state.root().to_path_buf();
        let socket = lane.dir.path().join("broker.sock");
        let broker = crate::test_broker(lane.dir.path().join("broker.log")).unwrap();
        let _server = broker.serve(&socket).unwrap();
        bridge.cfg.broker = socket.to_string_lossy().into_owned();
        bridge.conn = Some(bridge.connect().unwrap().0.into());
        let original = format!("/f/f/in/p/h-a/{}/note", bridge.node);
        let moved = format!("/f/f/in/p/h-b/{}/note", bridge.node);
        assert!(!bridge.on_own_lane(&original));
        let mut body = Body::new(1);
        body.from = Some("s-host".into());
        body.text = "legacy".into();
        let pin = PublishPin {
            seq: bridge.next_seq().unwrap(),
            via_key: false,
            identity: None,
            original: None,
            original_off: None,
            owner: None,
        };
        bridge.state.set_post_pin("s-host", 1, &pin).unwrap();
        let (off, deduped, _) = bridge
            .publish_reserved(&pin, &original, &body.encode())
            .unwrap();
        assert!(!deduped);
        let _ = bridge.remember_self_ack(off);
        drop(bridge);

        let (_second, mut bridge) = FakeVerbLane::bridge_with_state(&[], Some(&state));
        bridge.cfg.broker = socket.to_string_lossy().into_owned();
        bridge.conn = Some(bridge.connect().unwrap().0.into());
        let pin = bridge.state.post_pin("s-host", 1).unwrap().unwrap();
        let retry = bridge.publish_reserved(&pin, &moved, &body.encode());
        assert_eq!(
            retry.unwrap_or_else(|e| panic!("the legacy retry was refused: {e}")),
            (off, true, None)
        );
        assert!(bridge.conn.is_some(), "the connection stands");
    }

    #[cfg(unix)]
    fn serve_log(
        lane: &FakeVerbLane,
        name: &str,
    ) -> (String, astream_broker::Broker, astream_broker::BrokerHandle) {
        let socket = lane.dir.path().join(format!("{name}.sock"));
        let broker = crate::test_broker(lane.dir.path().join(format!("{name}.log"))).unwrap();
        let server = broker.serve(&socket).unwrap();
        (socket.to_string_lossy().into_owned(), broker, server)
    }

    #[cfg(unix)]
    fn node_evs(bridge: &Bridge) -> Vec<String> {
        let (mut reader, _) = bridge.connect().unwrap();
        let face = subject::node_face(&bridge.cfg.fleet, &bridge.node, "ev");
        let (rows, _) = reader.fetch(0, &face, 256).unwrap();
        rows.iter()
            .map(|(_, _, b)| crate::pct::decode(&String::from_utf8_lossy(b)))
            .collect()
    }

    #[cfg(unix)]
    fn next_delivery<S: std::io::Read + std::io::Write>(
        sub: &mut astream_broker::Subscription<S>,
        what: &str,
    ) -> BrokerRecord {
        let until = Instant::now() + aterm_spec::wait::MUST_HAPPEN;
        loop {
            match sub.recv_event().expect(what) {
                Some(astream_broker::Event::Delivery {
                    offset,
                    subject,
                    body,
                }) => return (offset, subject, body),
                Some(astream_broker::Event::Mark { .. }) => assert!(
                    Instant::now() < until,
                    "{what}: nothing delivered within {:?}",
                    aterm_spec::wait::MUST_HAPPEN
                ),
                Some(astream_broker::Event::Gap(gap)) => {
                    panic!("{what}: retention gave up records below the tail: {gap:?}")
                }
                None => panic!("{what}: the broker ended the subscription"),
            }
        }
    }

    #[cfg(unix)]
    #[test]
    fn a_halt_published_during_the_attach_refill_is_applied_before_it_ends() {
        const SID: &str = "s-busy";
        let (lane, mut bridge) = FakeVerbLane::bridge(&[(1, SID)]);
        let (socket, _broker, _server) = serve_log(&lane, "broker");
        bridge.cfg.broker = socket;
        let (mut writer, _) = bridge.connect().unwrap();
        let mail = format!("/f/f/in/{}/{SID}/h-writer/note", bridge.node);
        let (one, _) = writer.publish(81, 1, &mail, b"v=1 t=1 text=one").unwrap();
        let (two, _) = writer.publish(81, 2, &mail, b"v=1 t=2 text=two").unwrap();
        assert!(bridge.refresh_sessions().is_ok());
        assert!(
            bridge.per_sid.pending_admit.contains(SID),
            "the attach owes it a refill"
        );

        let mailbox = Arc::clone(&bridge.mailbox);
        let (mut human, _) = bridge.connect().unwrap();
        lane.on_first_deliver(move || {
            human
                .publish(
                    91,
                    1,
                    "/f/f/fleet/h-t/halt",
                    b"v=1 t=3 state=on reason=stop",
                )
                .unwrap();
            aterm_spec::wait::until_within(
                aterm_spec::wait::MUST_HAPPEN,
                "the fleet reader to queue the halt",
                || (mailbox.replay_pressure() == ReplayPressure::Urgent).then_some(()),
            );
        });
        assert!(bridge.attach_broker());
        let t = lane.transcript();
        let at = |line: &str| {
            t.iter()
                .position(|l| l == line)
                .unwrap_or_else(|| panic!("no `{line}` in {t:#?}"))
        };
        let held = at(&format!("hold {SID} on reason=stop origin=fleet"));
        assert!(at(&format!("deliver {one}")) < held, "{t:#?}");
        assert!(
            held < at(&format!("deliver {two}")),
            "the halt waited for the refill: {t:#?}"
        );
        assert!(bridge.halt_applied);
        assert!(
            !bridge.per_sid.pending_admit.contains(SID),
            "and the refill finished"
        );
    }

    /// A9 — WHAT THE BROKER GAVE UP UNREAD IS TOLD TO ATERM: five mails to this node, never
    /// read, then three `ev` records, all of 4 KiB; the broker's rules keep two `ev` records'
    /// bytes and three mails' bytes of unread mail below the cut, so the tick gives up the two
    /// oldest mails. Attached, the inbox group is told the range and the count before its first
    /// mail, and the bridge says `link gap from=<cursor> to=<oldest held> records=2` on the lane,
    /// once. An aterm older than the `link gap` lane is told nothing (its mail still comes).
    /// NC (run): a reader that drops the notice tells aterm nothing.
    #[cfg(unix)]
    #[test]
    fn unread_mail_the_broker_gave_up_is_told_to_aterm_as_a_link_gap() {
        use crate::broker_rules::{EV_FILTER, MAIL_FILTER, MAIL_READER};
        use astream_broker::{Retention, Rule};
        const SID: &str = "s-gap";
        const BODY: u64 = 4096;
        let run = |endpoint_lane: u32| -> (Vec<u64>, Vec<u64>, Vec<String>) {
            let (lane, mut bridge) = FakeVerbLane::bridge(&[(1, SID)]);
            let (socket, broker, _server) = serve_log(&lane, "broker");
            bridge.cfg.broker = socket;
            bridge.endpoint_lane = endpoint_lane;
            let (mut writer, _) = bridge.connect().unwrap();
            let mail = format!("/f/f/in/{}/{SID}/h-writer/note", bridge.node);
            let text = format!("v=1 t=1 text={}", "m".repeat(BODY as usize));
            let offs: Vec<u64> = (1..=5)
                .map(|seq| writer.publish(81, seq, &mail, text.as_bytes()).unwrap().0)
                .collect();
            let filler = vec![b'e'; BODY as usize];
            for seq in 1..=3 {
                writer
                    .publish(82, seq, "/f/f/pub/n-fill/s-fill/ev", &filler)
                    .unwrap();
            }
            broker.set_retention_interval(Duration::from_secs(3600));
            let rules = Retention::new(
                vec![
                    Rule::prefix(EV_FILTER).unwrap().max_bytes(5 * BODY / 2),
                    Rule::hold(MAIL_FILTER)
                        .unwrap()
                        .max_bytes(7 * BODY / 2)
                        .until_read(MAIL_READER)
                        .unwrap(),
                ],
                Rule::default_keeping_all(),
            )
            .unwrap();
            broker.set_retention(rules).unwrap();
            broker.retention_tick_at(broker.now_ms()).unwrap();
            let mails = astream_wire::Filter::new(MAIL_FILTER).unwrap();
            assert_eq!(
                broker.oldest(&mails),
                offs[2],
                "the two oldest mails given up"
            );

            assert!(bridge.refresh_sessions().is_ok());
            assert!(bridge.attach_broker());
            aterm_spec::wait::until_within(
                aterm_spec::wait::MUST_HAPPEN,
                "the bridge to deliver the three held mails",
                || {
                    assert!(bridge.round(Duration::from_millis(20)), "the lane closed");
                    (lane.delivery_offsets().len() >= 3).then_some(())
                },
            );
            let gaps = lane
                .links()
                .into_iter()
                .filter(|l| l.starts_with("link gap "))
                .map(|l| l.trim_end().to_string())
                .collect();
            let delivered = lane.delivery_offsets();
            bridge.close_subscriptions();
            (offs, delivered, gaps)
        };
        let (offs, delivered, gaps) = run(aterm_types::control_verbs::BRIDGE_LANE);
        assert_eq!(delivered, offs[2..].to_vec());
        assert_eq!(
            gaps,
            [format!(
                "link gap from={} to={} records=2",
                offs[0], offs[2]
            )],
            "told once, with the broker's count"
        );
        let (offs, delivered, gaps) = run(LINK_GAP_LANE - 1);
        assert_eq!(
            delivered,
            offs[2..].to_vec(),
            "an older aterm's mail still comes"
        );
        assert!(gaps.is_empty(), "an older aterm is told nothing: {gaps:?}");
    }

    #[cfg(unix)]
    #[test]
    fn a_replaced_log_keeps_the_halts_it_lost_until_their_human_writes_a_row() {
        const SID: &str = "s-held";
        let held = format!("hold {SID} on reason=stop origin=fleet");
        let lifted = format!("hold {SID} off origin=fleet");
        let (lane, mut bridge) = FakeVerbLane::bridge(&[(1, SID)]);
        assert!(bridge.refresh_sessions().is_ok());

        let (a, _broker_a, _server_a) = serve_log(&lane, "a");
        bridge.cfg.broker = a;
        let (mut human, _) = bridge.connect().unwrap();
        for seq in 1..=64 {
            human.publish(92, seq, "/f/f/x/history", b"v=1").unwrap();
        }
        human
            .publish(
                91,
                1,
                "/f/f/fleet/h-x/halt",
                b"v=1 t=1 state=on reason=stop",
            )
            .unwrap();
        assert!(bridge.attach_broker());
        assert!(bridge.halt_applied);
        assert!(lane.transcript().contains(&held));
        assert!(bridge.state.halt_acked().is_some(), "A's halt is answered");
        let high_on_a = bridge.state.log_witness().expect("a witness").high;

        let (b, _broker_b, _server_b) = serve_log(&lane, "b");
        bridge.close_subscriptions();
        bridge.conn = None;
        bridge.cfg.broker = b;
        let before = lane.transcript().len();
        assert!(bridge.attach_broker());
        let attach_b = lane.transcript()[before..].to_vec();
        assert!(
            !attach_b.contains(&lifted),
            "a fresh log lifted the halt: {attach_b:#?}"
        );
        assert!(bridge.halt_applied);
        assert_eq!(bridge.kept_halts.keys().collect::<Vec<_>>(), ["h-x"]);
        assert_eq!(bridge.state.halt_acked(), None, "A's watermark is not B's");
        assert!(bridge.state.log_witness().expect("re-based").high < high_on_a);
        let evs = node_evs(&bridge);
        assert!(
            evs.iter().any(|e| e.contains("ev=log-replaced head="))
                && evs.iter().any(|e| e.contains("ev=halt-kept by=h-x")),
            "{evs:#?}"
        );

        bridge.halts.clear();
        bridge.kept_halts.clear();
        bridge.halt_applied = false;
        bridge.close_subscriptions();
        bridge.conn = None;
        assert!(bridge.attach_broker());
        assert!(bridge.halt_applied, "the halt went with the memory");
        assert_eq!(lane.transcript().last(), Some(&held));

        let (mut human_b, _) = bridge.connect().unwrap();
        let body = b"v=1 t=2 state=off";
        let (off, _) = human_b.publish(91, 1, "/f/f/fleet/h-x/halt", body).unwrap();
        bridge.on_fleet_record(&(off, "/f/f/fleet/h-x/halt".to_string(), body.to_vec()));
        assert!(!bridge.halt_applied);
        assert_eq!(lane.transcript().last(), Some(&lifted));
        assert!(bridge.kept_halts.is_empty());
        assert!(bridge.state.standing_halts("f").is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn a_log_with_another_identity_has_every_old_offset_forgotten_however_long_it_is() {
        const SID: &str = "s-moved";
        let (lane, mut bridge) = FakeVerbLane::bridge(&[(1, SID)]);
        assert!(bridge.refresh_sessions().is_ok());
        bridge.endpoint_lane = aterm_types::control_verbs::BRIDGE_LANE;
        let halt = "/f/f/fleet/h-x/halt";

        let (a, broker_a, _server_a) = serve_log(&lane, "a");
        bridge.cfg.broker = a;
        let (mut human, _) = bridge.connect().unwrap();
        for seq in 1..=8 {
            human.publish(92, seq, "/f/f/x/history", b"v=1").unwrap();
        }
        let (a_halt, _) = human
            .publish(91, 1, halt, b"v=1 t=1 state=on reason=stop")
            .unwrap();
        assert!(bridge.attach_broker());
        assert_eq!(
            bridge.state.halt_acked(),
            Some(a_halt),
            "A's halt is answered"
        );
        let a_id = broker_a.log_identity();
        bridge.state.set_seen_off(SID, a_halt).unwrap();
        lane.opt_in(SID, &["topic build since=head"]);
        let topic = BTreeMap::from([("build".to_string(), TopicCursor { next: a_halt })]);
        bridge.state.set_topics(SID, &topic).unwrap();
        bridge.per_sid.topics.insert(SID.to_string(), topic);
        bridge.note_asked(a_halt, "n-peer");
        bridge.note_deadline(SID, a_halt, 60_000, Some("n-peer".to_string()));
        assert!(!bridge.deadlines.is_empty() && !bridge.state.asked().is_empty());

        let (b, broker_b, _server_b) = serve_log(&lane, "b");
        let b_id = broker_b.log_identity();
        assert_ne!(a_id, b_id);
        bridge.close_subscriptions();
        bridge.conn = None;
        bridge.cfg.broker = b;
        let (mut human_b, _) = bridge.connect().unwrap();
        let (b_halt, _) = human_b
            .publish(
                93,
                1,
                "/f/f/fleet/h-y/halt",
                b"v=1 t=2 state=on reason=again",
            )
            .unwrap();
        assert!(b_halt < a_halt);
        for seq in 1..=64 {
            human_b.publish(92, seq, "/f/f/x/history", b"v=1").unwrap();
        }
        let (_, (_, b_head)) = human_b.fetch(0, "/f/f/>", 0).unwrap();
        assert!(
            b_head > bridge.state.log_witness().expect("A's witness").high,
            "B's head alone cannot say it is another log"
        );
        assert!(bridge.attach_broker());

        assert_eq!(
            bridge.state.halt_acked(),
            Some(b_halt),
            "B's halt is answered"
        );
        let (mut reader, _) = bridge.connect().unwrap();
        let ack = subject::node_face("f", &bridge.node, "ack");
        let (rows, _) = reader.last(&ack, "", 8).unwrap();
        assert!(
            rows.iter().any(|(_, s, b)| *s == ack
                && String::from_utf8_lossy(b).contains(&format!(" re={b_halt} "))),
            "no ack of B's halt on B: {rows:?}"
        );
        assert_eq!(bridge.state.seen_off(SID), None, "A's watermark is not B's");
        let resumed = bridge.state.topics(SID);
        assert!(
            resumed.get("build").is_some_and(|c| c.next >= b_head)
                && bridge.per_sid.topics[SID]["build"].next >= b_head,
            "{resumed:?}"
        );
        assert!(
            bridge.self_acked.first().is_some_and(|w| *w + 1 >= b_head),
            "the self-lane window starts at B's head: {:?}",
            bridge.self_acked
        );
        assert!(bridge.deadlines.is_empty() && bridge.asked.is_empty());
        assert!(bridge.state.deadlines().is_empty() && bridge.state.asked().is_empty());

        let links = lane.links.lock().unwrap().clone();
        let told = |id: astream_broker::LogIdentity| {
            links.iter().position(|l| *l == format!("link log {id}"))
        };
        assert!(
            told(a_id) < told(b_id) && told(b_id).is_some(),
            "{links:#?}"
        );
        let evs = node_evs(&bridge);
        assert!(
            evs.iter().any(|e| e.contains("ev=log-replaced head=")
                && e.contains(&format!(" log={b_id} was_log={a_id}"))
                && e.contains(" deadlines=1")),
            "{evs:#?}"
        );
        let witness = bridge.state.log_witness().expect("re-based");
        assert_eq!(witness.id, Some(b_id));
        assert!(witness.high >= b_head);
        assert!(bridge.log_replaced.is_none(), "the re-base is finished");

        bridge.state.set_seen_off(SID, b_halt).unwrap();
        bridge.close_subscriptions();
        bridge.conn = None;
        assert!(bridge.attach_broker());
        assert_eq!(bridge.state.seen_off(SID), Some(b_halt));

        let (c, broker_c, _server_c) = serve_log(&lane, "c");
        let witness = bridge.state.root().join("bus-log");
        std::fs::remove_file(&witness).unwrap();
        std::fs::create_dir(&witness).unwrap();
        bridge.close_subscriptions();
        bridge.conn = None;
        bridge.cfg.broker = c;
        assert!(
            !bridge.attach_broker(),
            "no attach past an unwritten witness"
        );
        assert!(bridge.conn.is_none(), "and no connection is kept");
        assert!(bridge.log_replaced.is_some(), "the re-base is owed");
        bridge.state.set_seen_off(SID, 1).unwrap();
        std::fs::remove_dir(&witness).unwrap();
        bridge.close_subscriptions();
        bridge.conn = None;
        assert!(bridge.attach_broker());
        assert_eq!(bridge.state.seen_off(SID), Some(1), "C's own offset stands");
        assert!(bridge.log_replaced.is_none(), "and the re-base is finished");
        assert_eq!(
            bridge.state.log_witness().expect("written").id,
            Some(broker_c.log_identity())
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_known_log_identity_that_rewinds_is_refused_before_any_clock_mutation() {
        const SID: &str = "s-kept";
        let (lane, mut bridge) = FakeVerbLane::bridge(&[(1, SID)]);
        lane.answer_hello_with(&format!("OK lane={LINK_LOG_LANE} build=test\n"));
        bridge.begin_run();
        bridge.introduce();
        assert!(bridge.refresh_sessions().is_ok());
        let (socket, broker, _server) = serve_log(&lane, "rewound");
        bridge.cfg.broker = socket.clone();
        let (mut writer, _) = bridge.connect().unwrap();
        for seq in 1..=10 {
            writer.publish(81, seq, "/f/f/x/history", b"v=1").unwrap();
        }
        assert_eq!(broker.head(), 10, "the actual broker head");
        let log = writer.log_identity().unwrap().unwrap();
        assert_eq!(log, broker.log_identity(), "the actual publisher identity");

        let witnessed = LogWitness {
            high: 20,
            id: Some(log),
        };
        bridge.state.set_log_witness(&witnessed).unwrap();
        bridge.log_witness = witnessed;
        bridge.bus_log = Some(log);
        bridge.bus_high = 20;
        bridge.state.set_seen_off(SID, 17).unwrap();
        bridge
            .state
            .set_topics(
                SID,
                &BTreeMap::from([("desk".into(), TopicCursor { next: 18 })]),
            )
            .unwrap();
        let subject = format!("/f/f/in/{}/{SID}/{}/ack", bridge.node, bridge.node);
        let body = b"v=1 t=1 re=15 verdict=handled";
        let mut pin = plain_pin(bridge.next_seq().unwrap(), &subject, body);
        pin.original_off = Some(15);
        bridge.state.set_key_pin(SID, "kept", &pin).unwrap();
        bridge.state.set_post_pin(SID, 1, &pin).unwrap();
        bridge
            .state
            .set_in_flight(&InFlight {
                pin: pin.clone(),
                subject,
                body: body.to_vec(),
                log: Some(log),
            })
            .unwrap();
        let files = |root: &std::path::Path| {
            let mut todo = vec![root.to_path_buf()];
            let mut files = BTreeMap::new();
            while let Some(dir) = todo.pop() {
                for entry in std::fs::read_dir(dir).unwrap() {
                    let entry = entry.unwrap();
                    if entry.file_type().unwrap().is_dir() {
                        todo.push(entry.path());
                    } else {
                        // The only new facts a refusal may record: that this
                        // identity can no longer vouch for its old offsets, and
                        // the run's OPEN fence the attach's admission writes
                        // before any broker exchange (768104f73e).
                        if entry.path() != root.join("rewound-log")
                            && entry.path() != root.join("run")
                        {
                            files.insert(entry.path(), std::fs::read(entry.path()).unwrap());
                        }
                    }
                }
            }
            files
        };
        let before = files(bridge.state.root());
        let start = lane.links.lock().unwrap().len();
        assert!(
            !bridge.attach_broker(),
            "the same known identity rewound from 20 to 10"
        );
        assert_eq!(
            broker.head(),
            10,
            "no presence or old reference was published"
        );
        assert_eq!(files(bridge.state.root()), before, "all durable facts stay");
        let run = std::fs::read_to_string(bridge.state.root().join("run")).unwrap();
        assert!(
            run.starts_with("open "),
            "the run file is its OPEN fence: {run:?}"
        );
        assert_eq!(bridge.log_witness, witnessed, "the high-water is retained");
        assert!(
            lane.delivery_offsets().is_empty(),
            "no refill was delivered"
        );
        assert!(
            !lane.links.lock().unwrap()[start..]
                .iter()
                .any(|line| line.starts_with("link log ")),
            "no endpoint clock transition was acknowledged"
        );

        for seq in 11..=20 {
            writer.publish(81, seq, "/f/f/x/history", b"v=1").unwrap();
        }
        assert_eq!(broker.head(), 20);
        assert!(!bridge.attach_broker(), "regrowth does not repair identity");
        assert_eq!(broker.head(), 20, "regrowth publishes no recovery rows");
        assert_eq!(files(bridge.state.root()), before);
        let state_dir = bridge.state.root().to_path_buf();
        drop(bridge);
        let (next_lane, mut bridge) =
            FakeVerbLane::bridge_with_state(&[(1, SID)], Some(&state_dir));
        next_lane.answer_hello_with(&format!("OK lane={LINK_LOG_LANE} build=test\n"));
        bridge.introduce();
        assert!(bridge.refresh_sessions().is_ok());
        bridge.cfg.broker = socket;
        assert!(
            !bridge.attach_broker(),
            "a restarted bridge retains the fence"
        );
        assert_eq!(broker.head(), 20, "the restarted bridge publishes nothing");
        assert_eq!(files(bridge.state.root()), before);
        assert_eq!(bridge.state.log_witness(), Some(witnessed));
        assert!(next_lane
            .links
            .lock()
            .unwrap()
            .iter()
            .all(|line| !line.starts_with("link log ")));

        let (new_socket, new_broker, _new_server) = serve_log(&lane, "new-clock");
        assert_ne!(new_broker.log_identity(), log);
        bridge.cfg.broker = new_socket;
        assert!(
            bridge.attach_broker(),
            "a different known identity can rebase"
        );
        assert!(bridge.state.in_flight().unwrap().is_empty());
        assert_eq!(
            bridge
                .state
                .key_pin(SID, "kept")
                .unwrap()
                .unwrap()
                .original_off,
            None,
            "the old offset loses its authority on the new log"
        );
        assert_eq!(
            bridge.state.log_witness().unwrap().id,
            Some(new_broker.log_identity())
        );
        assert!(
            !state_dir.join("rewound-log").exists(),
            "a witnessed new identity releases the fence"
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_power_loss_under_a_relaxed_broker_continues_the_log_at_the_next_boot() {
        use crate::durability::{self, Durability};
        const SID: &str = "s-kept";
        let cases = [
            (true, false, true),
            (true, true, true),
            (true, false, false),
            (true, true, false),
            (false, false, true),
            (false, true, true),
        ];
        for (reboot, regrow, lineage) in cases {
            let (lane, mut bridge) = FakeVerbLane::bridge(&[(1, SID)]);
            lane.answer_hello_with(&format!("OK lane={LINK_LOG_LANE} build=test\n"));
            bridge.introduce();
            assert!(bridge.refresh_sessions().is_ok());
            bridge.ignore_lineage = !lineage;
            let log = lane.dir.path().join("relaxed.log");
            let socket = lane.dir.path().join("relaxed.sock");
            bridge.cfg.broker = socket.to_string_lossy().into_owned();
            let case = format!("reboot={reboot} regrow={regrow} lineage={lineage}");

            let (before, high, cut) = {
                let opened = durability::open(
                    &log,
                    None,
                    Durability::Relaxed,
                    Some(durability::test_boot(1)),
                )
                .unwrap();
                opened.broker.arm_unclean_stop();
                let mut server = opened.broker.serve(&socket).unwrap();
                let (mut writer, _) = bridge.connect().unwrap();
                for seq in 1..=4 {
                    writer.publish(81, seq, "/f/f/x/history", b"v=1").unwrap();
                }
                let cut = std::fs::metadata(&log).unwrap().len();
                for seq in 5..=10 {
                    writer.publish(81, seq, "/f/f/x/history", b"v=1").unwrap();
                }
                lane.opt_in(SID, &["topic t since=head"]);
                assert!(bridge.attach_broker(), "{case}: the first attach");
                assert!(bridge.sync_say_face(), "{case}");
                let high = bridge.state.log_witness().expect("witnessed").high;
                assert!(high >= 10, "{case}: {high}");
                assert!(bridge.per_sid.topics[SID]["t"].next >= 10, "{case}");
                bridge.state.set_seen_off(SID, high - 1).unwrap();
                drop(writer);
                bridge.close_subscriptions();
                bridge.conn = None;
                server.shutdown();
                (opened.broker.log_identity(), high, cut)
            };
            std::fs::OpenOptions::new()
                .write(true)
                .open(&log)
                .unwrap()
                .set_len(cut)
                .unwrap();

            let boot = durability::test_boot(if reboot { 2 } else { 1 });
            let reopened = durability::open(&log, None, Durability::Relaxed, Some(boot)).unwrap();
            assert_eq!(reopened.broker.head(), 4, "{case}: a clean prefix");
            let _server = reopened.broker.serve(&socket).unwrap();
            let body = Body::new(0).encode();
            let (mut other, _) = bridge.connect().unwrap();
            let (at_fork, _) = other
                .publish(82, 1, "/f/f/pub/n-src/s-src/say/t", &body)
                .unwrap();
            assert_eq!(
                at_fork, 4,
                "{case}: the broadcast takes the first offset given up"
            );
            let mut seq = 2;
            if regrow {
                while reopened.broker.head() <= high {
                    other.publish(82, seq, "/f/f/x/history", b"v=1").unwrap();
                    seq += 1;
                }
            }
            bridge.conn = None;
            let now = reopened.broker.log_identity();
            if reboot {
                assert_eq!(
                    (now.id, now.generation),
                    (before.id, before.generation + 1),
                    "{case}: the next generation of the same log"
                );
                assert!(
                    bridge.attach_broker(),
                    "{case}: a moved generation, attached"
                );
                assert_eq!(bridge.state.log_witness().unwrap().id, Some(now), "{case}");
                assert_eq!(bridge.state.rewound_log().unwrap(), None, "{case}");
                assert!(bridge.sync_say_face(), "{case}");
                let (sentinel, _) = other
                    .publish(82, seq, "/f/f/pub/n-src/s-src/say/t", &body)
                    .unwrap();
                let first = fan_in_next_say(&mut bridge);
                if lineage {
                    assert_eq!(
                        bridge.state.seen_off(SID),
                        Some(3),
                        "{case}: clamped to the fork"
                    );
                    assert_eq!(
                        first, at_fork,
                        "{case}: the broadcast at the fork is delivered"
                    );
                    assert_eq!(fan_in_next_say(&mut bridge), sentinel, "{case}");
                } else {
                    assert_eq!(bridge.state.seen_off(SID), None, "{case}: forgotten");
                    assert_eq!(
                        first, sentinel,
                        "{case}: CONTROL: re-based at the head, the broadcast at the fork is skipped"
                    );
                }
                bridge.close_subscriptions();
            } else if regrow {
                assert_eq!(now, before, "{case}: the same identity");
                assert!(bridge.attach_broker(), "{case}: nothing to tell it by");
                assert_eq!(
                    bridge.state.seen_off(SID),
                    Some(high - 1),
                    "{case}: the stale watermark survives — the hazard the generation move closes"
                );
                bridge.close_subscriptions();
            } else {
                assert_eq!(now, before, "{case}: the same identity");
                assert!(!bridge.attach_broker(), "{case}: refused as rewound");
                assert_eq!(bridge.state.rewound_log().unwrap(), Some(before), "{case}");
            }
        }
    }

    #[cfg(unix)]
    #[test]
    fn a_moved_generation_of_a_log_refused_as_rewound_continues_nothing() {
        use crate::durability::{self, Durability};
        const SID: &str = "s-kept";
        let (lane, mut bridge) = FakeVerbLane::bridge(&[(1, SID)]);
        lane.answer_hello_with(&format!("OK lane={LINK_LOG_LANE} build=test\n"));
        bridge.introduce();
        assert!(bridge.refresh_sessions().is_ok());
        let log = lane.dir.path().join("relaxed.log");
        let socket = lane.dir.path().join("relaxed.sock");
        bridge.cfg.broker = socket.to_string_lossy().into_owned();

        let backup;
        let (before, high, cut) = {
            let opened = durability::open(
                &log,
                None,
                Durability::Relaxed,
                Some(durability::test_boot(1)),
            )
            .unwrap();
            let mut server = opened.broker.serve(&socket).unwrap();
            let (mut writer, _) = bridge.connect().unwrap();
            for seq in 1..=4 {
                writer.publish(81, seq, "/f/f/x/history", b"v=1").unwrap();
            }
            let cut = std::fs::metadata(&log).unwrap().len();
            backup = std::fs::read(astream_broker::identity_path(&log)).unwrap();
            for seq in 5..=10 {
                writer.publish(81, seq, "/f/f/x/history", b"v=1").unwrap();
            }
            lane.opt_in(SID, &["topic t since=head"]);
            assert!(bridge.attach_broker(), "the first attach");
            assert!(bridge.sync_say_face());
            let high = bridge.state.log_witness().expect("witnessed").high;
            assert!(high >= 10, "{high}");
            bridge.state.set_seen_off(SID, high - 1).unwrap();
            drop(writer);
            bridge.close_subscriptions();
            bridge.conn = None;
            server.shutdown();
            (opened.broker.log_identity(), high, cut)
        };
        std::fs::write(astream_broker::identity_path(&log), &backup).unwrap();
        std::fs::OpenOptions::new()
            .write(true)
            .open(&log)
            .unwrap()
            .set_len(cut)
            .unwrap();
        {
            let restored = durability::open(
                &log,
                None,
                Durability::Relaxed,
                Some(durability::test_boot(1)),
            )
            .unwrap();
            restored.broker.arm_unclean_stop();
            assert_eq!(
                restored.broker.log_identity(),
                before,
                "no move astream can see"
            );
            assert_eq!(restored.broker.head(), 4);
            let mut server = restored.broker.serve(&socket).unwrap();
            assert!(!bridge.attach_broker(), "refused as rewound");
            assert_eq!(bridge.state.rewound_log().unwrap(), Some(before));
            let (mut other, _) = bridge.connect().unwrap();
            let mut seq = 1;
            while restored.broker.head() <= high {
                other.publish(82, seq, "/f/f/x/history", b"v=1").unwrap();
                seq += 1;
            }
            drop(other);
            bridge.conn = None;
            server.shutdown();
        }
        let moved = durability::open(
            &log,
            None,
            Durability::Relaxed,
            Some(durability::test_boot(2)),
        )
        .unwrap();
        let now = moved.broker.log_identity();
        assert_eq!(
            (now.id, now.generation),
            (before.id, before.generation + 1),
            "the next generation of the restored log"
        );
        let fork = moved.broker.head();
        assert!(
            fork > high,
            "regrown past what this node saw: {fork} > {high}"
        );
        let _server = moved.broker.serve(&socket).unwrap();
        {
            let mut client = astream_broker::Client::connect(&socket).unwrap();
            assert_eq!(client.log_identity().unwrap(), Some(now));
            let lineage = client
                .log_lineage()
                .cloned()
                .expect("the Hello carries lineage");
            assert_eq!(
                lineage.steps().last(),
                Some(&(now.generation, fork)),
                "astream continues the regrown log below its head"
            );
        }
        assert!(
            bridge.attach_broker(),
            "another known identity lifts the refusal"
        );
        assert_eq!(bridge.state.rewound_log().unwrap(), None);
        assert_eq!(bridge.state.log_witness().unwrap().id, Some(now));
        assert_eq!(
            bridge.state.seen_off(SID),
            None,
            "forgotten: a clamp at {fork} would keep {} and skip the records posted after the \
             restore",
            high - 1
        );
        assert_eq!(
            bridge.per_sid.topics[SID]["t"].next, fork,
            "the topic re-based at the head, not resumed at a cursor the restore made a lie"
        );
        bridge.close_subscriptions();
    }

    #[cfg(unix)]
    #[test]
    fn an_in_flight_reply_below_the_fork_is_published_on_the_moved_generation() {
        use crate::durability::{self, Durability};
        const SID: &str = "s-kept";
        for lineage in [true, false] {
            let (lane, mut bridge) = FakeVerbLane::bridge(&[(1, SID)]);
            lane.answer_hello_with(&format!("OK lane={LINK_LOG_LANE} build=test\n"));
            bridge.introduce();
            assert!(bridge.refresh_sessions().is_ok());
            bridge.ignore_lineage = !lineage;
            let log = lane.dir.path().join("relaxed.log");
            let socket = lane.dir.path().join("relaxed.sock");
            bridge.cfg.broker = socket.to_string_lossy().into_owned();
            let subject = format!("/f/f/in/{}/{SID}/{}/ack", bridge.node, bridge.node);
            let body = b"v=1 t=1 re=1 verdict=handled".to_vec();

            let cut = {
                let opened = durability::open(
                    &log,
                    None,
                    Durability::Relaxed,
                    Some(durability::test_boot(1)),
                )
                .unwrap();
                opened.broker.arm_unclean_stop();
                let mut server = opened.broker.serve(&socket).unwrap();
                let (mut writer, _) = bridge.connect().unwrap();
                for seq in 1..=4 {
                    writer.publish(81, seq, "/f/f/x/history", b"v=1").unwrap();
                }
                let cut = std::fs::metadata(&log).unwrap().len();
                for seq in 5..=10 {
                    writer.publish(81, seq, "/f/f/x/history", b"v=1").unwrap();
                }
                assert!(
                    bridge.attach_broker(),
                    "lineage={lineage}: the first attach"
                );
                let pin = plain_pin(bridge.next_seq().unwrap(), &subject, &body);
                bridge
                    .state
                    .set_in_flight(&InFlight {
                        pin,
                        subject: subject.clone(),
                        body: body.clone(),
                        log: Some(opened.broker.log_identity()),
                    })
                    .unwrap();
                drop(writer);
                bridge.close_subscriptions();
                bridge.conn = None;
                server.shutdown();
                cut
            };
            std::fs::OpenOptions::new()
                .write(true)
                .open(&log)
                .unwrap()
                .set_len(cut)
                .unwrap();
            let reopened = durability::open(
                &log,
                None,
                Durability::Relaxed,
                Some(durability::test_boot(2)),
            )
            .unwrap();
            assert_eq!(reopened.broker.head(), 4, "lineage={lineage}");
            let _server = reopened.broker.serve(&socket).unwrap();
            assert!(
                bridge.attach_broker(),
                "lineage={lineage}: the moved generation"
            );
            assert!(
                bridge.state.in_flight().unwrap().is_empty(),
                "lineage={lineage}: settled either way"
            );
            let (mut reader, _) = bridge.connect().unwrap();
            let (rows, _) = reader.fetch(0, &subject, 64).unwrap();
            let replies: Vec<u64> = rows
                .into_iter()
                .filter(|(_, _, b)| b.as_slice() == body.as_slice())
                .map(|(off, _, _)| off)
                .collect();
            if lineage {
                assert_eq!(
                    replies.len(),
                    1,
                    "the reply to offset 1, below the fork, is published once: {replies:?}"
                );
                assert!(replies[0] >= 4, "on the new generation: {replies:?}");
            } else {
                assert!(
                    replies.is_empty(),
                    "CONTROL: another log drops the reply: {replies:?}"
                );
            }
            bridge.close_subscriptions();
        }
    }

    #[cfg(unix)]
    #[test]
    fn a_moved_generation_is_told_to_a_lane_3_endpoint_with_its_fork() {
        use crate::durability::{self, Durability};
        const SID: &str = "s-kept";
        for (endpoint_lane, lineage, refuse) in [
            (LINK_CONTINUES_LANE, true, false),
            (LINK_LOG_LANE, true, false),
            (LINK_CONTINUES_LANE, false, false),
            (LINK_CONTINUES_LANE, true, true),
        ] {
            let case = format!("lane={endpoint_lane} lineage={lineage} refuse={refuse}");
            let (lane, mut bridge) = FakeVerbLane::bridge(&[(1, SID)]);
            lane.answer_hello_with(&format!("OK lane={endpoint_lane} build=test\n"));
            bridge.introduce();
            assert_eq!(bridge.endpoint_lane, endpoint_lane, "{case}");
            assert!(bridge.refresh_sessions().is_ok());
            bridge.ignore_lineage = !lineage;
            let log = lane.dir.path().join("relaxed.log");
            let socket = lane.dir.path().join("relaxed.sock");
            bridge.cfg.broker = socket.to_string_lossy().into_owned();

            let (before, cut) = {
                let opened = durability::open(
                    &log,
                    None,
                    Durability::Relaxed,
                    Some(durability::test_boot(1)),
                )
                .unwrap();
                opened.broker.arm_unclean_stop();
                let mut server = opened.broker.serve(&socket).unwrap();
                let (mut writer, _) = bridge.connect().unwrap();
                for seq in 1..=4 {
                    writer.publish(81, seq, "/f/f/x/history", b"v=1").unwrap();
                }
                let cut = std::fs::metadata(&log).unwrap().len();
                for seq in 5..=10 {
                    writer.publish(81, seq, "/f/f/x/history", b"v=1").unwrap();
                }
                assert!(bridge.attach_broker(), "{case}: the first attach");
                bridge.state.set_seen_off(SID, 9).unwrap();
                drop(writer);
                bridge.close_subscriptions();
                bridge.conn = None;
                server.shutdown();
                (opened.broker.log_identity(), cut)
            };
            std::fs::OpenOptions::new()
                .write(true)
                .open(&log)
                .unwrap()
                .set_len(cut)
                .unwrap();
            let reopened = durability::open(
                &log,
                None,
                Durability::Relaxed,
                Some(durability::test_boot(2)),
            )
            .unwrap();
            assert_eq!(reopened.broker.head(), 4, "{case}");
            let now = reopened.broker.log_identity();
            assert_eq!(
                (now.id, now.generation),
                (before.id, before.generation + 1),
                "{case}: the next generation of the same log"
            );
            let _server = reopened.broker.serve(&socket).unwrap();
            if refuse {
                *lane.log_answer.lock().unwrap() = "ERR usage: link log <identity>\n".to_string();
            }
            let attached = bridge.attach_broker();
            let told = lane
                .links
                .lock()
                .unwrap()
                .iter()
                .rev()
                .find(|l| l.starts_with("link log "))
                .cloned();
            let continued = format!("link log {now} continues=4 was={before}");
            if refuse {
                assert!(!attached, "{case}: a refused move fails the attach");
                assert_eq!(told, Some(continued), "{case}");
                assert_eq!(
                    bridge.state.seen_off(SID),
                    Some(9),
                    "{case}: nothing moved before the endpoint took the move"
                );
                assert_eq!(
                    bridge.state.log_witness().unwrap().id,
                    Some(before),
                    "{case}"
                );
                continue;
            }
            assert!(attached, "{case}: the moved generation, attached");
            let expected = if endpoint_lane >= LINK_CONTINUES_LANE && lineage {
                continued
            } else {
                format!("link log {now}")
            };
            assert_eq!(told, Some(expected), "{case}");
            assert_eq!(
                bridge.state.seen_off(SID),
                if lineage { Some(3) } else { None },
                "{case}: the bridge clamps at the fork it told"
            );
            bridge.close_subscriptions();
        }
    }

    #[cfg(unix)]
    #[test]
    fn a_clamp_clears_halt_acked_and_the_halt_is_answered_again() {
        use crate::durability::{self, Durability};
        const SID: &str = "s-held";
        let (lane, mut bridge) = FakeVerbLane::bridge(&[(1, SID)]);
        assert!(bridge.refresh_sessions().is_ok());
        let log = lane.dir.path().join("relaxed.log");
        let socket = lane.dir.path().join("relaxed.sock");
        bridge.cfg.broker = socket.to_string_lossy().into_owned();
        let node = bridge.node.clone();
        let acks_of = |sock: &std::path::Path| -> Vec<String> {
            let (mut c, _) = transport::connect(&sock.to_string_lossy(), ACK_DEADLINE).unwrap();
            let (rows, _) = c
                .fetch(0, &subject::node_face("f", &node, "ack"), 64)
                .unwrap();
            rows.into_iter()
                .map(|(_, _, body)| String::from_utf8_lossy(&body).into_owned())
                .filter(|b| b.contains(" re=0 "))
                .collect()
        };
        let cut = {
            let opened = durability::open(
                &log,
                None,
                Durability::Relaxed,
                Some(durability::test_boot(1)),
            )
            .unwrap();
            opened.broker.arm_unclean_stop();
            let mut server = opened.broker.serve(&socket).unwrap();
            let (mut human, _) = bridge.connect().unwrap();
            let (halt, _) = human
                .publish(
                    91,
                    1,
                    "/f/f/fleet/h-x/halt",
                    b"v=1 t=1 state=on reason=stop",
                )
                .unwrap();
            assert_eq!(halt, 0);
            let cut = std::fs::metadata(&log).unwrap().len();
            assert!(bridge.attach_broker());
            assert!(bridge.halt_applied);
            assert_eq!(
                bridge.state.halt_acked(),
                Some(0),
                "answered on boot 1's log"
            );
            assert_eq!(acks_of(&socket).len(), 1);
            drop(human);
            bridge.close_subscriptions();
            bridge.conn = None;
            server.shutdown();
            cut
        };
        std::fs::OpenOptions::new()
            .write(true)
            .open(&log)
            .unwrap()
            .set_len(cut)
            .unwrap();
        let reopened = durability::open(
            &log,
            None,
            Durability::Relaxed,
            Some(durability::test_boot(2)),
        )
        .unwrap();
        assert_eq!(
            reopened.broker.head(),
            1,
            "the halt survived, its ack did not"
        );
        let _server = reopened.broker.serve(&socket).unwrap();
        assert!(acks_of(&socket).is_empty());
        assert!(bridge.attach_broker(), "the moved generation, clamped at 1");
        assert!(bridge.halt_applied, "the halt still holds");
        assert_eq!(bridge.state.halt_acked(), Some(0), "answered again");
        assert_eq!(acks_of(&socket).len(), 1, "one ack on the new generation");

        bridge.close_subscriptions();
        bridge.conn = None;
        assert!(bridge.attach_broker());
        assert_eq!(acks_of(&socket).len(), 1, "a reattach answers nothing more");
        bridge.close_subscriptions();
    }

    #[cfg(unix)]
    #[test]
    fn a_log_switch_drops_old_commit_floors_but_keeps_the_session_fence() {
        const HELD: &str = "s-held";
        const HEALTHY: &str = "s-healthy";
        let (lane, mut bridge) = FakeVerbLane::bridge(&[(1, HELD), (2, HEALTHY)]);
        assert!(bridge.refresh_sessions().is_ok());
        let (a, _broker_a, _server_a) = serve_log(&lane, "debt-a");
        bridge.cfg.broker = a;
        assert!(bridge.attach_broker());
        let node = bridge.node.clone();
        let face = |sid: &str| format!("/f/f/in/{node}/{sid}/n-peer/note");
        let body = b"v=1 t=1 from=s-peer text=mail";
        let (mut writer, _) = bridge.connect().unwrap();
        let (old, _) = writer.publish(81, 1, &face(HELD), body).unwrap();
        lane.answer_deliveries_to(HELD, SHIPPED_DELIVER_USAGE);
        bridge.on_inbox_record(&(old, face(HELD), body.to_vec()));
        bridge.commit_floor = Some(old);
        bridge.conn = None;
        assert!(bridge.attach_broker());
        assert_eq!(
            bridge.inbox_commit_floor(),
            Some(old),
            "same-log debt stays"
        );

        let (b, _broker_b, _server_b) = serve_log(&lane, "debt-b");
        bridge.close_subscriptions();
        bridge.conn = None;
        bridge.cfg.broker = b;
        let (mut writer, _) = bridge.connect().unwrap();
        for seq in 1..=32 {
            writer.publish(82, seq, "/f/f/x/history", b"v=1").unwrap();
        }
        assert!(bridge.attach_broker());
        let (healthy, _) = writer.publish(82, 33, &face(HEALTHY), body).unwrap();
        bridge.on_inbox_record(&(healthy, face(HEALTHY), body.to_vec()));
        assert_eq!(bridge.committed, healthy, "A's debt cannot clamp B's group");
        assert_eq!(
            bridge.per_sid.lane_owed.get(HELD),
            Some(&None),
            "grammar fence stays"
        );

        let delivered = lane.delivery_offsets();
        let (owed, _) = writer.publish(82, 34, &face(HELD), body).unwrap();
        bridge.on_inbox_record(&(owed, face(HELD), body.to_vec()));
        let (later, _) = writer.publish(82, 35, &face(HEALTHY), body).unwrap();
        bridge.on_inbox_record(&(later, face(HEALTHY), body.to_vec()));
        assert_eq!(bridge.committed, owed - 1, "B's new owed row bounds B");
        assert_eq!(bridge.per_sid.lane_owed[HELD], Some(owed));
        assert_eq!(lane.delivery_offsets(), [delivered, vec![later]].concat());
    }

    fn log_head(reader: &mut Conn) -> u64 {
        reader.fetch(0, "/f/>", 0).expect("a head query").1 .1
    }

    fn run_rounds(bridge: &mut Bridge) {
        loop {
            let idle = bridge.mailbox.is_idle();
            assert!(bridge.round(Duration::ZERO), "aterm did not close the lane");
            if idle {
                break;
            }
        }
    }

    fn queue_inbox(bridge: &Bridge, records: &[BrokerRecord]) {
        let generation = bridge.mailbox.broker_generation(Source::Inbox);
        for rec in records {
            assert_eq!(
                bridge.mailbox.push(Source::Inbox, rec.clone(), generation),
                Pushed::Queued
            );
        }
    }

    fn burst_rig(
        lane: &FakeVerbLane,
        bridge: &mut Bridge,
        sid: &str,
        n: u64,
    ) -> (
        Vec<BrokerRecord>,
        Conn,
        String,
        astream_broker::Broker,
        astream_broker::BrokerHandle,
    ) {
        let (socket, broker, server) = serve_log(lane, "burst");
        bridge.cfg.broker = socket.clone();
        bridge.conn = Some(bridge.connect().unwrap().0.into());
        assert!(bridge.refresh_sessions().is_ok());
        bridge.per_sid.pending_admit.clear();
        let face = format!("/f/f/in/{}/{sid}/n-peer/note", bridge.node);
        let body = b"v=1 t=1 from=s-peer text=burst".to_vec();
        let (mut writer, _) = bridge.connect().unwrap();
        let records = (1..=n)
            .map(|seq| {
                let (off, _) = writer.publish(91, seq, &face, &body).unwrap();
                (off, face.clone(), body.clone())
            })
            .collect();
        (records, writer, socket, broker, server)
    }

    #[cfg(unix)]
    #[test]
    fn a_refill_below_the_retained_floor_reads_on_from_it() {
        const SID: &str = "s-kept";
        let (lane, mut bridge) = FakeVerbLane::bridge(&[(1, SID)]);
        let (records, mut reader, _socket, broker, _server) = burst_rig(&lane, &mut bridge, SID, 3);
        let offs: Vec<u64> = records.iter().map(|r| r.0).collect();
        broker
            .retain_before_carrying_state(offs[1])
            .expect("the cut");
        let filter = format!("/f/f/in/{}/{SID}/>", bridge.node);
        let refused = reader
            .fetch(offs[0], &filter, 8)
            .expect_err("PRECONDITION: a plain read below the floor is refused");
        assert_eq!(transport::retained_past(&refused, offs[0]), Some(offs[1]));
        assert_eq!(
            bridge.state.seen_off(SID),
            None,
            "PRECONDITION: no `seen`, so the refill reads from its floor span, offset 0"
        );

        assert!(bridge.refill(SID), "the refill reads on");
        assert_eq!(
            lane.delivery_offsets(),
            offs[1..],
            "the kept records, in order"
        );
        assert!(bridge.conn.is_some(), "and the link stays up");
    }

    #[cfg(unix)]
    #[test]
    fn an_inbox_burst_costs_one_commit_record_and_one_record_at_a_time_costs_one_each() {
        const SID: &str = "s-burst";
        let (lane, mut bridge) = FakeVerbLane::bridge(&[(1, SID)]);
        let (records, mut reader, _socket, _broker, _server) =
            burst_rig(&lane, &mut bridge, SID, 32);
        let (burst, singles) = records.split_at(16);

        let before = log_head(&mut reader);
        queue_inbox(&bridge, burst);
        run_rounds(&mut bridge);
        let offs: Vec<u64> = burst.iter().map(|r| r.0).collect();
        assert_eq!(lane.delivery_offsets(), offs, "every record, in order");
        assert_eq!(bridge.committed, burst[15].0, "committed through the last");
        assert_eq!(
            log_head(&mut reader) - before,
            1,
            "one commit for the burst"
        );

        let before = log_head(&mut reader);
        for rec in singles {
            bridge.on_inbox_record(rec);
        }
        assert_eq!(bridge.committed, singles[15].0);
        assert_eq!(
            log_head(&mut reader) - before,
            16,
            "the control: a commit per record handed over alone"
        );
    }

    #[cfg(unix)]
    #[test]
    fn the_round_takes_its_next_item_while_its_commit_is_in_the_flush_and_reads_it_after() {
        let (lane, mut bridge) = FakeVerbLane::bridge(&[(1, "s-a"), (2, "s-b")]);
        let (records, _reader, _socket, broker, _server) = burst_rig(&lane, &mut bridge, "s-b", 1);
        let before = bridge.committed;
        bridge.in_round = true;
        bridge.on_inbox_record(&records[0]);
        bridge.in_round = false;
        assert_eq!(
            bridge.inbox_pending,
            Some(records[0].0),
            "PRECONDITION: the delivery owes its commit, nothing sent"
        );
        assert_eq!(bridge.conn.as_ref().map(Link::unacked), Some(0));

        let node = bridge.node.clone();
        bridge
            .indexed_subjects
            .insert(subject::post_lane("f", &node, "s-b", &node, "note"));
        let held = broker.pause_next_acks();
        let (go, gate) = std::sync::mpsc::channel::<()>();
        let watchdog = std::thread::spawn(move || {
            let reached = held.wait();
            let _ = gate.recv_timeout(aterm_spec::wait::MUST_HAPPEN);
            held.resume();
            reached
        });
        *lane.outbox.lock().unwrap() =
            Some("post sid=s-a id=1 to=@s-b kind=note len=4\npong".to_string());
        bridge.mailbox.push_event("EVENT 1 post 1".to_string());
        assert!(bridge.round(aterm_spec::wait::MUST_HAPPEN));
        let owed: Vec<&str> = bridge
            .conn
            .as_ref()
            .expect("the link stands")
            .owed()
            .map(|then| match then {
                Then::Commit { .. } => "commit",
                Then::Post(_) => "post",
                _ => "other",
            })
            .collect();
        assert_eq!(
            owed,
            ["commit", "post"],
            "the post went out behind the commit, its answer unread"
        );
        assert_eq!(bridge.committed, before, "the group has not moved");
        assert_eq!(bridge.inbox_pending, Some(records[0].0), "still owed");

        go.send(()).expect("the watchdog waits");
        assert!(
            watchdog.join().expect("the watchdog"),
            "the commit's batch was the one held"
        );
        assert!(bridge.mailbox.is_idle());
        assert!(bridge.round(Duration::ZERO));
        assert_eq!(bridge.conn.as_ref().map(Link::unacked), Some(0));
        assert_eq!(bridge.committed, records[0].0, "the group is there");
        assert_eq!(bridge.inbox_pending, None);
        assert!(
            lane.sent
                .lock()
                .unwrap()
                .iter()
                .any(|l| l.starts_with("outbox sent s-a 1 off=")),
            "the post retired: {:?}",
            lane.sent.lock().unwrap()
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_commit_refused_by_a_rolled_back_batch_keeps_the_link_and_its_retry_lands_it() {
        let (lane, mut bridge) = FakeVerbLane::bridge(&[(1, "s-a")]);
        let socket = lane.dir.path().join("commit-refused.sock");
        let broker =
            astream_broker::Broker::open(lane.dir.path().join("commit-refused.log")).unwrap();
        let _server = broker.serve(&socket).unwrap();
        bridge.cfg.broker = socket.to_string_lossy().into_owned();
        bridge.conn = Some(bridge.connect().unwrap().0.into());
        let face = format!("/f/f/in/{}/s-a/n-peer/note", bridge.node);
        let (mut writer, _) = bridge.connect().unwrap();
        let (r0, _) = writer.publish(91, 1, &face, b"v=1 t=1 text=a").unwrap();
        let (r1, _) = writer.publish(91, 2, &face, b"v=1 t=1 text=b").unwrap();
        let fsync_fails = |on: bool| {
            broker.inject_faults(astream_broker::InjectedFaults {
                sync: on,
                truncate: false,
            });
        };

        bridge.inbox_pending = Some(r0);
        fsync_fails(true);
        bridge.in_round = true;
        bridge.flush_inbox_commit();
        bridge.in_round = false;
        assert!(
            bridge
                .conn
                .as_ref()
                .is_some_and(|link| link.owed().any(Then::is_commit)),
            "PRECONDITION: the commit rides the pipeline"
        );
        bridge.settle_acks();
        assert!(
            bridge.conn.is_some(),
            "a commit refused by a rolled-back batch is an answer, not a lost link"
        );
        assert_eq!(bridge.committed, 0, "the group has not moved");
        assert_eq!(bridge.inbox_pending, Some(r0), "the debt is kept");

        fsync_fails(false);
        bridge.land_inbox_commit();
        assert!(bridge.conn.is_some(), "the link stood throughout");
        assert_eq!(bridge.committed, r0, "the retry landed the group");
        assert_eq!(bridge.inbox_pending, None);

        bridge.inbox_pending = Some(r1);
        fsync_fails(true);
        bridge.in_round = true;
        bridge.flush_inbox_commit();
        bridge.in_round = false;
        bridge.settle_acks();
        assert!(bridge.conn.is_some(), "the first refusal is an answer");
        bridge.land_inbox_commit();
        fsync_fails(false);
        assert!(
            bridge.conn.is_none(),
            "a refused retry is the link down, as the blocking commit's was"
        );
        assert_eq!(bridge.committed, r0, "the group stayed where it was");
        assert_eq!(
            bridge.inbox_pending,
            Some(r1),
            "the debt kept for the next connection"
        );
    }

    #[cfg(unix)]
    #[test]
    fn the_wait_tally_counts_the_commit_and_the_ack_the_loop_waited_on_and_an_off_one_counts_nothing(
    ) {
        use crate::wait_tally::{Row, Tally, FILE};
        const SID: &str = "s-tally";
        for on in [true, false] {
            let (lane, mut bridge) = FakeVerbLane::bridge(&[(1, SID)]);
            let (records, _reader, _socket, _broker, _server) =
                burst_rig(&lane, &mut bridge, SID, 4);
            let path = bridge.state.root().join(FILE);
            assert!(!bridge.waits.on(), "off unless the seam is set");
            if on {
                bridge.tally_to(&path);
            }
            let read = || Tally::parse(&std::fs::read_to_string(&path).unwrap()).unwrap();

            queue_inbox(&bridge, &records);
            run_rounds(&mut bridge);
            assert_eq!(bridge.committed, records[3].0, "on={on}");
            let counted = bridge.waits.tally();
            if !on {
                assert_eq!(counted, Tally::default(), "an off tally counts nothing");
                assert!(!path.exists(), "and writes nothing");
                continue;
            }
            let counts = |t: &Tally, r: Row| t.row(r).count;
            assert_eq!(
                (
                    counts(&counted, Row::Request),
                    counts(&counted, Row::Commit),
                    counts(&counted, Row::Ack)
                ),
                (0, 1, 1),
                "the burst's one commit is the one wait, a pipelined ack"
            );
            assert!(counted.row(Row::Commit).total <= counted.row(Row::Ack).total);
            let written = read();
            assert_eq!(
                written.committed, records[3].0,
                "written at the idle moment, past the commit"
            );
            for row in [Row::Request, Row::Commit, Row::Ack, Row::LaneDeliver] {
                assert_eq!(written.row(row), counted.row(row), "{row:?}");
            }
            assert_eq!(counts(&written, Row::LaneDeliver), 4);
            assert_eq!(
                (
                    counts(&written, Row::QueuedRecord),
                    counts(&written, Row::WakeRecord)
                ),
                (4, 0)
            );
            assert!(counts(&written, Row::Park) >= 4, "a take per record");

            bridge.in_round = true;
            bridge.publish_ev("tallied");
            bridge.in_round = false;
            run_rounds(&mut bridge);
            let waited = read().since(&written).unwrap();
            assert_eq!(
                (
                    waited.row(Row::Request).count,
                    waited.row(Row::Commit).count,
                    waited.row(Row::Ack).count
                ),
                (0, 0, 1),
                "a pipelined ack, read at the idle moment"
            );
            assert!(waited.at_ns > 0, "the bridge's own clock moved");
        }
    }

    #[cfg(unix)]
    #[test]
    fn a_crash_before_the_bursts_commit_redelivers_the_burst_and_the_endpoint_keeps_one_of_each() {
        const SID: &str = "s-crash";
        const N: usize = 8;
        let (lane, mut bridge) = FakeVerbLane::bridge(&[(1, SID)]);
        let (burst, mut reader, socket, _broker, _server) =
            burst_rig(&lane, &mut bridge, SID, N as u64);
        let offs: Vec<u64> = burst.iter().map(|r| r.0).collect();
        let group = subject::inbox_group("f", &bridge.node);
        let filter = subject::inbox_filter("f", &bridge.node);
        let group_offers = |what: &str, n: usize| -> Vec<u64> {
            let (client, _) = transport::connect(&socket, aterm_spec::wait::MUST_HAPPEN).unwrap();
            let mut sub = client.subscribe_group(&group, &filter).unwrap();
            (0..n).map(|_| next_delivery(&mut sub, what).0).collect()
        };

        queue_inbox(&bridge, &burst);
        for _ in 0..N {
            assert!(bridge.round(Duration::ZERO));
        }
        assert_eq!(lane.delivery_offsets(), offs, "the endpoint took the burst");
        assert_eq!(
            bridge.inbox_pending,
            Some(offs[N - 1]),
            "its commit is owed"
        );
        let before = log_head(&mut reader);
        assert_eq!(
            group_offers("the durable cursor after the crash", N),
            offs,
            "nothing was committed: the next bridge is offered the whole burst"
        );

        bridge.inbox_pending = None;
        queue_inbox(&bridge, &burst);
        run_rounds(&mut bridge);
        assert_eq!(
            lane.delivery_offsets(),
            offs,
            "redelivered, answered from the endpoint's memory, appended nowhere"
        );
        assert_eq!(bridge.committed, offs[N - 1]);
        assert_eq!(log_head(&mut reader) - before, 1, "and committed once");
        let (mut writer, _) = bridge.connect().unwrap();
        let (sentinel, _) = writer
            .publish(92, 1, &burst[0].1, b"v=1 t=2 from=s-peer text=after")
            .unwrap();
        assert_eq!(
            group_offers("the cursor after the commit", 1),
            [sentinel],
            "the burst is behind the cursor now"
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_reconnect_does_not_commit_a_local_refusal_whose_verdict_never_landed() {
        const HOSTED: &str = "s-hosted";
        const ABSENT: &str = "s-not-hosted";
        let (lane, bridge) = FakeVerbLane::bridge(&[(1, HOSTED)]);
        let (socket, _broker, _server) = serve_log(&lane, "unproved");
        struct CloseBridge(Bridge);
        impl Drop for CloseBridge {
            fn drop(&mut self) {
                self.0.close_subscriptions();
                self.0.conn = None;
            }
        }
        let mut owned = CloseBridge(bridge);
        let bridge = &mut owned.0;
        bridge.cfg.broker = socket.clone();
        bridge.conn = Some(bridge.connect().unwrap().0.into());
        assert!(bridge.refresh_sessions().is_ok());
        bridge.per_sid.pending_admit.clear();
        let (mut writer, _) = bridge.connect().unwrap();
        let same_log = writer.log_identity().expect("the private log identity");
        let node = bridge.node.clone();
        let face = |sid: &str| format!("/f/f/in/{node}/{sid}/h-sender/note");
        let body = b"v=1 t=1 text=owned-regression";
        let mut record = |seq: u64, sid: &str| {
            let subject = face(sid);
            let (off, _) = writer.publish(812, seq, &subject, body).unwrap();
            (off, subject, body.to_vec())
        };
        let accepted = record(1, HOSTED);
        bridge.on_inbox_record(&accepted);
        assert_eq!(lane.delivery_offsets(), [accepted.0], "endpoint control");
        assert_eq!(bridge.committed, accepted.0);
        let refused = record(2, ABSENT);
        bridge.on_inbox_record(&refused);
        assert_eq!(bridge.committed, refused.0, "durable refusal control");

        let (mut reader, _) = bridge.connect().unwrap();
        let ev_face = bridge.ev_face(None);
        let verdict_landed = |reader: &mut Conn, off: u64| {
            let (rows, _) = reader.fetch(0, &ev_face, 64).unwrap();
            rows.iter().any(|(_, _, raw)| {
                Body::decode(raw).0.unknown.get("ev").is_some_and(|v| {
                    crate::pct::decode(v).starts_with(&format!("undeliverable off={off} "))
                })
            })
        };
        assert!(verdict_landed(&mut reader, refused.0));
        let unproved = record(3, ABSENT);
        let sentinel = record(4, ABSENT);
        let (publisher, closer) = bridge.connect().unwrap();
        bridge.conn = Some(publisher.into());
        closer.close();
        queue_inbox(bridge, std::slice::from_ref(&unproved));
        assert!(bridge.round(Duration::ZERO));
        assert!(
            bridge.conn.is_none(),
            "the pre-write refusal lost the publisher"
        );
        assert_eq!(bridge.committed, refused.0, "no failed write committed");
        assert!(
            !verdict_landed(&mut reader, unproved.0),
            "no verdict landed"
        );
        assert_eq!(lane.delivery_offsets(), [accepted.0], "not an endpoint ack");

        assert!(
            bridge.attach_broker(),
            "actual re-attach to the private broker"
        );
        assert_eq!(reader.log_identity().unwrap(), same_log, "the same log");
        assert!(
            !verdict_landed(&mut reader, unproved.0),
            "no refill re-proved it"
        );
        assert_eq!(
            lane.delivery_offsets(),
            [accepted.0],
            "still no endpoint ack"
        );
        let (client, _) = transport::connect(&socket, aterm_spec::wait::MUST_HAPPEN).unwrap();
        let group = subject::inbox_group("f", &bridge.node);
        let filter = subject::inbox_filter("f", &bridge.node);
        let mut sub = client.subscribe_group(&group, &filter).unwrap();
        let offered = next_delivery(&mut sub, "the first record still owed after reconnect").0;
        assert_eq!(
            offered, unproved.0,
            "an unproven local refusal stays offered, rather than advancing to sentinel {}",
            sentinel.0
        );
    }

    #[cfg(unix)]
    #[test]
    fn no_burst_commit_passes_an_owed_record() {
        const HELD: &str = "s-held";
        const OK: &str = "s-ok";
        let (lane, mut bridge) = FakeVerbLane::bridge(&[(1, HELD), (2, OK)]);
        let (records, _reader, _socket, _broker, _server) = burst_rig(&lane, &mut bridge, OK, 6);
        let node = bridge.node.clone();
        let mail = |off: u64, sid: &str, body: &[u8]| -> BrokerRecord {
            (
                off,
                format!("/f/f/in/{node}/{sid}/n-peer/note"),
                body.to_vec(),
            )
        };
        let plain = b"v=1 t=1 from=s-peer text=hi";
        lane.answer_deliveries_to(HELD, SHIPPED_DELIVER_USAGE);
        queue_inbox(
            &bridge,
            &[
                mail(records[0].0, OK, plain),
                mail(records[1].0, HELD, plain),
                mail(records[2].0, OK, plain),
            ],
        );
        run_rounds(&mut bridge);
        assert_eq!(bridge.inbox_commit_floor(), Some(records[1].0));
        assert_eq!(
            bridge.committed, records[0].0,
            "the burst's commit stops below the owed record"
        );

        bridge.in_round = true;
        for rec in &records[3..] {
            bridge.on_inbox_record(&mail(rec.0, OK, plain));
        }
        bridge.in_round = false;
        assert_eq!(bridge.inbox_pending, Some(records[5].0), "accounted for");
        bridge.wind_down();
        assert_eq!(
            bridge.committed, records[0].0,
            "the exit's commit is still below the owed record"
        );
        assert_eq!(bridge.inbox_commit_floor(), Some(records[1].0));
        assert_eq!(
            bridge.state.inbox_owed(),
            None,
            "nor is anything past it handed to the next bridge"
        );
    }

    #[cfg(unix)]
    #[test]
    fn no_exit_commit_passes_an_unaccounted_record() {
        const OK: &str = "s-ok";
        let (lane, mut bridge) = FakeVerbLane::bridge(&[(1, OK)]);
        let (records, _reader, _socket, _broker, _server) = burst_rig(&lane, &mut bridge, OK, 4);
        bridge.on_inbox_record(&records[0]);
        assert_eq!(bridge.committed, records[0].0);

        bridge.in_round = true;
        bridge.on_inbox_record(&records[1]);
        bridge.fault = Fault::LoseAtermBeforeDeliver;
        bridge.on_inbox_record(&records[2]);
        assert!(bridge.ctl.lost(), "the lane is gone");
        let (off, subject, _) = &records[3];
        bridge.on_inbox_record(&(
            *off,
            subject.clone(),
            b"v=1 t=1 from=s-peer via=NOT-A-PRINCIPAL text=x".to_vec(),
        ));
        bridge.in_round = false;
        assert_eq!(
            bridge.inbox_pending,
            Some(records[3].0),
            "the local refusal accounted for itself past the unaccounted record"
        );
        assert_eq!(
            bridge.inbox_commit_floor(),
            Some(records[2].0),
            "and the unaccounted record is the only floor"
        );
        bridge.wind_down();
        assert_eq!(
            bridge.committed, records[1].0,
            "through the delivered record, and not past the unaccounted one"
        );
        assert!(
            bridge
                .state
                .inbox_owed()
                .is_none_or(|owed| owed < records[2].0),
            "nothing past it is handed over"
        );
    }

    #[cfg(unix)]
    fn first_offered(socket: &str, node: &str, what: &str) -> u64 {
        let (client, _) = transport::connect(socket, aterm_spec::wait::MUST_HAPPEN).unwrap();
        let group = subject::inbox_group("f", node);
        let filter = subject::inbox_filter("f", node);
        let mut sub = client.subscribe_group(&group, &filter).unwrap();
        next_delivery(&mut sub, what).0
    }

    #[cfg(unix)]
    #[test]
    fn a_records_commit_waits_for_its_own_verdicts_answer_and_dies_with_its_connection() {
        const SID: &str = "s-host";
        const ABSENT: &str = "s-not-hosted";
        for dropped in [false, true] {
            let (lane, mut bridge) = FakeVerbLane::bridge(&[(1, SID)]);
            let (records, mut writer, socket, _broker, _server) =
                burst_rig(&lane, &mut bridge, SID, 1);
            let node = bridge.node.clone();
            let (delivered, hosted, body) = records[0].clone();
            let absent = format!("/f/f/in/{node}/{ABSENT}/n-peer/note");
            let (refused, _) = writer.publish(91, 2, &absent, &body).unwrap();
            let (after, _) = writer.publish(91, 3, &hosted, &body).unwrap();

            bridge.in_round = true;
            bridge.publish_ev("unrelated");
            bridge.on_inbox_record(&(delivered, hosted.clone(), body.clone()));
            assert_eq!(
                bridge.inbox_pending,
                Some(delivered),
                "an ack that is no output of it holds nothing back"
            );
            bridge.on_inbox_record(&(refused, absent.clone(), body.clone()));
            assert_eq!(
                bridge.inbox_pending,
                Some(delivered),
                "its verdict is still owed an answer"
            );
            bridge.on_inbox_record(&(after, hosted.clone(), body.clone()));
            assert_eq!(
                bridge.inbox_pending,
                Some(delivered),
                "a later record waits on the earlier one's verdict"
            );
            bridge.in_round = false;
            assert_eq!(
                lane.delivery_offsets(),
                [delivered, after],
                "the endpoint took both hosted records"
            );

            if !dropped {
                bridge.settle_acks();
                assert_eq!(bridge.inbox_pending, Some(after), "answered: all three");
                bridge.land_inbox_commit();
                assert_eq!(bridge.committed, after);
                continue;
            }
            bridge.conn = None;
            assert_eq!(bridge.inbox_pending, Some(delivered), "the claim went too");
            assert!(bridge.attach_broker(), "re-attached to the same log");
            assert_eq!(
                bridge.committed, delivered,
                "the attach commits what was proven, and no further"
            );
            let (sentinel, _) = writer.publish(91, 4, &hosted, &body).unwrap();
            assert_eq!(
                first_offered(&socket, &node, "the durable cursor after the reconnect"),
                refused,
                "the refused record is offered again, not skipped for sentinel {sentinel}"
            );
            bridge.close_subscriptions();
        }
    }

    #[cfg(unix)]
    #[test]
    fn a_notice_a_dead_publisher_never_carried_is_not_committed_past() {
        const SID: &str = "s-host";
        let (lane, mut bridge) = FakeVerbLane::bridge(&[(1, SID)]);
        let (records, mut reader, socket, _broker, _server) = burst_rig(&lane, &mut bridge, SID, 1);
        let node = bridge.node.clone();
        let face = format!("/f/f/in/{node}/{SID}/n-peer/note");
        bridge.on_inbox_record(&records[0]);
        assert_eq!(bridge.committed, records[0].0);
        let refused = b"v=1 t=1 from=s-peer via=NOT-A-PRINCIPAL text=x";
        let (mut writer, _) = bridge.connect().unwrap();
        let off = writer.publish(93, 1, &face, refused).unwrap().0;
        let (conn, closer) = bridge.connect().unwrap();
        bridge.conn = Some(conn.into());
        closer.close();
        bridge.in_round = true;
        bridge.on_inbox_record(&(off, face.clone(), refused.to_vec()));
        bridge.in_round = false;
        assert!(bridge.conn.is_none(), "the connection died");
        assert_eq!(bridge.inbox_pending, None, "nothing it did is proven");

        assert!(bridge.attach_broker(), "re-attached to the same log");
        assert_eq!(bridge.committed, records[0].0, "nothing past the refusal");
        let (sentinel, _) = writer
            .publish(94, 1, &face, b"v=1 t=2 from=s-peer text=s")
            .unwrap();
        assert_eq!(
            first_offered(&socket, &node, "the durable cursor"),
            off,
            "offered again, not committed past for sentinel {sentinel}"
        );
        let notices = format!("/f/f/in/n-peer/s-peer/{node}/undeliverable");
        let (rows, _) = reader.fetch(0, &notices, 64).unwrap();
        assert!(rows.is_empty(), "no notice landed: {rows:?}");
        bridge.close_subscriptions();
    }

    #[cfg(unix)]
    #[test]
    fn an_exit_with_no_connection_hands_the_bursts_commit_to_the_next_bridge() {
        const SID: &str = "s-burst";
        for handed in [true, false] {
            let (lane, mut old) = FakeVerbLane::bridge(&[(1, SID)]);
            let state = old.state.root().to_path_buf();
            let (burst, mut writer, socket, _broker, _server) = burst_rig(&lane, &mut old, SID, 4);
            let node = old.node.clone();
            old.in_round = true;
            for rec in &burst {
                old.on_inbox_record(rec);
            }
            old.in_round = false;
            assert_eq!(
                old.inbox_pending,
                Some(burst[3].0),
                "the burst's commit is owed"
            );
            assert_eq!(lane.delivery_offsets().len(), 4, "the endpoint took it");
            old.state.set_seen_off(SID, burst[3].0).unwrap();
            old.conn = None;
            old.wind_down();
            assert_eq!(old.state.inbox_owed(), Some(burst[3].0), "handed over");
            drop(old);
            if !handed {
                old_bridge_forgets_the_hand_over(&state);
            }

            let (next_lane, mut next) = FakeVerbLane::bridge_with_state(&[(1, SID)], Some(&state));
            next.cfg.broker = socket.clone();
            assert!(next.attach_broker(), "the next bridge attached");
            let (sentinel, _) = writer.publish(91, 5, &burst[0].1, &burst[0].2).unwrap();
            let first = first_offered(&socket, &node, "the durable cursor after the handoff");
            if handed {
                assert_eq!(first, sentinel, "the burst is committed, not offered again");
                assert_eq!(next.committed, burst[3].0);
                assert_eq!(next.state.inbox_owed(), None, "and the hand-over forgotten");
            } else {
                assert_eq!(first, burst[0].0, "the control: the whole burst again");
            }
            next.close_subscriptions();
            drop(next_lane);
        }
    }

    #[cfg(unix)]
    fn old_bridge_forgets_the_hand_over(state: &std::path::Path) {
        std::fs::remove_file(state.join("inbox-owed")).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn a_handed_over_inbox_commit_above_the_fork_dies_with_a_relaxed_generation() {
        use crate::durability::{self, Durability};
        const SID: &str = "s-kept";
        for reboot in [true, false] {
            let case = format!("reboot={reboot}");
            let (lane, mut old) = FakeVerbLane::bridge(&[(1, SID)]);
            lane.answer_hello_with(&format!("OK lane={LINK_LOG_LANE} build=test\n"));
            old.introduce();
            assert!(old.refresh_sessions().is_ok());
            let state = old.state.root().to_path_buf();
            let log = lane.dir.path().join("relaxed.log");
            let socket = lane.dir.path().join("relaxed.sock");
            old.cfg.broker = socket.to_string_lossy().into_owned();
            let node = old.node.clone();
            let face = format!("/f/f/in/{node}/{SID}/n-peer/note");
            let body = b"v=1 t=1 from=s-peer text=burst".to_vec();
            let (high, cut, burst) = {
                let opened = durability::open(
                    &log,
                    None,
                    Durability::Relaxed,
                    Some(durability::test_boot(1)),
                )
                .unwrap();
                opened.broker.arm_unclean_stop();
                let mut server = opened.broker.serve(&socket).unwrap();
                let (mut writer, _) = old.connect().unwrap();
                let mut burst = Vec::new();
                for seq in 1..=2 {
                    let (off, _) = writer.publish(91, seq, &face, &body).unwrap();
                    burst.push((off, face.clone(), body.clone()));
                }
                let cut = std::fs::metadata(&log).unwrap().len();
                for seq in 3..=4 {
                    let (off, _) = writer.publish(91, seq, &face, &body).unwrap();
                    burst.push((off, face.clone(), body.clone()));
                }
                assert!(old.attach_broker(), "{case}: the first attach");
                old.mailbox.reset_broker_sources();
                old.in_round = true;
                for rec in &burst {
                    old.on_inbox_record(rec);
                }
                old.in_round = false;
                assert_eq!(old.inbox_pending, Some(burst[3].0), "{case}: owed");
                let high = old.state.log_witness().expect("witnessed").high;
                drop(writer);
                old.conn = None;
                old.wind_down();
                assert_eq!(
                    old.state.inbox_owed(),
                    Some(burst[3].0),
                    "{case}: handed over"
                );
                old.close_subscriptions();
                drop(old);
                server.shutdown();
                (high, cut, burst)
            };
            std::fs::OpenOptions::new()
                .write(true)
                .open(&log)
                .unwrap()
                .set_len(cut)
                .unwrap();
            let boot = durability::test_boot(if reboot { 2 } else { 1 });
            let reopened = durability::open(&log, None, Durability::Relaxed, Some(boot)).unwrap();
            assert_eq!(reopened.broker.head(), 2, "{case}: a clean prefix");
            let _server = reopened.broker.serve(&socket).unwrap();
            let (mut other, _) =
                transport::connect(&socket.to_string_lossy(), ACK_DEADLINE).unwrap();
            let mut new_mail = Vec::new();
            let mut seq = 1;
            while reopened.broker.head() <= high {
                let (off, _) = other
                    .publish(92, seq, &face, b"v=1 t=1 from=s-peer text=new")
                    .unwrap();
                new_mail.push(off);
                seq += 1;
            }
            assert!(
                new_mail.contains(&burst[2].0),
                "{case}: the new mail reuses the burst's offsets"
            );

            let (next_lane, mut next) = FakeVerbLane::bridge_with_state(&[(1, SID)], Some(&state));
            next_lane.answer_hello_with(&format!("OK lane={LINK_LOG_LANE} build=test\n"));
            next.introduce();
            assert!(next.refresh_sessions().is_ok());
            next.cfg.broker = socket.to_string_lossy().into_owned();
            assert!(next.attach_broker(), "{case}: the next bridge attached");
            let first = first_offered(&socket.to_string_lossy(), &node, "after the power loss");
            if reboot {
                assert!(
                    first <= new_mail[0],
                    "{case}: the group offers the new mail (first offered {first}, new mail \
                     {new_mail:?})"
                );
                assert_eq!(
                    next.state.inbox_owed(),
                    None,
                    "{case}: the hand-over, above the fork, dropped"
                );
            } else {
                assert!(
                    first > new_mail[0],
                    "{case}: CONTROL: the kept identity commits the hand-over over the new \
                     mail (first offered {first}, new mail {new_mail:?})"
                );
            }
            next.close_subscriptions();
            drop(next_lane);
        }
    }

    #[cfg(unix)]
    #[test]
    fn a_refused_live_records_notice_and_its_commit_are_one_record() {
        const SID: &str = "s-host";
        let (lane, mut bridge) = FakeVerbLane::bridge(&[(1, SID)]);
        let (records, mut reader, socket, _broker, _server) = burst_rig(&lane, &mut bridge, SID, 1);
        let node = bridge.node.clone();
        let face = format!("/f/f/in/{node}/{SID}/n-peer/note");
        let refused = b"v=1 t=1 from=s-peer via=NOT-A-PRINCIPAL text=x";
        let (mut writer, _) = bridge.connect().unwrap();
        let mut refused_at = |seq: u64| writer.publish(93, seq, &face, refused).unwrap().0;
        let notices = format!("/f/f/in/n-peer/s-peer/{node}/undeliverable");
        let answered = |reader: &mut Conn| -> Vec<Option<u64>> {
            let (rows, _) = reader.fetch(0, &notices, 64).unwrap();
            rows.iter().map(|(_, _, b)| Body::decode(b).0.re).collect()
        };
        bridge.on_inbox_record(&records[0]);

        let live = refused_at(1);
        let before = log_head(&mut reader);
        bridge.on_inbox_record(&(live, face.clone(), refused.to_vec()));
        assert_eq!(
            log_head(&mut reader) - before,
            2,
            "the ev, and notice+commit"
        );
        assert_eq!(answered(&mut reader), [Some(live)]);
        assert_eq!(bridge.committed, live);
        let group = subject::inbox_group("f", &node);
        let filter = subject::inbox_filter("f", &node);
        let (mut w, _) = bridge.connect().unwrap();
        let (sentinel, _) = w
            .publish(94, 1, &face, b"v=1 t=2 from=s-peer text=s")
            .unwrap();
        let (client, _) = transport::connect(&socket, aterm_spec::wait::MUST_HAPPEN).unwrap();
        let mut sub = client.subscribe_group(&group, &filter).unwrap();
        assert_eq!(
            next_delivery(&mut sub, "the cursor after the notice").0,
            sentinel,
            "the notice's own commit moved the durable cursor past the record"
        );
        drop(sub);
        bridge.on_inbox_record(&(
            sentinel,
            face.clone(),
            b"v=1 t=2 from=s-peer text=s".to_vec(),
        ));

        let old_way = refused_at(2);
        let before = log_head(&mut reader);
        assert_eq!(
            bridge.deliver_record(old_way, &face, refused),
            Delivery::Accounted
        );
        bridge.commit_upto(old_way);
        bridge.settle_acks();
        assert_eq!(
            log_head(&mut reader) - before,
            3,
            "ev, notice, then a commit"
        );

        let refilled = refused_at(3);
        bridge.state.set_seen_off(SID, refilled - 1).unwrap();
        let committed = bridge.committed;
        let before = log_head(&mut reader);
        assert!(bridge.refill(SID));
        assert_eq!(
            log_head(&mut reader) - before,
            2,
            "ev and notice, no commit"
        );
        assert_eq!(bridge.committed, committed, "a refill commits nothing");
        let before = log_head(&mut reader);
        bridge.on_inbox_record(&(refilled, face.clone(), refused.to_vec()));
        assert_eq!(log_head(&mut reader) - before, 2, "ev and a plain commit");
        assert_eq!(bridge.committed, refilled);

        let floored = refused_at(4);
        bridge.commit_floor = Some(floored);
        let before = log_head(&mut reader);
        bridge.on_inbox_record(&(floored, face.clone(), refused.to_vec()));
        assert_eq!(
            log_head(&mut reader) - before,
            3,
            "ev, the notice alone, and a commit stopped below the floor"
        );
        assert_eq!(bridge.committed, floored - 1, "never at the floor");
        assert_eq!(
            answered(&mut reader),
            [Some(live), Some(old_way), Some(refilled), Some(floored)],
            "every refusal told its sender once"
        );
    }

    #[cfg(unix)]
    #[test]
    fn an_attach_sends_every_sessions_presence_row_before_reading_one_ack() {
        const N: u64 = 24;
        let sids: Vec<String> = (1..=N).map(|i| format!("s-p{i:02}")).collect();
        let roster: Vec<(u64, &str)> = sids
            .iter()
            .enumerate()
            .map(|(i, sid)| (i as u64 + 1, sid.as_str()))
            .collect();
        let (lane, mut bridge) = FakeVerbLane::bridge(&roster);
        assert!(bridge.refresh_sessions().is_ok());
        let (socket, _broker, _server) = serve_log(&lane, "attach");
        bridge.cfg.broker = socket;
        assert!(bridge.attach_broker());
        let link = bridge.conn.as_ref().expect("attached");
        assert_eq!(link.unacked(), 0, "the attach read every ack it owed");
        assert!(
            link.peak() >= N as usize,
            "the rows went out together: peak {}",
            link.peak()
        );
        let (mut reader, _) = bridge.connect().unwrap();
        let mut rows = BTreeSet::new();
        transport::walk_last(
            &mut reader,
            &format!("/f/f/pub/{}/*/presence", bridge.node),
            |(_, subject, raw)| {
                if Body::decode(raw).0.unknown.get("state").map(String::as_str) == Some("live") {
                    rows.insert(subject.split('/').nth(5).unwrap_or_default().to_string());
                }
                Ok(())
            },
        )
        .unwrap();
        for sid in &sids {
            assert!(rows.contains(sid), "{sid}'s row landed: {rows:?}");
        }

        bridge.conn = Some(bridge.connect().unwrap().0.into());
        for sid in &sids {
            bridge.publish_session_presence(sid, "live");
        }
        assert_eq!(
            bridge.conn.as_ref().unwrap().peak(),
            1,
            "the control: one at a time"
        );
    }

    fn queued_note(sid: &str, id: u64, to: &str, text: &str) -> String {
        format!(
            "post sid={sid} id={id} to={to} kind=note len={}\n{text}",
            text.len()
        )
    }

    fn without_post(frame: &str, sid: &str, id: &str) -> String {
        let (sid, id) = (format!("sid={sid}"), format!("id={id}"));
        let mut kept = String::new();
        let mut rest = frame;
        while let Some((line, tail)) = rest.split_once('\n') {
            let words: Vec<&str> = line.split_whitespace().collect();
            let len = words
                .iter()
                .find_map(|word| word.strip_prefix("len="))
                .and_then(|n| n.parse::<usize>().ok())
                .filter(|_| line.starts_with("post "))
                .unwrap_or(0)
                .min(tail.len());
            let (body, next) = tail.split_at(len);
            if !(words.contains(&sid.as_str()) && words.contains(&id.as_str())) {
                kept.push_str(line);
                kept.push('\n');
                kept.push_str(body);
            }
            rest = next;
        }
        kept
    }

    fn retirements(lane: &FakeVerbLane) -> Vec<(String, u64, Option<u64>)> {
        lane.sent
            .lock()
            .unwrap()
            .iter()
            .filter_map(|line| {
                let mut words = line.strip_prefix("outbox sent ")?.split_whitespace();
                let sid = words.next()?.to_string();
                let id = words.next()?.parse().ok()?;
                let off = words.next()?.strip_prefix("off=")?.parse().ok();
                Some((sid, id, off))
            })
            .collect()
    }

    fn notes_on(reader: &mut Conn, subject: &str) -> Vec<(u64, String)> {
        let (rows, _) = reader.fetch(0, subject, 256).unwrap();
        rows.iter()
            .map(|(off, _, raw)| (*off, Body::decode(raw).0.text))
            .collect()
    }

    #[cfg(unix)]
    #[test]
    fn a_burst_from_many_senders_is_on_the_wire_at_once_and_each_lands_in_its_order() {
        const SENDERS: [&str; 3] = ["s-a", "s-b", "s-c"];
        let (lane, mut bridge) =
            FakeVerbLane::bridge(&[(1, "s-a"), (2, "s-b"), (3, "s-c"), (4, "s-r")]);
        assert!(bridge.refresh_sessions().is_ok());
        let (socket, _broker, _server) = serve_log(&lane, "burst");
        bridge.cfg.broker = socket;
        bridge.conn = Some(bridge.connect().unwrap().0.into());
        let lane_of = format!("/f/f/in/{0}/s-r/{0}/note", bridge.node);
        bridge.indexed_subjects.insert(lane_of.clone());
        let burst = |key: bool| -> String {
            let mut frame = String::new();
            for n in 1..=3u64 {
                for sid in SENDERS {
                    let text = format!("{sid}-{n}");
                    let line = queued_note(sid, n, "@s-r", &text);
                    frame.push_str(&if key {
                        line.replacen(" kind=", &format!(" key={text} kind="), 1)
                    } else {
                        line
                    });
                }
            }
            frame
        };
        *lane.outbox.lock().unwrap() = Some(burst(false));
        bridge.drain_outbox();

        let link = bridge.conn.as_ref().expect("still connected");
        assert_eq!(
            link.unacked(),
            0,
            "a drain outside the round reads its acks"
        );
        assert_eq!(
            link.peak(),
            SENDERS.len(),
            "one post of each sender on the wire at once, never two of one"
        );
        let retired = retirements(&lane);
        assert_eq!(retired.len(), 9, "every post retired once: {retired:#?}");
        let (mut reader, _) = bridge.connect().unwrap();
        let notes = notes_on(&mut reader, &lane_of);
        assert_eq!(notes.len(), 9, "each note exactly once: {notes:#?}");
        for sid in SENDERS {
            let mine: Vec<String> = notes
                .iter()
                .map(|(_, text)| text.clone())
                .filter(|text| text.starts_with(sid))
                .collect();
            let posted: Vec<String> = (1..=3).map(|n| format!("{sid}-{n}")).collect();
            assert_eq!(mine, posted, "{sid}'s notes in the order it posted them");
            for n in 1..=3u64 {
                let at = notes
                    .iter()
                    .find(|(_, text)| *text == format!("{sid}-{n}"))
                    .map(|(off, _)| *off);
                assert!(
                    retired.contains(&(sid.to_string(), n, at)),
                    "{sid} {n} retired at its own record's offset {at:?}: {retired:#?}"
                );
                assert_eq!(bridge.state.post_pin(sid, n).unwrap(), None);
                assert!(at.is_some_and(|off| bridge.self_acked.contains(&off)));
            }
        }
        assert!(
            bridge.state.in_flight().unwrap().is_empty(),
            "every own-lane entry went with its answer"
        );

        bridge.conn = Some(bridge.connect().unwrap().0.into());
        lane.sent.lock().unwrap().clear();
        *lane.outbox.lock().unwrap() = Some(burst(true));
        bridge.drain_outbox();
        assert_eq!(retirements(&lane).len(), 9);
        assert_eq!(
            bridge.conn.as_ref().unwrap().peak(),
            0,
            "the control: nothing pipelined"
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_senders_post_refused_mid_burst_holds_its_later_posts_and_no_one_elses() {
        let (lane, mut bridge) = FakeVerbLane::bridge(&[(1, "s-a"), (2, "s-b"), (3, "s-r")]);
        assert!(bridge.refresh_sessions().is_ok());
        let socket = lane.dir.path().join("refused.sock");
        let broker = astream_broker::Broker::open(lane.dir.path().join("refused.log")).unwrap();
        let _server = broker.serve(&socket).unwrap();
        let socket = socket.to_string_lossy().into_owned();
        let broker = Arc::new(broker);
        bridge.cfg.broker = socket;
        bridge.conn = Some(bridge.connect().unwrap().0.into());
        let lane_of = format!("/f/f/in/{0}/s-r/{0}/note", bridge.node);
        bridge.indexed_subjects.insert(lane_of.clone());
        *lane.outbox.lock().unwrap() = Some(
            [
                queued_note("s-a", 1, "@s-r", "a1"),
                queued_note("s-a", 2, "@s-r", "a2"),
                queued_note("s-b", 1, "@s-r", "b1"),
                queued_note("s-a", 3, "@s-r", "a3"),
                queued_note("s-b", 2, "@s-r", "b2"),
            ]
            .concat(),
        );

        broker.inject_faults(astream_broker::InjectedFaults {
            sync: true,
            truncate: false,
        });
        let held = broker.pause_next_acks();
        let (done_tx, done) = std::sync::mpsc::channel();
        let writer = broker.clone();
        std::thread::spawn(move || {
            let tried = held.wait();
            writer.inject_faults(astream_broker::InjectedFaults::default());
            held.resume();
            let _ = done_tx.send(tried);
        });
        bridge.drain_outbox();
        assert_eq!(
            done.recv_timeout(aterm_spec::wait::MUST_HAPPEN),
            Ok(true),
            "a1's batch was tried and held unanswered (a hang detector)"
        );

        assert!(
            bridge.conn.is_some(),
            "a refusal is an answer, not a lost link"
        );
        let retired = retirements(&lane);
        let (mut reader, _) = bridge.connect().unwrap();
        let notes = notes_on(&mut reader, &lane_of);
        let texts: Vec<&str> = notes.iter().map(|(_, text)| text.as_str()).collect();
        assert_eq!(texts, ["b1", "b2"], "only s-b's notes landed");
        for (n, text) in [(1, "b1"), (2, "b2")] {
            let at = notes.iter().find(|(_, t)| t == text).map(|(off, _)| *off);
            assert!(
                retired.contains(&("s-b".to_string(), n, at)),
                "{retired:#?}"
            );
        }
        assert!(
            retired.iter().all(|(sid, _, _)| sid == "s-b"),
            "nothing of s-a's retired: {retired:#?}"
        );
        assert!(
            bridge.state.post_pin("s-a", 1).unwrap().is_some(),
            "a1 keeps its sequence for the retry"
        );
        for n in [2, 3] {
            assert_eq!(
                bridge.state.post_pin("s-a", n).unwrap(),
                None,
                "a{n} was never reserved, let alone sent"
            );
        }
        assert!(bridge.state.in_flight().unwrap().is_empty());

        lane.sent.lock().unwrap().clear();
        *lane.outbox.lock().unwrap() = Some(
            [
                queued_note("s-a", 1, "@s-r", "a1"),
                queued_note("s-a", 2, "@s-r", "a2"),
                queued_note("s-a", 3, "@s-r", "a3"),
            ]
            .concat(),
        );
        bridge.drain_outbox();
        let retired = retirements(&lane);
        let notes = notes_on(&mut reader, &lane_of);
        let texts: Vec<&str> = notes.iter().map(|(_, text)| text.as_str()).collect();
        assert_eq!(
            texts,
            ["b1", "b2", "a1", "a2", "a3"],
            "s-a's notes once each, in its order"
        );
        for (n, text) in [(1, "a1"), (2, "a2"), (3, "a3")] {
            let at = notes.iter().find(|(_, t)| t == text).map(|(off, _)| *off);
            assert!(
                retired.contains(&("s-a".to_string(), n, at)),
                "{retired:#?}"
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn an_own_lane_record_is_judged_only_once_its_pipelined_post_is_answered() {
        let (lane, mut bridge) = FakeVerbLane::bridge(&[(1, "s-a"), (2, "s-r")]);
        assert!(bridge.refresh_sessions().is_ok());
        let (socket, _broker, _server) = serve_log(&lane, "self-lane");
        bridge.cfg.broker = socket;
        assert!(bridge.attach_broker());
        let lane_of = format!("/f/f/in/{0}/s-r/{0}/note", bridge.node);
        bridge.indexed_subjects.insert(lane_of.clone());
        *lane.outbox.lock().unwrap() = Some(queued_note("s-a", 1, "@s-r", "mine"));
        bridge.in_round = true;
        bridge.drain_outbox();
        assert_eq!(
            bridge.conn.as_ref().unwrap().unacked(),
            1,
            "the post's ack is left for the loop"
        );
        assert!(
            retirements(&lane).is_empty(),
            "not retired before its answer"
        );

        let (mut reader, _) = bridge.connect().unwrap();
        let (off, raw) = aterm_spec::wait::until_within(
            aterm_spec::wait::MUST_HAPPEN,
            "the note on the log",
            || {
                let (rows, _) = reader.fetch(0, &lane_of, 1).ok()?;
                rows.into_iter().next().map(|(off, _, raw)| (off, raw))
            },
        );
        assert!(
            forged_self(&bridge.self_acked, off),
            "the control: with its ack unread the set calls the node's own post a forgery"
        );
        bridge.on_inbox_record(&(off, lane_of.clone(), raw));
        bridge.in_round = false;

        assert!(bridge.self_acked.contains(&off), "the ack was read first");
        assert!(
            lane.transcript().contains(&format!("deliver {off}")),
            "delivered: {:#?}",
            lane.transcript()
        );
        assert!(retirements(&lane).contains(&("s-a".to_string(), 1, Some(off))));
        bridge.settle_acks();
        let (rows, _) = reader.fetch(0, "/f/f/pub/>", 4096).unwrap();
        let evs: Vec<String> = rows
            .iter()
            .map(|(_, _, raw)| crate::pct::decode(&String::from_utf8_lossy(raw)))
            .filter(|ev| ev.contains("forged-self") || ev.contains("cap-compromised"))
            .collect();
        assert!(evs.is_empty(), "no accusation: {evs:#?}");
    }

    #[cfg(unix)]
    #[test]
    fn a_halt_queued_at_a_drain_of_fresh_posts_is_applied_inside_the_drain() {
        let (lane, mut bridge) = FakeVerbLane::bridge(&[(1, "s-a"), (2, "s-b"), (3, "s-r")]);
        assert!(bridge.refresh_sessions().is_ok());
        let (socket, _broker, _server) = serve_log(&lane, "halt");
        bridge.cfg.broker = socket;
        bridge.conn = Some(bridge.connect().unwrap().0.into());
        let lane_of = format!("/f/f/in/{0}/s-r/{0}/note", bridge.node);
        bridge.indexed_subjects.insert(lane_of.clone());
        let mut frame = String::new();
        for n in 1..=4u64 {
            for sid in ["s-a", "s-b"] {
                frame.push_str(&queued_note(sid, n, "@s-r", &format!("{sid}-{n}")));
            }
        }
        *lane.outbox.lock().unwrap() = Some(frame);
        let generation = bridge.mailbox.broker_generation(Source::Fleet);
        let pushed = bridge.mailbox.push(
            Source::Fleet,
            (
                900,
                "/f/f/fleet/h-t/halt".to_string(),
                b"v=1 t=3 state=on reason=stop".to_vec(),
            ),
            generation,
        );
        assert!(matches!(pushed, Pushed::Queued));
        bridge.drain_outbox();

        assert!(bridge.conn.is_some(), "nothing here drops the connection");
        assert!(bridge.halt_applied, "the halt was applied by the drain");
        let holds: Vec<String> = lane
            .transcript()
            .into_iter()
            .filter(|line| line.starts_with("hold "))
            .collect();
        for sid in ["s-a", "s-b", "s-r"] {
            assert!(
                holds
                    .iter()
                    .any(|line| line.starts_with(&format!("hold {sid} on"))),
                "{sid} held: {holds:#?}"
            );
        }
        let retired = retirements(&lane);
        assert_eq!(
            retired.len(),
            8,
            "a halt holds PTY-reaching verbs, not the outbox: {retired:#?}"
        );
        let (mut reader, _) = bridge.connect().unwrap();
        assert_eq!(notes_on(&mut reader, &lane_of).len(), 8);
    }

    #[cfg(unix)]
    #[test]
    fn a_fresh_posts_unwritable_in_flight_entry_ends_the_drain_as_a_blocking_one_does() {
        for key in [false, true] {
            let (lane, mut bridge) = FakeVerbLane::bridge(&[(1, "s-a"), (3, "s-r")]);
            assert!(bridge.refresh_sessions().is_ok());
            let (socket, _broker, _server) = serve_log(&lane, "inflight");
            bridge.cfg.broker = socket;
            bridge.conn = Some(bridge.connect().unwrap().0.into());
            let own = format!("/f/f/in/{0}/s-r/{0}/note", bridge.node);
            let other = format!("/f/f/in/p/h-human/{}/note", bridge.node);
            bridge.indexed_subjects.insert(own.clone());
            bridge.indexed_subjects.insert(other.clone());
            let inflight = bridge.state.root().join("inflight");
            let _ = std::fs::remove_dir_all(&inflight);
            std::fs::write(&inflight, b"not a directory").unwrap();
            let keyed = |line: String, text: &str| {
                if key {
                    line.replacen(" kind=", &format!(" key={text} kind="), 1)
                } else {
                    line
                }
            };
            *lane.outbox.lock().unwrap() = Some(
                [
                    keyed(queued_note("s-a", 1, "@s-r", "a1"), "a1"),
                    keyed(queued_note("s-a", 2, "h-human", "a2"), "a2"),
                ]
                .concat(),
            );
            bridge.drain_outbox();

            assert!(bridge.conn.is_some(), "key={key}: the link stands");
            let (mut reader, _) = bridge.connect().unwrap();
            assert_eq!(
                notes_on(&mut reader, &own),
                [],
                "key={key}: a1 did not land"
            );
            assert_eq!(
                notes_on(&mut reader, &other),
                [],
                "key={key}: a2 never went out ahead of a1"
            );
            assert_eq!(
                retirements(&lane),
                [],
                "key={key}: nothing of s-a's retired"
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn a_reply_naming_a_pipelined_ask_is_judged_once_the_asks_answer_is_read() {
        let (lane, mut bridge) = FakeVerbLane::bridge(&[(1, "s-a")]);
        assert!(bridge.refresh_sessions().is_ok());
        let (socket, _broker, _server) = serve_log(&lane, "stray");
        bridge.cfg.broker = socket;
        assert!(bridge.attach_broker());
        bridge.state.pin("s-w", "n-far").expect("pin");
        bridge
            .connect()
            .unwrap()
            .0
            .publish(
                9_000,
                1,
                &subject::session_face("f", "n-far", "s-w", "presence"),
                b"v=1 t=1 inc=1 state=live",
            )
            .expect("n-far advertises s-w");
        let face = format!("/f/f/in/n-far/s-w/{}/ask", bridge.node);
        bridge.indexed_subjects.insert(face.clone());
        *lane.outbox.lock().unwrap() =
            Some("post sid=s-a id=1 to=@s-w@n-far kind=ask len=1\nq".to_string());
        bridge.in_round = true;
        bridge.drain_outbox();
        assert_eq!(
            bridge.conn.as_ref().unwrap().unacked(),
            1,
            "the ask's ack is left for the loop"
        );

        let (mut reader, _) = bridge.connect().unwrap();
        let asked = aterm_spec::wait::until_within(
            aterm_spec::wait::MUST_HAPPEN,
            "the ask on the log",
            || {
                let (rows, _) = reader.fetch(0, &face, 1).ok()?;
                rows.into_iter().next().map(|(off, _, _)| off)
            },
        );
        assert_eq!(
            bridge.reply_from(asked, "n-evil"),
            None,
            "the control: with the ask's ack unread, nothing says where it went"
        );
        let stray = format!("/f/f/in/{}/s-a/n-evil/answer", bridge.node);
        let mut body = Body::new(0);
        body.re = Some(asked);
        body.text = "forged".to_string();
        let off = asked + 1;
        bridge.on_inbox_record(&(off, stray, body.encode()));
        bridge.in_round = false;

        assert_eq!(
            bridge.reply_from(asked, "n-evil"),
            Some(false),
            "the ack was read first: the ask is on record as n-far's to answer"
        );
        let delivered: Vec<String> = lane
            .deliveries()
            .into_iter()
            .filter(|line| line.contains(&format!(" off={off} ")))
            .collect();
        assert_eq!(delivered.len(), 1, "{delivered:#?}");
        assert!(
            delivered[0].contains(" kind=note ") && delivered[0].contains(" demoted=answer"),
            "the stray delivered as what it is: {delivered:#?}"
        );
        assert!(retirements(&lane).contains(&("s-a".to_string(), 1, Some(asked))));
    }

    #[cfg(unix)]
    #[test]
    fn a_post_still_in_flight_at_the_next_drains_peek_lands_once() {
        let (lane, mut bridge) = FakeVerbLane::bridge(&[(1, "s-a"), (2, "s-r")]);
        assert!(bridge.refresh_sessions().is_ok());
        let (socket, _broker, _server) = serve_log(&lane, "peek");
        bridge.cfg.broker = socket;
        bridge.conn = Some(bridge.connect().unwrap().0.into());
        let lane_of = format!("/f/f/in/{0}/s-r/{0}/note", bridge.node);
        bridge.indexed_subjects.insert(lane_of.clone());
        lane.retire_from_outbox();
        *lane.outbox.lock().unwrap() = Some(queued_note("s-a", 1, "@s-r", "once"));
        bridge.in_round = true;
        bridge.drain_outbox();
        assert_eq!(
            bridge.conn.as_ref().unwrap().unacked(),
            1,
            "the note's ack is left for the loop"
        );
        assert!(
            lane.outbox
                .lock()
                .unwrap()
                .as_deref()
                .is_some_and(|frame| frame.contains("sid=s-a id=1 ")),
            "the control: the endpoint still lists the note in flight"
        );

        bridge.drain_outbox();
        bridge.in_round = false;
        bridge.settle_acks();

        let (mut reader, _) = bridge.connect().unwrap();
        let notes = notes_on(&mut reader, &lane_of);
        let texts: Vec<&str> = notes.iter().map(|(_, text)| text.as_str()).collect();
        assert_eq!(texts, ["once"], "one record: {notes:#?}");
        assert_eq!(
            retirements(&lane),
            [("s-a".to_string(), 1, Some(notes[0].0))],
            "retired once, at its own offset"
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_whole_drain_answers_the_drain_events_queued_before_its_peek_and_a_short_one_none() {
        let (lane, mut bridge) = FakeVerbLane::bridge(&[(1, "s-a"), (2, "s-b"), (3, "s-r")]);
        assert!(bridge.refresh_sessions().is_ok());
        let (socket, _broker, _server) = serve_log(&lane, "forget");
        bridge.cfg.broker = socket;
        bridge.conn = Some(bridge.connect().unwrap().0.into());
        let lane_of = format!("/f/f/in/{0}/s-r/{0}/note", bridge.node);
        bridge.indexed_subjects.insert(lane_of.clone());
        lane.retire_from_outbox();
        let queue = |bridge: &Bridge| {
            for line in [
                "EVENT 1 post 2",
                "EVENT 2 inbox 7 from=s-a kind=note off=40",
                "EVENT 2 post 1",
                "GAP 2 events-dropped=1",
                "EVENT 1 fetch off=9",
            ] {
                bridge.mailbox.push_event(line.to_string());
            }
        };
        let left = |bridge: &Bridge| {
            let mut left = Vec::new();
            while let Some(Item::Event(line)) = bridge.mailbox.take(Duration::ZERO) {
                left.push(line);
            }
            left
        };
        queue(&bridge);
        bridge.on_event("EVENT 2 inbox 6 from=s-a kind=note off=39");
        assert_eq!(
            left(&bridge).len(),
            5,
            "the control: an inbox event forgets nothing"
        );

        *lane.outbox.lock().unwrap() =
            Some(format!("{}more\n", queued_note("s-a", 1, "@s-r", "a1")));
        queue(&bridge);
        bridge.on_event("EVENT 1 post 1");
        assert_eq!(
            retirements(&lane).len(),
            1,
            "the post it was shown is published: {:?}",
            retirements(&lane)
        );
        assert_eq!(
            left(&bridge).len(),
            5,
            "a cut listing leaves every drain event for the next drain"
        );

        std::fs::write(bridge.state.root().join("fail-post-publish"), "s-b\n").unwrap();
        bridge.fault = Fault::FailPostPublishWhileMarked;
        *lane.outbox.lock().unwrap() = Some(queued_note("s-b", 1, "@s-r", "b1"));
        queue(&bridge);
        bridge.on_event("EVENT 2 post 1");
        assert!(
            bridge.held_senders.contains("s-b"),
            "the refusal holds its sender"
        );
        assert_eq!(
            left(&bridge).len(),
            5,
            "a held sender leaves every drain event for the next drain"
        );

        std::fs::remove_file(bridge.state.root().join("fail-post-publish")).unwrap();
        queue(&bridge);
        bridge.on_event("EVENT 2 post 1");
        assert_eq!(
            retirements(&lane)
                .iter()
                .filter(|(sid, _, off)| sid == "s-b" && off.is_some())
                .count(),
            1,
            "b1 landed: {:?}",
            retirements(&lane)
        );
        assert_eq!(
            left(&bridge),
            [
                "EVENT 2 inbox 7 from=s-a kind=note off=40",
                "GAP 2 events-dropped=1"
            ]
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_refused_pipelined_publish_is_its_own_records_answer() {
        let (lane, mut bridge) = FakeVerbLane::bridge(&[(1, "s-a")]);
        assert!(bridge.refresh_sessions().is_ok());
        let max = astream_broker::store::MAX_SUBJECTS_PER_PRODUCER as u64;
        let (_broker, _server) = seeded_broker(&lane, &mut bridge, &["s-a"], &[], max);
        bridge
            .per_sid
            .presence
            .entry("s-new".to_string())
            .or_default()
            .sampled = true;
        bridge.batched(|bridge| {
            bridge.publish_ev_for(Some("s-a"), "one");
            bridge.publish_session_presence("s-new", "live");
            bridge.publish_ev_for(Some("s-a"), "two");
            assert_eq!(bridge.conn.as_ref().unwrap().unacked(), 3, "all in flight");
        });
        assert!(bridge.conn.is_some(), "a refusal leaves the link up");
        assert_eq!(
            bridge.per_sid.presence["s-new"].due(bridge.cfg.presence, Instant::now()),
            crate::presence::Due::First,
            "the refused row is not on the bus"
        );
        let (mut reader, _) = bridge.connect().unwrap();
        let face = subject::session_face("f", &bridge.node, "s-a", "ev");
        let (rows, _) = reader.fetch(0, &face, 64).unwrap();
        let evs: Vec<String> = rows
            .iter()
            .filter(|(_, s, _)| *s == face)
            .filter_map(|(_, _, raw)| {
                let ev = Body::decode(raw).0.unknown.get("ev")?.clone();
                Some(crate::pct::decode(&ev))
            })
            .collect();
        assert_eq!(evs, ["one", "two"]);
    }

    #[cfg(unix)]
    #[test]
    fn a_pipelined_halt_ack_moves_the_watermark_once_it_lands_and_goes_once() {
        let (lane, mut bridge) = FakeVerbLane::bridge(&[]);
        let (socket, _broker, _server) = serve_log(&lane, "ack");
        bridge.cfg.broker = socket;
        bridge.conn = Some(bridge.connect().unwrap().0.into());
        bridge.in_round = true;
        bridge.ack_halt(41);
        bridge.ack_halt(41);
        assert_eq!(bridge.conn.as_ref().unwrap().unacked(), 1, "sent once");
        assert_eq!(bridge.state.halt_acked(), None, "not before it lands");
        bridge.in_round = false;
        bridge.settle_acks();
        assert_eq!(bridge.state.halt_acked(), Some(41));
        let (mut reader, _) = bridge.connect().unwrap();
        let face = subject::node_face("f", &bridge.node, "ack");
        let (rows, _) = reader.fetch(0, &face, 8).unwrap();
        assert_eq!(
            rows.iter()
                .map(|(_, _, b)| Body::decode(b).0.re)
                .collect::<Vec<_>>(),
            [Some(41)]
        );
    }

    #[test]
    fn sequences_are_reserved_a_block_at_a_time_and_a_restart_never_reuses_one() {
        let (_lane, mut bridge) = FakeVerbLane::bridge(&[]);
        let state = bridge.state.root().to_path_buf();
        let first = bridge.next_seq().unwrap();
        let end = bridge.state.sequence();
        assert_eq!(end, first + SEQ_BLOCK - 1, "one write reserves the block");
        let mut handed = vec![first];
        for _ in 1..SEQ_BLOCK {
            handed.push(bridge.next_seq().unwrap());
            assert_eq!(bridge.state.sequence(), end, "no write inside the block");
        }
        handed.push(bridge.next_seq().unwrap());
        assert_eq!(bridge.state.sequence(), end + SEQ_BLOCK, "the next block");
        for _ in 0..3 {
            handed.push(bridge.next_seq().unwrap());
        }
        assert!(
            handed.windows(2).all(|w| w[0] + 1 == w[1]),
            "gapless while alive"
        );
        let inc = bridge.inc;
        drop(bridge);
        let (_again_lane, mut again) = FakeVerbLane::bridge_with_state(&[], Some(&state));
        again.inc = inc;
        let resumed = again.next_seq().unwrap();
        assert_eq!(
            resumed,
            end + SEQ_BLOCK + 1,
            "above the dead block's end: its unused rest is burned"
        );
        assert!(handed.iter().all(|seq| *seq < resumed), "nothing is reused");
    }

    #[cfg(unix)]
    #[test]
    fn a_log_switch_rechecks_a_cached_broadcast_against_the_new_subject_budget() {
        let (lane, mut bridge) = FakeVerbLane::bridge(&[(1, "s-a")]);
        assert!(bridge.refresh_sessions().is_ok());
        let (a, _broker_a, _server_a) = serve_log(&lane, "say-a");
        bridge.cfg.broker = a;
        assert!(bridge.attach_broker());
        *lane.outbox.lock().unwrap() =
            Some("post sid=s-a id=1 to=say:known kind=note len=3\nold".to_string());
        bridge.drain_outbox();
        let subject = format!("/f/f/pub/{}/s-a/say/known", bridge.node);
        assert!(bridge.indexed_subjects.contains(&subject));
        bridge.conn = None;
        assert!(bridge.attach_broker());
        assert!(
            bridge.indexed_subjects.contains(&subject),
            "same-log cache stays"
        );

        bridge.close_subscriptions();
        bridge.conn = None;
        let max = astream_broker::store::MAX_SUBJECTS_PER_PRODUCER as u64;
        let held = max - max / SAY_RESERVE_DIVISOR;
        let (_broker_b, _server_b) = seeded_broker(&lane, &mut bridge, &["s-a"], &[], held);
        *lane.outbox.lock().unwrap() =
            Some("post sid=s-a id=2 to=say:known kind=note len=3\nnew".to_string());
        bridge.drain_outbox();
        assert!(
            lane.sent
                .lock()
                .unwrap()
                .iter()
                .any(|line| line == "outbox sent s-a 2 off=- reason=subject-budget"),
            "A's cache spent B's reserve: {:?}",
            lane.sent.lock().unwrap()
        );
        let (mut reader, _) = bridge.connect().unwrap();
        assert!(reader.fetch(0, &subject, 8).unwrap().0.is_empty());
        assert_eq!(
            reader
                .subject_budget(bridge.producer_id)
                .unwrap()
                .unwrap()
                .subjects,
            held
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_log_switch_waits_until_every_old_offset_directory_can_be_read() {
        const SID: &str = "s-kept";
        for blocked in ["seen", "topics"] {
            let (lane, mut bridge) = FakeVerbLane::bridge(&[(1, SID)]);
            assert!(bridge.refresh_sessions().is_ok());
            let (a, broker_a, _server_a) = serve_log(&lane, "read-a");
            bridge.cfg.broker = a;
            assert!(bridge.attach_broker());
            bridge.state.set_seen_off(SID, 40).unwrap();
            bridge
                .state
                .set_topics(
                    SID,
                    &BTreeMap::from([("build".to_string(), TopicCursor { next: 41 })]),
                )
                .unwrap();
            let area = bridge.state.root().join(blocked);
            let saved = bridge.state.root().join(format!("{blocked}-saved"));
            std::fs::rename(&area, &saved).unwrap();
            std::fs::write(&area, b"not a directory").unwrap();
            let (b, broker_b, _server_b) = serve_log(&lane, "read-b");
            bridge.close_subscriptions();
            bridge.conn = None;
            bridge.cfg.broker = b;
            assert!(
                !bridge.attach_broker(),
                "unreadable {blocked} was treated as empty"
            );
            assert_eq!(
                bridge.state.log_witness().unwrap().id,
                Some(broker_a.log_identity())
            );
            assert_eq!(std::fs::read(&area).unwrap(), b"not a directory");
            assert!(saved.join(SID).is_file(), "the old offset was preserved");

            std::fs::remove_file(&area).unwrap();
            std::fs::rename(&saved, &area).unwrap();
            assert!(bridge.attach_broker(), "readable {blocked} can be retried");
            assert_eq!(
                bridge.state.log_witness().unwrap().id,
                Some(broker_b.log_identity())
            );
            assert_eq!(bridge.state.seen_off(SID), None);
        }
    }

    #[cfg(unix)]
    #[test]
    fn a_log_switch_waits_until_every_listed_topic_file_can_be_read() {
        const SID: &str = "s-kept";
        let (lane, mut bridge) = FakeVerbLane::bridge(&[(1, SID)]);
        assert!(bridge.refresh_sessions().is_ok());
        let (a, broker_a, _server_a) = serve_log(&lane, "entry-a");
        bridge.cfg.broker = a;
        assert!(bridge.attach_broker());
        bridge.state.set_seen_off(SID, 40).unwrap();
        let topic = bridge.state.root().join("topics").join(SID);
        std::fs::create_dir_all(topic.parent().unwrap()).unwrap();
        std::os::unix::fs::symlink(lane.dir.path().join("missing-topic"), &topic).unwrap();
        let (b, broker_b, _server_b) = serve_log(&lane, "entry-b");
        bridge.close_subscriptions();
        bridge.conn = None;
        bridge.cfg.broker = b;
        assert!(
            !bridge.attach_broker(),
            "a listed unreadable cursor was treated as empty"
        );
        assert_eq!(
            bridge.state.log_witness().unwrap().id,
            Some(broker_a.log_identity())
        );
        assert_eq!(
            bridge.state.seen_off(SID),
            Some(40),
            "preflight changed nothing"
        );
        assert!(
            topic.is_symlink(),
            "the unreadable entry was not overwritten"
        );

        std::fs::remove_file(&topic).unwrap();
        bridge
            .state
            .set_topics(
                SID,
                &BTreeMap::from([("build".to_string(), TopicCursor { next: 41 })]),
            )
            .unwrap();
        assert!(bridge.attach_broker());
        assert_eq!(
            bridge.state.log_witness().unwrap().id,
            Some(broker_b.log_identity())
        );
        assert_eq!(bridge.state.seen_off(SID), None);
    }

    #[cfg(unix)]
    #[test]
    fn a_log_transition_requires_the_endpoints_ack_before_refill() {
        const SID: &str = "s-kept";
        for (index, reply) in [
            "ERR busy\n",
            "ERR usage link\n",
            "ERR denied\n",
            "OK stale=1\n",
        ]
        .into_iter()
        .enumerate()
        {
            let (lane, mut bridge) = FakeVerbLane::bridge(&[(1, SID)]);
            lane.answer_hello_with(&format!("OK lane={LINK_LOG_LANE} build=test\n"));
            bridge.introduce();
            assert!(bridge.refresh_sessions().is_ok());
            let (socket, broker, _server) = serve_log(&lane, "log-ack");
            bridge.cfg.broker = socket;
            let (mut writer, _) = bridge.connect().unwrap();
            let subject = format!("/f/f/in/{}/{SID}/h-writer/note", bridge.node);
            let (off, _) = writer
                .publish(81, 1, &subject, b"v=1 t=1 text=mail")
                .unwrap();
            let head = broker.head();
            *lane.log_answer.lock().unwrap() = reply.to_string();
            let attached = bridge.attach_broker();
            assert!(
                lane.delivery_offsets().is_empty(),
                "{reply:?} allowed an unrebased refill"
            );
            assert!(!attached, "{reply:?} acknowledged no transition");
            assert_eq!(
                broker.head(),
                head,
                "a failed clock barrier published presence"
            );
            assert!(bridge.per_sid.pending_admit.contains(SID));
            assert_eq!(bridge.state.seen_off(SID), None);

            *lane.log_answer.lock().unwrap() = if index % 2 == 0 {
                "OK\n"
            } else {
                "OK forgot=1\n"
            }
            .to_string();
            assert!(
                bridge.attach_broker(),
                "an acknowledged transition can finish"
            );
            assert_eq!(lane.delivery_offsets(), [off]);
            assert!(!bridge.per_sid.pending_admit.contains(SID));
        }
    }

    #[cfg(unix)]
    #[test]
    fn a_log_transition_with_a_lost_endpoint_publishes_nothing() {
        let (lane, mut bridge) = FakeVerbLane::bridge(&[(1, "s-kept")]);
        lane.answer_hello_with(&format!("OK lane={LINK_LOG_LANE} build=test\n"));
        bridge.introduce();
        assert!(bridge.refresh_sessions().is_ok());
        let (socket, broker, _server) = serve_log(&lane, "log-lost");
        bridge.cfg.broker = socket;
        let head = broker.head();
        lane.log_answer.lock().unwrap().clear();
        let attached = bridge.attach_broker();
        assert_eq!(
            broker.head(),
            head,
            "a dead endpoint's attach wrote to the broker"
        );
        assert!(!attached);
        assert!(bridge.aterm_gone());
        assert!(lane.delivery_offsets().is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn a_log_transition_is_not_sent_to_an_endpoint_below_lane_two() {
        const SID: &str = "s-kept";
        for endpoint_lane in [0, LINK_LOG_LANE - 1] {
            let (lane, mut bridge) = FakeVerbLane::bridge(&[(1, SID)]);
            lane.answer_hello_with(&format!("OK lane={endpoint_lane} build=old\n"));
            bridge.introduce();
            assert!(bridge.refresh_sessions().is_ok());
            let (socket, _broker, _server) = serve_log(&lane, "old-lane");
            bridge.cfg.broker = socket;
            let (mut writer, _) = bridge.connect().unwrap();
            let subject = format!("/f/f/in/{}/{SID}/h-writer/note", bridge.node);
            let (off, _) = writer
                .publish(81, 1, &subject, b"v=1 t=1 text=mail")
                .unwrap();
            lane.log_answer.lock().unwrap().clear();
            assert!(bridge.attach_broker());
            assert_eq!(lane.delivery_offsets(), [off]);
            assert!(!lane
                .links()
                .iter()
                .any(|line| line.starts_with("link log ")));
        }
    }

    #[cfg(unix)]
    #[test]
    fn a_record_decided_on_one_log_is_never_published_on_another() {
        const SID: &str = "s-decider";
        let (lane, mut bridge) = FakeVerbLane::bridge(&[(1, SID)]);
        assert!(bridge.refresh_sessions().is_ok());
        let node = bridge.node.clone();
        let lanes = format!("/f/f/in/{node}/>");
        let expired_face = format!("/f/f/in/{node}/{SID}/{node}/expired");
        let expired = |re: u64| {
            let mut body = Body::new(1);
            body.re = Some(re);
            body.dl = Some(1_000);
            body.encode()
        };
        let cut = |bridge: &mut Bridge| {
            let (conn, closer) = bridge.connect().unwrap();
            bridge.conn = Some(conn.into());
            bridge.read_log_identity().unwrap();
            closer.close();
        };

        let (a, broker_a, _server_a) = serve_log(&lane, "a");
        bridge.cfg.broker = a;
        assert!(bridge.attach_broker());
        let owed = OwedReceipt {
            sid: SID.to_string(),
            rid: 1,
            off: 5,
            verdict: "handled".to_string(),
            kind: "ask".to_string(),
            from: format!("s-asker@{node}"),
            earlier_log: false,
        };
        cut(&mut bridge);
        assert!(
            bridge.send_receipt(&owed).is_err(),
            "the receipt stays owed"
        );
        cut(&mut bridge);
        assert!(bridge.publish_own_off(&expired_face, &expired(5)).is_err());
        let journal = bridge.state.in_flight().unwrap();
        assert_eq!(journal.len(), 2, "{journal:#?}");
        assert!(
            journal
                .iter()
                .all(|(_, e)| e.log == Some(broker_a.log_identity())),
            "the journal names A: {journal:#?}"
        );

        let (b, broker_b, _server_b) = serve_log(&lane, "b");
        bridge.close_subscriptions();
        bridge.conn = None;
        bridge.cfg.broker = b;
        assert!(bridge.attach_broker());
        assert!(bridge.state.in_flight().unwrap().is_empty());
        let (mut reader, _) = bridge.connect().unwrap();
        let on_lanes = |reader: &mut Conn| -> Vec<(u64, String, String)> {
            let (rows, _) = reader.fetch(0, &lanes, 64).unwrap();
            rows.into_iter()
                .map(|(off, s, b)| (off, s, String::from_utf8_lossy(&b).into_owned()))
                .collect()
        };
        let rows = on_lanes(&mut reader);
        assert!(
            rows.is_empty(),
            "A's decisions reached B's lanes: {rows:#?}"
        );
        assert!(
            node_evs(&bridge)
                .iter()
                .any(|e| e.contains("ev=log-replaced head=") && e.contains(" inflight=2 ")),
            "{:#?}",
            node_evs(&bridge)
        );

        let before = lane.transcript().len();
        bridge
            .send_receipt(&OwedReceipt {
                earlier_log: true,
                ..owed.clone()
            })
            .unwrap();
        let rows = on_lanes(&mut reader);
        assert!(rows.is_empty(), "A's receipt reached B: {rows:#?}");
        assert_eq!(lane.transcript()[before..], ["deliver -".to_string()]);
        assert_eq!(bridge.state.receipt_pin(SID, 1).unwrap(), None);
        let ev_face = subject::session_face("f", &node, SID, "ev");
        let (rows, _) = reader.fetch(0, &ev_face, 64).unwrap();
        let said: Vec<String> = rows
            .iter()
            .map(|(_, _, b)| crate::pct::decode(&String::from_utf8_lossy(b)))
            .collect();
        assert!(
            said.iter()
                .any(|e| e.contains("ev=unacked re=5 verdict=handled reason=log-replaced")),
            "{said:#?}"
        );

        cut(&mut bridge);
        assert!(bridge.publish_own_off(&expired_face, &expired(7)).is_err());
        assert_eq!(
            bridge.state.in_flight().unwrap()[0].1.log,
            Some(broker_b.log_identity())
        );
        bridge.close_subscriptions();
        bridge.conn = None;
        assert!(bridge.attach_broker());
        let rows = on_lanes(&mut reader);
        assert_eq!(rows.len(), 1, "{rows:#?}");
        assert_eq!(Body::decode(rows[0].2.as_bytes()).0.re, Some(7));
        assert!(bridge.self_acked.contains(&rows[0].0), "and it is ours");
        assert!(bridge.state.in_flight().unwrap().is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn a_log_restored_under_its_own_identity_replays_no_record_decided_before_it() {
        const SID: &str = "s-decider";
        let (lane, mut bridge) = FakeVerbLane::bridge(&[(1, SID)]);
        assert!(bridge.refresh_sessions().is_ok());
        let node = bridge.node.clone();
        let expired_face = format!("/f/f/in/{node}/{SID}/{node}/expired");
        let mut expired = Body::new(1);
        expired.re = Some(500);
        expired.dl = Some(1_000);
        let (a, broker, _server) = serve_log(&lane, "a");
        bridge.cfg.broker = a;
        assert!(bridge.attach_broker());
        let (mut conn, closer) = bridge.connect().unwrap();
        assert_eq!(conn.log_identity().unwrap(), Some(broker.log_identity()));
        bridge.conn = Some(conn.into());
        closer.close();
        assert!(bridge
            .publish_own_off(&expired_face, &expired.encode())
            .is_err());
        let journal = bridge.state.in_flight().unwrap();
        assert_eq!(journal.len(), 1, "{journal:#?}");
        assert_eq!(journal[0].1.log, Some(broker.log_identity()));

        bridge.bus_high = 1_000;
        bridge.close_subscriptions();
        bridge.conn = None;
        assert!(!bridge.attach_broker());
        let retained = bridge.state.in_flight().unwrap();
        assert_eq!(retained.len(), 1);
        assert_eq!(retained[0].1.body, journal[0].1.body);
        assert_eq!(
            bridge.state.rewound_log().unwrap(),
            Some(broker.log_identity())
        );
        let (mut reader, _) = bridge.connect().unwrap();
        let (rows, _) = reader.fetch(0, &format!("/f/f/in/{node}/>"), 64).unwrap();
        assert!(
            rows.is_empty(),
            "a record decided before the restore reached it: {rows:#?}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn an_observer_answers_no_halt() {
        for (attachment, answers) in [(Attachment::Inherited, true), (Attachment::Observer, false)]
        {
            let (lane, mut bridge) = FakeVerbLane::bridge(&[(1, "s-s")]);
            let (socket, _broker, _server) = serve_log(&lane, "broker");
            bridge.cfg.broker = socket;
            bridge.conn = Some(bridge.connect().unwrap().0.into());
            bridge.attachment = attachment;
            let halt = (
                7,
                "/f/f/fleet/h-x/halt".to_string(),
                b"v=1 t=1 state=on".to_vec(),
            );
            bridge.on_fleet_record(&halt);
            assert!(bridge.halt_applied, "{attachment:?} records the transition");
            let ack_face = subject::node_face("f", &bridge.node, "ack");
            let (acks, _) = bridge.conn_now().unwrap().last(&ack_face, "", 8).unwrap();
            assert_eq!(!acks.is_empty(), answers, "{attachment:?}: {acks:?}");
            assert_eq!(
                bridge.state.halt_acked().is_some(),
                answers,
                "{attachment:?}"
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn a_halt_the_verb_lane_died_under_is_answered_by_the_bridge_that_applies_it() {
        const SID: &str = "s-held";
        const HALT: &str = "/f/f/fleet/h-x/halt";
        let (lane, mut bridge) = FakeVerbLane::bridge(&[(1, SID)]);
        let (socket, _broker, _server) = serve_log(&lane, "broker");
        bridge.cfg.broker = socket.clone();
        bridge.conn = Some(bridge.connect().unwrap().0.into());
        let (mut human, _) = bridge.connect().unwrap();
        let ack_face = subject::node_face("f", &bridge.node, "ack");
        let answers = |bridge: &Bridge| -> Vec<String> {
            let (mut reader, _) = bridge.connect().unwrap();
            let (rows, _) = reader.last(&ack_face, "", 8).unwrap();
            rows.iter()
                .map(|(_, _, body)| String::from_utf8_lossy(body).into_owned())
                .collect()
        };
        let names = |rows: &[String], re: u64, state: Option<&str>| {
            rows.iter().any(|row| {
                let mut words = row.split_whitespace();
                words.clone().any(|w| w == format!("re={re}"))
                    && state.is_none_or(|state| words.any(|w| w == format!("state={state}")))
            })
        };

        let on = b"v=1 t=1 state=on reason=stop";
        let (on_at, _) = human.publish(91, 1, HALT, on).unwrap();
        bridge.on_fleet_record(&(on_at, HALT.to_string(), on.to_vec()));
        assert!(bridge.halt_applied);
        assert_eq!(bridge.state.halt_acked(), Some(on_at));
        let rows = answers(&bridge);
        assert!(names(&rows, on_at, Some("held")), "{rows:?}");

        let off = b"v=1 t=2 state=off";
        let (lift, _) = human.publish(91, 2, HALT, off).unwrap();
        lane.hang_up_on_hold();
        bridge.on_fleet_record(&(lift, HALT.to_string(), off.to_vec()));
        assert!(bridge.ctl.lost(), "the lane closed under the `hold`");
        assert!(
            lane.transcript()
                .contains(&format!("hold {SID} off origin=fleet")),
            "{:#?}",
            lane.transcript()
        );
        assert!(bridge.halt_applied, "the belief is still the previous one");
        assert_eq!(
            bridge.state.halt_acked(),
            Some(on_at),
            "a barrier this bridge did not apply was marked answered"
        );
        let rows = answers(&bridge);
        assert!(
            names(&rows, on_at, Some("held")) && !names(&rows, lift, None),
            "the dying bridge answered the lift: {rows:?}"
        );

        let state_dir = bridge.state.root().to_path_buf();
        drop(bridge);
        let (next_lane, mut successor) =
            FakeVerbLane::bridge_with_state(&[(1, SID)], Some(&state_dir));
        successor.cfg.broker = socket;
        assert!(successor.refresh_sessions().is_ok());
        assert!(successor.attach_broker());
        assert!(!successor.halt_applied);
        assert!(
            next_lane
                .transcript()
                .contains(&format!("hold {SID} off origin=fleet")),
            "{:#?}",
            next_lane.transcript()
        );
        assert_eq!(successor.state.halt_acked(), Some(lift));
        let rows = answers(&successor);
        assert!(names(&rows, lift, Some("ready")), "{rows:?}");
    }

    #[cfg(unix)]
    fn halt_answers(bridge: &Bridge) -> Vec<Body> {
        let (mut reader, _) = bridge.connect().unwrap();
        let ack_face = subject::node_face("f", &bridge.node, "ack");
        let (rows, _) = reader.fetch(0, &ack_face, 256).unwrap();
        rows.into_iter()
            .map(|(_, _, raw)| Body::decode(&raw).0)
            .collect()
    }

    #[cfg(unix)]
    #[test]
    fn a_busy_hold_is_answered_only_after_a_later_roster_round_reaches_it() {
        const FIRST: &str = "s-first";
        const LATER: &str = "s-later";
        const HALT: &str = "/f/f/fleet/h-x/halt";
        for on in [true, false] {
            let (lane, mut bridge) = FakeVerbLane::bridge(&[(1, FIRST), (2, LATER)]);
            let (socket, _broker, _server) = serve_log(&lane, "broker");
            bridge.cfg.broker = socket;
            bridge.conn = Some(bridge.connect().unwrap().0.into());
            assert!(bridge.refresh_sessions().is_ok());
            bridge.per_sid.pending_admit.clear();
            let (mut human, _) = bridge.connect().unwrap();
            if !on {
                let body = b"v=1 t=1 state=on reason=stop";
                let (off, _) = human.publish(91, 1, HALT, body).unwrap();
                bridge.on_fleet_record(&(off, HALT.into(), body.to_vec()));
                assert!(lane.holds(FIRST) && lane.holds(LATER));
            }
            let previous_ack = bridge.state.halt_acked();
            lane.refuse_hold(FIRST, Some("ERR busy update-committing"));
            let state = if on { "on" } else { "off" };
            let body = format!("v=1 t=2 state={state} reason=stop").into_bytes();
            let (barrier, _) = human.publish(91, 2, HALT, &body).unwrap();
            let before = lane.transcript().len();
            bridge.on_fleet_record(&(barrier, HALT.into(), body));
            assert_eq!(lane.holds(FIRST), !on, "a refused hold moved");
            assert_eq!(lane.holds(LATER), on, "the refusal skipped later sessions");
            assert!(lane.transcript()[before..]
                .iter()
                .any(|line| { line.starts_with(&format!("hold {LATER} {state} ")) }));
            assert_eq!(
                bridge.state.halt_acked(),
                previous_ack,
                "an unconfirmed transition was marked answered"
            );
            assert!(!halt_answers(&bridge)
                .iter()
                .any(|row| row.re == Some(barrier)));

            let endpoint = if on {
                EndpointHold::Fleet
            } else {
                EndpointHold::Free
            };
            bridge.converge_hold(LATER, endpoint);
            assert_eq!(
                lane.holds(LATER),
                on,
                "convergence rolled back the new intent"
            );
            bridge.roster_backstop();
            assert_eq!(bridge.state.halt_acked(), previous_ack);
            lane.refuse_hold(FIRST, None);
            bridge.roster_backstop();
            assert_eq!(lane.holds(FIRST), on);
            assert_eq!(lane.holds(LATER), on);
            assert_eq!(bridge.state.halt_acked(), Some(barrier));
            let expected = if on { "held" } else { "ready" };
            let answers = halt_answers(&bridge);
            let answer = answers.iter().find(|row| row.re == Some(barrier)).unwrap();
            assert_eq!(
                answer.unknown.get("state").map(String::as_str),
                Some(expected)
            );
            bridge.roster_backstop();
            assert_eq!(
                halt_answers(&bridge)
                    .iter()
                    .filter(|row| row.re == Some(barrier))
                    .count(),
                1
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn only_an_absent_session_satisfies_a_halt_without_an_ok_hold() {
        for (reply, reached) in [("ERR no such session", true), ("ERR denied", false)] {
            let (lane, mut bridge) = FakeVerbLane::bridge(&[(1, "s-first"), (2, "s-later")]);
            let (socket, _broker, _server) = serve_log(&lane, "broker");
            bridge.cfg.broker = socket;
            bridge.conn = Some(bridge.connect().unwrap().0.into());
            lane.refuse_hold("s-first", Some(reply));
            let (mut human, _) = bridge.connect().unwrap();
            let body = b"v=1 t=1 state=on reason=stop";
            let subject = "/f/f/fleet/h-x/halt";
            let (barrier, _) = human.publish(91, 1, subject, body).unwrap();
            bridge.on_fleet_record(&(barrier, subject.into(), body.to_vec()));
            assert!(
                lane.holds("s-later"),
                "later sessions were skipped after {reply}"
            );
            assert_eq!(
                bridge.state.halt_acked(),
                reached.then_some(barrier),
                "{reply}"
            );
            assert_eq!(
                halt_answers(&bridge)
                    .iter()
                    .any(|row| row.re == Some(barrier)),
                reached
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn an_unread_roster_leaves_its_halt_owed_until_a_successful_round() {
        let (lane, mut bridge) = FakeVerbLane::bridge(&[(1, "s-first"), (2, "s-later")]);
        let (socket, _broker, _server) = serve_log(&lane, "broker");
        bridge.cfg.broker = socket;
        bridge.conn = Some(bridge.connect().unwrap().0.into());
        assert!(bridge.refresh_sessions().is_ok());
        bridge.per_sid.pending_admit.clear();
        lane.refuse_roster(true);
        let (mut human, _) = bridge.connect().unwrap();
        let body = b"v=1 t=1 state=on reason=stop";
        let subject = "/f/f/fleet/h-x/halt";
        let (barrier, _) = human.publish(91, 1, subject, body).unwrap();
        bridge.on_fleet_record(&(barrier, subject.into(), body.to_vec()));
        assert!(lane.holds("s-first") && lane.holds("s-later"));
        assert_eq!(bridge.state.halt_acked(), None);
        bridge.converge_hold("s-later", EndpointHold::Fleet);
        assert!(
            lane.holds("s-later"),
            "unread roster rolled back a standing halt"
        );
        bridge.roster_backstop();
        assert_eq!(bridge.state.halt_acked(), None);
        lane.refuse_roster(false);
        bridge.roster_backstop();
        assert_eq!(bridge.state.halt_acked(), Some(barrier));
        assert!(halt_answers(&bridge)
            .iter()
            .any(|row| row.re == Some(barrier)));
    }

    #[cfg(unix)]
    #[test]
    fn convergence_never_lifts_the_owners_local_hold() {
        const SID: &str = "s-held";
        let (lane, mut bridge) = FakeVerbLane::bridge(&[(1, SID)]);
        let (socket, _broker, _server) = serve_log(&lane, "broker");
        bridge.cfg.broker = socket;
        bridge.conn = Some(bridge.connect().unwrap().0.into());
        assert!(bridge.refresh_sessions().is_ok());
        bridge.per_sid.pending_admit.clear();
        let row = |origin: &str| {
            format!(
                "1 {SID} {:032x} sid=1 revision=1 hold=1 hold_origin={origin} detail=- agent=-",
                1
            )
        };
        let writes = |lane: &FakeVerbLane| -> Vec<String> {
            let hold = format!("hold {SID} ");
            let transcript = lane.transcript();
            transcript
                .into_iter()
                .filter(|line| line.starts_with(&hold))
                .collect()
        };

        lane.answer_status_with(&[row("local")]);
        bridge.sample_local_control();
        bridge.on_event("EVENT 1 hold 1 reason=mine origin=local");
        assert_eq!(
            writes(&lane),
            Vec::<String>::new(),
            "the owner's hold was lifted"
        );

        lane.answer_status_with(&[row("fleet")]);
        bridge.sample_local_control();
        bridge.on_event("EVENT 1 hold 1 reason=fabric-lost origin=fleet");
        let lift = format!("hold {SID} off origin=fleet");
        assert_eq!(writes(&lane), [lift.clone(), lift.clone()]);

        let (mut human, _) = bridge.connect().unwrap();
        let halt = "/f/f/fleet/h-x/halt";
        let body = b"v=1 t=1 state=on reason=stop";
        let (off, _) = human.publish(91, 1, halt, body).unwrap();
        bridge.on_fleet_record(&(off, halt.into(), body.to_vec()));
        let held = format!("hold {SID} on reason=stop origin=fleet");
        let before = writes(&lane);
        assert_eq!(before.last(), Some(&held), "the halt reached the session");
        lane.answer_status_with(&[row("local")]);
        bridge.sample_local_control();
        assert_eq!(&writes(&lane)[before.len()..], std::slice::from_ref(&held));
        let before = writes(&lane);
        lane.answer_status_with(&[row("fleet,local")]);
        bridge.sample_local_control();
        bridge.on_event("EVENT 1 hold 1 reason=stop origin=fleet hold_origin=fleet,local");
        assert_eq!(writes(&lane)[before.len()..], [] as [String; 0]);

        let off_body = b"v=1 t=2 state=off";
        let (off, _) = human.publish(91, 2, halt, off_body).unwrap();
        bridge.on_fleet_record(&(off, halt.into(), off_body.to_vec()));
        assert_eq!(writes(&lane).last(), Some(&lift), "the fleet's lift");
        let before = writes(&lane);
        lane.answer_status_with(&[row("local")]);
        bridge.sample_local_control();
        bridge.on_event("EVENT 1 hold 1 reason=mine origin=local hold_origin=local");
        assert_eq!(
            writes(&lane)[before.len()..],
            [] as [String; 0],
            "the owner's stop outlives the fleet's halt"
        );
        bridge.on_event("EVENT 1 hold 1 reason=stop origin=fleet hold_origin=fleet,local");
        assert_eq!(writes(&lane)[before.len()..], [lift]);
    }

    #[cfg(unix)]
    #[test]
    fn an_observer_leaves_the_nodes_inbox_group_where_it_found_it() {
        const SID: &str = "s-watched";
        let (lane, mut bridge) = FakeVerbLane::bridge(&[(1, SID)]);
        bridge.attachment = Attachment::Observer;
        let (socket, _broker, _server) = serve_log(&lane, "broker");
        bridge.cfg.broker = socket;
        let (mut writer, _) = bridge.connect().unwrap();
        let mail = format!("/f/f/in/{}/{SID}/h-writer/note", bridge.node);
        let (old, _) = writer.publish(81, 1, &mail, b"v=1 t=1 text=old").unwrap();
        assert!(bridge.refresh_sessions().is_ok());
        assert!(bridge.attach_broker());
        assert!(!bridge.per_sid.pending_admit.contains(SID));
        assert!(
            !node_evs(&bridge)
                .iter()
                .any(|e| e.contains("reason=observer")),
            "an observer refilled its node's mail"
        );

        let (live, _) = writer.publish(81, 2, &mail, b"v=1 t=2 text=live").unwrap();
        let rec = match bridge.mailbox.take(aterm_spec::wait::MUST_HAPPEN) {
            Some(Item::Inbox(rec)) => rec,
            other => panic!("the observer's lane reader: {other:?}"),
        };
        assert_eq!(rec.0, live);
        bridge.on_inbox_record(&rec);
        assert!(
            node_evs(&bridge)
                .iter()
                .any(|e| e.contains(&format!("undeliverable off={live} reason=observer"))),
            "live mail gets its one honest record"
        );
        assert_eq!(lane.delivery_offsets(), [], "and nothing is delivered");

        let (consumer, closer) = bridge.connect().unwrap();
        closer
            .set_read_timeout(Some(aterm_spec::wait::MUST_HAPPEN))
            .unwrap();
        let group = subject::inbox_group("f", &bridge.node);
        let mut sub = consumer
            .subscribe_group(&group, &subject::inbox_filter("f", &bridge.node))
            .unwrap();
        let first = next_delivery(&mut sub, "the group's first record");
        assert_eq!(first.0, old, "the observer moved the node's cursor");
    }

    #[cfg(unix)]
    #[test]
    fn an_observer_on_a_copy_of_a_live_nodes_state_dir_is_refused() {
        let (lane, mut bridge) = FakeVerbLane::bridge(&[(1, "s-copy")]);
        bridge.attachment = Attachment::Observer;
        let (socket, _broker, _server) = serve_log(&lane, "broker");
        bridge.cfg.broker = socket;
        let (mut node, _) = bridge.connect().unwrap();
        let face = subject::node_face("f", &bridge.node, "presence");
        let host = crate::pct::encode(&hostname());
        let holder = std::os::unix::process::parent_id();
        let live = format!("v=1 t=1 state=live inc=4 fabric=connected host={host} pid={holder}");
        node.publish(77, 1, &face, live.as_bytes()).unwrap();
        assert!(!bridge.attach_broker());
        let refused = bridge.refused.take().expect("refused, not retried");
        assert_eq!(refused.kind(), io::ErrorKind::AlreadyExists);
        assert_eq!(bridge.state.incarnation(), 0, "it took no incarnation");
        let (rows, _) = node.last(&face, "", 8).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].2, live.as_bytes(), "it wrote no node row");

        let gone = "v=1 t=2 state=gone inc=4 fabric=disconnected";
        node.publish(77, 2, &face, gone.as_bytes()).unwrap();
        assert!(bridge.attach_broker());
        let (rows, _) = node.last(&face, "", 8).unwrap();
        let row = String::from_utf8_lossy(&rows[0].2).into_owned();
        assert!(
            row.contains("state=live inc=5") && row.contains("observer=1"),
            "{row}"
        );
    }

    #[test]
    fn a_node_row_is_carried_elsewhere_only_by_a_live_holder() {
        let running = std::os::unix::process::parent_id();
        let (dead, me) = (u32::MAX, std::process::id());
        for (state, holder, ours, carried) in [
            ("live", format!("host=here pid={running}"), 0, true),
            ("live", format!("host=here pid={running}"), 3, false),
            ("gone", format!("host=here pid={running}"), 0, false),
            ("live", format!("host=here pid={dead}"), 0, false),
            ("live", format!("host=here pid={me}"), 0, false),
            ("live", format!("host=far pid={dead}"), 0, true),
            ("live", String::new(), 0, true),
            ("live", "host=here".to_string(), 0, true),
            ("live", "host=here pid=unknown".to_string(), 0, true),
            ("live", "host=here pid=4294967296".to_string(), 0, true),
        ] {
            let row = format!("v=1 state={state} inc=3 {holder}");
            let body = Body::decode(row.as_bytes()).0;
            assert_eq!(
                carried_elsewhere(&body, 3, ours, "here").is_some(),
                carried,
                "{row} (our inc {ours})"
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn the_ghost_sweep_never_retires_a_row_superseded_past_its_walk() {
        use astream_broker::proto::{decode_request, encode_response, read_frame, write_frame};
        use astream_broker::{Request, Response};
        let (_lane, mut bridge) = FakeVerbLane::bridge(&[]);
        bridge.inc = 2;
        bridge.fault = Fault::SweepGhostsAtOnceWhileMarked;
        std::fs::write(bridge.state.root().join("sweep-ghosts-now"), b"").unwrap();
        let node = bridge.node.clone();
        let published = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let (ours, theirs) = std::os::unix::net::UnixStream::pair().unwrap();
        {
            let published = Arc::clone(&published);
            std::thread::spawn(move || {
                let mut s = theirs;
                let row = |offset: u64, inc: u64| Response::Delivery {
                    offset,
                    subject: format!("/f/f/pub/{node}/s-x/presence"),
                    body: format!("v=1 t=1 inc={inc} state=live").into_bytes(),
                };
                while let Ok(Some(frame)) = read_frame(&mut s) {
                    let (rows, next, head) = match decode_request(&frame) {
                        Some(Request::Last { .. }) => (vec![row(3, 2)], 10, 10),
                        Some(Request::Fetch {
                            from_offset: 10, ..
                        }) => (vec![row(11, 3)], 12, 12),
                        _ => {
                            published.store(true, std::sync::atomic::Ordering::SeqCst);
                            return;
                        }
                    };
                    for r in rows.iter().chain([&Response::Mark {
                        next,
                        head,
                        resume: String::new(),
                        durable: None,
                        oldest: None,
                    }]) {
                        if write_frame(&mut s, &encode_response(r)).is_err() {
                            return;
                        }
                    }
                }
            });
        }
        bridge.conn = Some(Conn::from_stream(Box::new(ours)).into());
        bridge.retire_ghost_presence();
        assert!(bridge.conn.is_some(), "the read was answered");
        assert!(bridge.ghosts.is_empty(), "{:?}", bridge.ghosts);
        assert!(
            !published.load(std::sync::atomic::Ordering::SeqCst),
            "the sweep wrote over a newer incarnation's row"
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_refused_exited_row_is_owed_to_the_sweep_until_it_lands() {
        let (lane, mut bridge) = FakeVerbLane::bridge(&[]);
        let (socket, _broker, _server) = serve_log(&lane, "exits-owed");
        bridge.cfg.broker = socket;
        assert!(bridge.attach_broker());
        assert!(!bridge.sweep_owed(), "nothing owed after the attach");
        let (fleet, node) = (bridge.cfg.fleet.clone(), bridge.node.clone());
        let face = |sid: &str| subject::session_face(&fleet, &node, sid, "presence");
        let (mut writer, _) = bridge.connect().unwrap();
        let mark = bridge.mark.clone().expect("the state dir's mark");
        let ours = |row: &str| format!("{row} {}={mark}", crate::state::PRESENCE_MARK);
        let live = ours(&format!("v=1 t=1 inc={} state=live", bridge.inc));
        writer
            .publish(91, 1, &face("s-gone"), live.as_bytes())
            .unwrap();
        writer
            .publish(91, 2, &face("s-done"), live.as_bytes())
            .unwrap();
        let done = ours(&format!("v=1 t=2 inc={} state=exited", bridge.inc));
        writer
            .publish(91, 3, &face("s-done"), done.as_bytes())
            .unwrap();
        for sid in ["s-gone", "s-done"] {
            bridge.conclude(
                Then::Presence {
                    sid: sid.to_string(),
                    mark: None,
                    exited: true,
                },
                Err(io::Error::other("batch rolled back")),
            );
        }
        assert_eq!(
            bridge.exits_owed,
            ["s-done", "s-gone"].map(String::from).into_iter().collect(),
            "each refused `exited` row is owed"
        );
        assert!(bridge.sweep_owed(), "the sweep's arm has work");
        bridge.retire_ghost_presence();
        let newest = |bridge: &Bridge, sid: &str| {
            let (mut reader, _) = bridge.connect().unwrap();
            let (rows, _) = reader.last(&face(sid), "", 8).unwrap();
            String::from_utf8_lossy(&rows.last().expect("a row").2).into_owned()
        };
        assert!(
            newest(&bridge, "s-gone").contains("state=exited"),
            "the owed row was written"
        );
        assert_eq!(
            newest(&bridge, "s-done"),
            done,
            "nothing written over a row that already says exited"
        );
        assert!(bridge.exits_owed.is_empty(), "{:?}", bridge.exits_owed);
        assert!(bridge.ghosts.is_empty(), "an owed exit is no ghost");
        assert!(
            !node_evs(&bridge)
                .iter()
                .any(|ev| ev.contains("presence-retired")),
            "a departure's own row is no retirement"
        );
    }

    #[cfg(unix)]
    #[test]
    fn failed_live_admission_keeps_new_group_mail_behind_old_refill() {
        const SID: &str = "s-live-refill";
        let (lane, mut bridge) = FakeVerbLane::bridge(&[(1, SID)]);
        let socket = lane.dir.path().join("broker.sock");
        let broker = crate::test_broker(lane.dir.path().join("broker.log")).unwrap();
        let _server = broker.serve(&socket).unwrap();
        bridge.cfg.broker = socket.to_string_lossy().into_owned();

        let (mut writer, _) = bridge.connect().unwrap();
        let subject = format!("/f/f/in/{}/{SID}/h-writer/note", bridge.node);
        let (old, _) = writer
            .publish(82, 1, &subject, b"v=1 t=1 text=old")
            .unwrap();
        let group = subject::inbox_group("f", &bridge.node);
        writer.commit(&group, old).unwrap();
        let model = aterm_models_session::fabric_refill_retry_model();
        let mut state = model.init_state();
        let (first, first_closer) = bridge.connect().unwrap();
        bridge.conn = Some(first.into());
        assert!(model.fire("Attach", &mut state));
        let (consumer, closer) = bridge.connect().unwrap();
        closer
            .set_read_timeout(Some(Duration::from_secs(60)))
            .unwrap();
        let mut sub = consumer
            .subscribe_group(&group, &subject::inbox_filter("f", &bridge.node))
            .unwrap();
        assert!(model.fire("Subscribe", &mut state));

        assert!(bridge.refresh_sessions().is_ok());
        assert!(bridge.per_sid.pending_admit.contains(SID));
        assert!(model.fire("Discover", &mut state));
        first_closer.close();
        bridge.admit_fresh_sessions();
        assert!(model.fire("FetchFails", &mut state));
        assert!(bridge.per_sid.pending_admit.contains(SID));
        assert!(bridge.conn.is_none());

        let (new, _) = writer
            .publish(82, 2, &subject, b"v=1 t=2 text=new")
            .unwrap();
        let live = next_delivery(&mut sub, "the live group offered new mail");
        assert_eq!(live.0, new);
        bridge.on_inbox_record(&live);
        assert!(!model.fire("DeliverNew", &mut state));
        assert_eq!(
            lane.delivery_offsets(),
            [],
            "new live mail cannot overtake a failed old-mail refill"
        );

        let stale_generation = bridge.mailbox.broker_generation(Source::Inbox);
        let _ = bridge
            .mailbox
            .push(Source::Inbox, live.clone(), stale_generation);
        bridge.mailbox.reset_broker_sources();
        assert!(
            bridge.mailbox.take(Duration::ZERO).is_none(),
            "the previous subscription's queued copy is gone"
        );
        let (second, _) = bridge.connect().unwrap();
        bridge.conn = Some(second.into());
        assert!(model.fire("Attach", &mut state));
        assert!(bridge.refill_pending_admissions(false));
        assert!(model.fire("RefillCompletes", &mut state));
        assert_eq!(lane.delivery_offsets(), [old, new]);
        assert!(model.fire("DeliverNew", &mut state));
        bridge.on_inbox_record(&live);
        assert_eq!(
            lane.delivery_offsets(),
            [old, new],
            "a broker redelivery remains idempotent"
        );
        for name in ["RefillDebtSurvivesFailure", "OldBeforeLive", "OldBeforeNew"] {
            assert!(model.check_invariant(name, &state));
        }
    }

    struct FakePushLane {
        ours: std::os::unix::net::UnixStream,
        barriers: usize,
    }

    impl FakePushLane {
        fn attach(bridge: &Bridge) -> Self {
            use std::io::{BufRead, BufReader, Write};
            let (mut ours, theirs) = std::os::unix::net::UnixStream::pair().expect("a push lane");
            spawn_event_reader(
                Ctl::from_stream(theirs).expect("the reader's half"),
                bridge.mailbox.clone(),
            );
            let mut asked = String::new();
            BufReader::new(ours.try_clone().expect("a read half"))
                .read_line(&mut asked)
                .expect("the reader subscribes");
            assert_eq!(asked, "subscribe @* events,sessions\n");
            ours.write_all(b"OK subscribe 0\n").expect("the handshake");
            FakePushLane { ours, barriers: 0 }
        }

        fn write(&mut self, lines: &[&str]) {
            use std::io::Write;
            for line in lines {
                self.ours
                    .write_all(format!("{line}\n").as_bytes())
                    .expect("the push lane");
            }
        }

        fn park_through(&mut self, bridge: &mut Bridge, lines: &[&str]) {
            self.barriers += 1;
            let sid = format!("s-park-barrier-{}", self.barriers);
            let barrier = format!("sub {} {sid}", 1_000_000 + self.barriers);
            let mut all = lines.to_vec();
            all.push(&barrier);
            self.write(&all);
            let deadline = Instant::now() + Duration::from_secs(60);
            while !bridge.per_sid.watched.contains_key(&sid) {
                assert!(
                    !bridge.park_detached(Duration::from_millis(20)),
                    "aterm is still there"
                );
                assert!(
                    Instant::now() < deadline,
                    "the parked loop never took {barrier:?}"
                );
            }
            bridge.per_sid.watched.remove(&sid);
        }

        fn deliver(&mut self, bridge: &mut Bridge, lines: &[&str]) {
            use std::io::Write;
            self.barriers += 1;
            let barrier = format!("EVENT * test-barrier {}", self.barriers);
            for line in lines.iter().copied().chain([barrier.as_str()]) {
                self.ours
                    .write_all(format!("{line}\n").as_bytes())
                    .expect("the push lane");
            }
            let deadline = Instant::now() + Duration::from_secs(60);
            loop {
                match bridge.mailbox.take(Duration::from_millis(50)) {
                    Some(Item::Event(line)) if line == barrier => return,
                    Some(Item::Event(line)) => bridge.on_event(&line),
                    Some(Item::Closed(source)) => panic!("{source:?} closed mid-test"),
                    Some(_) => {}
                    None => assert!(
                        Instant::now() < deadline,
                        "the reader never delivered {barrier:?}"
                    ),
                }
            }
        }
    }

    fn holds(bridge: &Bridge, sid: &str, topic: &str) -> bool {
        bridge
            .per_sid
            .topics
            .get(sid)
            .is_some_and(|t| t.contains_key(topic))
    }

    #[test]
    fn a_kept_sid_listed_after_the_attach_resumes_from_its_cursor_on_disk() {
        const S: &str = "s-s";
        const T: &str = "s-t";
        const U: &str = "s-u";
        const V: &str = "s-v";
        let (lane, mut bridge) = FakeVerbLane::bridge(&[(2, S), (3, T), (4, U), (5, V)]);
        let kept = BTreeMap::from([("builds".to_string(), TopicCursor { next: 91234 })]);
        for sid in [S, U, V] {
            bridge
                .state
                .set_topics(sid, &kept)
                .expect("a previous bridge's cursors");
        }
        assert!(
            bridge.per_sid.topics.is_empty(),
            "PRECONDITION: no attach loaded any cursor"
        );
        lane.opt_in(S, &["topic builds since=@5"]);
        lane.opt_in(T, &["topic builds since=@5"]);
        lane.refuse_reads(U, 1);
        lane.opt_in(U, &["topic builds since=@5", "topic other since=@7"]);
        let mut push = FakePushLane::attach(&bridge);
        push.deliver(&mut bridge, &["EVENT * session-created s-s watch=deferred"]);
        assert_eq!(lane.reads_of(S), 1, "REACH: the announcement read S");
        assert_eq!(
            (
                bridge.per_sid.topics[S]["builds"],
                bridge.state.topics(S)["builds"]
            ),
            (TopicCursor { next: 91234 }, TopicCursor { next: 91234 }),
            "the kept cursor resumes, in memory and on disk"
        );
        assert_eq!(
            bridge.per_sid.topics[T]["builds"],
            TopicCursor { next: 5 },
            "nothing on disk: the row's since= resolves"
        );
        assert!(
            !bridge.per_sid.topics.contains_key(V) && bridge.state.topics(V).is_empty(),
            "an endpoint that lists nothing for V removes V's cursors"
        );

        push.deliver(&mut bridge, &["EVENT 4 topic add other since=@7"]);
        assert_eq!(
            bridge.state.topics(U),
            BTreeMap::from([
                ("builds".to_string(), TopicCursor { next: 91234 }),
                ("other".to_string(), TopicCursor { next: 7 }),
            ]),
            "the pushed add keeps U's other cursor"
        );
        bridge.roster_backstop();
        assert_eq!(
            bridge.per_sid.topics[U]["builds"],
            TopicCursor { next: 91234 },
            "and U's later read resumes it"
        );
    }

    #[test]
    fn inbox_seen_persists_the_endpoints_addressed_floor() {
        const T: &str = "s-t";
        let (_lane, mut bridge) = FakeVerbLane::bridge(&[(1, T)]);
        bridge.locals.insert(1, T.to_string());
        let mut push = FakePushLane::attach(&bridge);
        let mut seen = |line: &str| {
            push.deliver(&mut bridge, &[line]);
            bridge.state.seen_off(T)
        };
        assert_eq!(
            seen("EVENT 1 inbox-seen 1 off=1000 floor=-"),
            None,
            "no addressed row yet: nothing to persist"
        );
        assert_eq!(
            seen("EVENT 1 inbox-seen 2 off=1100 floor=900"),
            Some(899),
            "the unhandled task at 900 is refilled, whatever the acked row's offset"
        );
        assert_eq!(
            seen("EVENT 1 inbox-seen 5 off=1200 floor=2001"),
            Some(2000),
            "the handled task at 2000 is behind the floor"
        );
        assert_eq!(
            seen("EVENT 1 inbox-seen 6 off=3000 floor=0"),
            Some(2000),
            "a zero floor persists nothing"
        );
        assert_eq!(
            seen("EVENT 1 inbox-seen 6 off=3000 floor=junk"),
            Some(2000),
            "a floor that is no number persists nothing — never the row's offset"
        );
        assert_eq!(
            seen("EVENT 1 inbox-seen 7 off=1500"),
            Some(2000),
            "an older endpoint's `off=` is read, and the watermark never goes back"
        );
        assert_eq!(seen("EVENT 1 inbox-seen 8 off=2500"), Some(2500));
    }

    #[test]
    fn an_observer_reads_each_session_once_and_does_not_poll() {
        const S: &str = "s-s";
        let (lane, mut bridge) = FakeVerbLane::bridge(&[(2, S)]);
        bridge.attachment = Attachment::Observer;
        bridge.roster_backstop();
        assert_eq!(lane.reads_of(S), 1, "REACH: the first round read S");
        bridge.roster_backstop();
        bridge.roster_backstop();
        assert_eq!(lane.reads_of(S), 1, "an observer polled a session");
    }

    const ANNOUNCEMENT_LAWS: [&str; 3] = [
        "NotAnnouncedBeforeItsWatchOrExit",
        "StoppedOnlyWhileWatched",
        "NoTopicAddLostSilently",
    ];

    struct BridgeWorld {
        lane: FakeVerbLane,
        push: FakePushLane,
        bridge: Bridge,
        model: aterm_spec::derive::Model,
        state: aterm_spec::interp::State,
    }

    impl BridgeWorld {
        const LOCAL: u64 = 2;
        const S: &'static str = "s-s";
        const ACK: &'static str = "sub 2 s-s";
        const PUSH: &'static str = "EVENT 2 topic add probe since=@5";

        fn new() -> Self {
            let (lane, bridge) = FakeVerbLane::bridge(&[]);
            let push = FakePushLane::attach(&bridge);
            let model = aterm_models_session::subscribe_announcement_order_model();
            let state = model.init_state();
            let w = BridgeWorld {
                lane,
                push,
                bridge,
                model,
                state,
            };
            assert_eq!(w.project(), w.state, "the world starts at Init");
            w
        }

        fn project(&self) -> aterm_spec::interp::State {
            let b = &self.bridge;
            let acked = b.per_sid.watched.get(Self::S) == Some(&Self::LOCAL);
            let listed = b.locals.get(&Self::LOCAL).is_some_and(|sid| sid == Self::S);
            let mut s = self.state.clone();
            s.insert("listed", i64::from(listed));
            s.insert("acked", i64::from(acked));
            s.insert("known", i64::from(holds(b, Self::S, "probe")));
            s.insert(
                "stopped",
                i64::from(acked && b.per_sid.topics_sampled.contains(Self::S)),
            );
            s.insert("asking", 0);
            s.insert("seen", 0);
            s
        }

        fn step(
            &self,
            prev: &aterm_spec::interp::State,
            next: &aterm_spec::interp::State,
            action: &str,
        ) {
            let (ok, why) = aterm_spec::verify::validate_transition_tiered(
                &self.model,
                &[],
                prev,
                next,
                Some(action),
                &format!("bridge {action}"),
            );
            assert!(
                ok,
                "the real {action} is not the model's: {prev:?} -> {next:?}\n{why}"
            );
            for law in ANNOUNCEMENT_LAWS {
                assert!(
                    self.model.check_invariant(law, next),
                    "{law} fails at {next:?}"
                );
            }
        }

        fn endpoint(&mut self, action: &str) {
            assert!(
                self.model.fire(action, &mut self.state),
                "{action}: {:?}",
                self.state
            );
            for law in ANNOUNCEMENT_LAWS {
                assert!(self.model.check_invariant(law, &self.state), "{law}");
            }
            let mut rows = self.lane.sessions.lock().unwrap();
            rows.retain(|(local, _)| *local != Self::LOCAL);
            if self.state["live"] == 1 {
                rows.push((Self::LOCAL, Self::S.to_string()));
            }
            drop(rows);
            if action == "Add" {
                self.lane.opt_in(Self::S, &["topic probe since=@5"]);
            }
        }

        fn reading(&mut self, call: impl FnOnce(&mut Self)) -> Option<bool> {
            let before = self.lane.reads_of(Self::S);
            let refusing = self
                .lane
                .reads_refused
                .lock()
                .unwrap()
                .get(Self::S)
                .is_some_and(|left| *left > 0);
            call(self);
            match self.lane.reads_of(Self::S) - before {
                0 => None,
                1 => Some(!refusing),
                n => panic!("one call read S {n} times"),
            }
        }

        fn read_tail(
            &mut self,
            from: aterm_spec::interp::State,
            read: Option<bool>,
            after: aterm_spec::interp::State,
        ) {
            assert_eq!(
                self.model.action_enabled("Ask", &from),
                read.is_some(),
                "the bridge reads S exactly when the rule says it still reads it: {from:?}"
            );
            match read {
                None => assert_eq!(from, after, "no read, and nothing else moved"),
                Some(answered) => {
                    let mut asking = from.clone();
                    asking.insert("asking", 1);
                    asking.insert("seen", from["added"]);
                    self.step(&from, &asking, "Ask");
                    self.step(&asking, &after, if answered { "Answer" } else { "Fail" });
                }
            }
            self.state = after;
        }

        fn round(&mut self) {
            let prev = self.state.clone();
            let roster_read = !self
                .lane
                .roster_refused
                .load(std::sync::atomic::Ordering::SeqCst);
            let read = self.reading(|w| w.bridge.roster_backstop());
            let after = self.project();
            let mut listed = prev.clone();
            if roster_read {
                listed.insert("listed", after["listed"]);
                if after["listed"] == 0 {
                    for gone in ["acked", "stopped", "known"] {
                        listed.insert(gone, after[gone]);
                    }
                }
                self.step(&prev, &listed, "Roster");
            }
            self.read_tail(listed, read, after);
        }

        fn hear_ack(&mut self) {
            assert_eq!(self.state["ackwire"], 1, "S's ack is on the wire");
            let prev = self.state.clone();
            let read = self.reading(|w| w.push.deliver(&mut w.bridge, &[Self::ACK]));
            self.state.insert("ackwire", 0);
            let after = self.project();
            let mut heard = prev.clone();
            heard.insert("ackwire", 0);
            heard.insert("acked", after["acked"]);
            heard.insert("stopped", 0);
            self.step(&prev, &heard, "HearAck");
            self.read_tail(heard, read, after);
        }

        fn hear_push(&mut self) {
            assert_eq!(self.state["pushed"], 1, "the add's push is on the wire");
            let prev = self.state.clone();
            let read = self.reading(|w| w.push.deliver(&mut w.bridge, &[Self::PUSH]));
            assert_eq!(read, None, "a pushed change is not a read");
            self.state.insert("pushed", 0);
            let after = self.project();
            self.step(&prev, &after, "HearPush");
            self.state = after;
        }

        fn outage(&mut self) {
            let prev = self.state.clone();
            let mut lines = Vec::new();
            if prev["ackwire"] == 1 {
                lines.push(Self::ACK);
            }
            if prev["pushed"] == 1 {
                lines.push(Self::PUSH);
            }
            self.push.park_through(&mut self.bridge, &lines);
            self.state.insert("ackwire", 0);
            self.state.insert("pushed", 0);
            let read = self.reading(|w| w.bridge.resample_topics());
            let after = self.project();
            let mut down = prev.clone();
            down.insert("ackwire", 0);
            down.insert("pushed", 0);
            down.insert("acked", after["acked"]);
            down.insert("stopped", 0);
            self.step(&prev, &down, "Outage");
            self.read_tail(down, read, after);
        }
    }

    #[test]
    fn bridge_rounds_refine_the_announcement_model() {
        let mut w = BridgeWorld::new();
        for e in ["Register", "Read", "Drain", "Seed", "Write"] {
            w.endpoint(e);
        }
        assert_eq!(
            w.state["told"], 2,
            "REACH: S was announced `watch=deferred`"
        );
        w.lane.refuse_roster(true);
        w.round();
        w.lane.refuse_roster(false);
        w.round();
        w.endpoint("Add");
        w.round();
        w.round();
        assert_eq!(
            (w.state["known"], w.state["stopped"]),
            (1, 0),
            "REACH: learned by a round, and still read"
        );

        let mut w = BridgeWorld::new();
        w.endpoint("Register");
        w.round();
        for e in ["Read", "Drain", "Seed", "Write", "Add", "FreeSlot"] {
            w.endpoint(e);
        }
        for e in ["Read", "Drain", "Seed", "Write"] {
            w.endpoint(e);
        }
        assert_eq!(w.state["ackwire"], 1, "REACH: the freed slot adopted S");
        w.lane.refuse_reads(BridgeWorld::S, 1);
        w.hear_ack();
        assert_eq!(
            (w.state["acked"], w.state["known"]),
            (1, 0),
            "REACH: the ack's read was refused"
        );
        w.lane.refuse_reads(BridgeWorld::S, 1);
        w.round();
        assert_eq!(w.state["known"], 0, "REACH: the round's read was refused");
        w.round();
        w.round();
        assert_eq!((w.state["known"], w.state["stopped"]), (1, 1));

        let mut w = BridgeWorld::new();
        w.endpoint("FreeSlot");
        w.endpoint("Register");
        w.round();
        for e in ["Read", "Drain", "Seed", "Write", "Add"] {
            w.endpoint(e);
        }
        assert_eq!(
            (w.state["ackwire"], w.state["pushed"]),
            (1, 1),
            "REACH: both lines are on the wire"
        );
        w.outage();
        assert_eq!(
            (w.state["acked"], w.state["known"], w.state["stopped"]),
            (1, 1, 1)
        );
        w.round();

        let mut w = BridgeWorld::new();
        for e in ["FreeSlot", "Register", "Read", "Drain", "Seed", "Write"] {
            w.endpoint(e);
        }
        w.hear_ack();
        w.round();
        w.endpoint("Add");
        w.hear_push();
        assert_eq!((w.state["known"], w.state["stopped"]), (1, 1));
        w.endpoint("Exit");
        w.round();
        assert_eq!(
            (w.state["listed"], w.state["acked"], w.state["known"]),
            (0, 0, 0)
        );

        for refused in [false, true] {
            let mut w = BridgeWorld::new();
            w.endpoint("Register");
            if refused {
                w.endpoint("Add");
                w.lane.refuse_reads(BridgeWorld::S, 1);
            }
            let _ = w.bridge.refresh_sessions();
            let mut listed = w.state.clone();
            listed.insert("listed", 1);
            assert_eq!(w.project(), listed, "REACH: the roster lists S");
            let read = w.reading(|w| {
                let _ = w.bridge.sample_topics(BridgeWorld::S);
                w.bridge
                    .per_sid
                    .topics_sampled
                    .insert(BridgeWorld::S.to_string());
            });
            assert_eq!(read, Some(!refused), "REACH: S was read once");
            let mut asking = listed.clone();
            asking.insert("asking", 1);
            asking.insert("seen", listed["added"]);
            let mut once = asking.clone();
            once.insert("asking", 0);
            once.insert("seen", 0);
            once.insert("stopped", 1);
            let action = if refused { "Fail" } else { "Answer" };
            let admits = |overrides: &[(&'static str, i64)]| {
                aterm_spec::verify::validate_transition_tiered(
                    &w.model,
                    overrides,
                    &asking,
                    &once,
                    Some(action),
                    "bridge read-once",
                )
            };
            assert!(
                !admits(&[]).0,
                "refused={refused}: the healthy machine admitted the read-once rule"
            );
            let (old, why) = admits(&[("Buggy", 1)]);
            assert!(old, "refused={refused}: Buggy=1 must reproduce it\n{why}");
            let buggy = aterm_spec::interp::with_buggy(&w.model, 1);
            let mut end = once.clone();
            if !refused {
                assert!(buggy.fire("Add", &mut end));
            }
            assert!(
                !buggy.check_invariant("NoTopicAddLostSilently", &end),
                "refused={refused}: an add read by nobody and pushed by nobody: {end:?}"
            );
        }
    }

    #[cfg(unix)]
    fn advertising(
        sessions: &[(u64, &str)],
    ) -> (
        FakeVerbLane,
        Bridge,
        FakePushLane,
        Conn,
        astream_broker::BrokerHandle,
    ) {
        let (lane, mut bridge) = FakeVerbLane::bridge(sessions);
        let socket = lane.dir.path().join("broker.sock");
        let broker = astream_broker::Broker::open(lane.dir.path().join("broker.log")).unwrap();
        let server = broker.serve(&socket).unwrap();
        bridge.cfg.broker = socket.to_string_lossy().into_owned();
        let (conn, _) = bridge.connect().unwrap();
        bridge.conn = Some(conn.into());
        let push = FakePushLane::attach(&bridge);
        bridge.refresh_sessions().unwrap();
        bridge.admit_fresh_sessions();
        let (reader, _) = bridge.connect().unwrap();
        (lane, bridge, push, reader, server)
    }

    #[cfg(unix)]
    fn presence_states(bridge: &Bridge, reader: &mut Conn, sid: &str) -> Vec<String> {
        let face = subject::session_face("f", &bridge.node, sid, "presence");
        let (rows, _) = reader.fetch(0, &face, 256).unwrap();
        rows.iter()
            .filter(|(_, s, _)| *s == face)
            .filter_map(|(_, _, raw)| Body::decode(raw).0.unknown.get("state").cloned())
            .collect()
    }

    #[cfg(unix)]
    fn node_ev_words(bridge: &Bridge, reader: &mut Conn) -> Vec<String> {
        let face = subject::node_face("f", &bridge.node, "ev");
        let (rows, _) = reader.fetch(0, &face, 256).unwrap();
        rows.iter()
            .filter(|(_, s, _)| *s == face)
            .filter_map(|(_, _, raw)| {
                let ev = Body::decode(raw).0.unknown.get("ev")?.clone();
                Some(crate::pct::decode(&ev))
            })
            .collect()
    }

    #[cfg(unix)]
    #[test]
    fn a_quit_withdraws_every_session_once_and_a_handoff_withdraws_none() {
        const A: &str = "s-a";
        const B: &str = "s-b";
        const C: &str = "s-c";
        let (lane, mut bridge, mut push, mut reader, _server) = advertising(&[(1, A), (2, B)]);
        for sid in [A, B] {
            assert_eq!(presence_states(&bridge, &mut reader, sid), ["live"]);
        }

        push.deliver(&mut bridge, &["EVENT * bye handoff"]);
        assert!(lane
            .links
            .lock()
            .unwrap()
            .contains(&"link bye handoff".to_string()));
        for sid in [A, B] {
            assert_eq!(
                presence_states(&bridge, &mut reader, sid),
                ["live"],
                "a handoff's successor re-attaches {sid}: nothing is withdrawn"
            );
        }
        assert!(node_ev_words(&bridge, &mut reader).contains(&"bye reason=handoff".to_string()));

        push.deliver(&mut bridge, &["EVENT * bye quit"]);
        assert!(lane
            .links
            .lock()
            .unwrap()
            .contains(&"link bye quit".to_string()));
        for sid in [A, B] {
            assert_eq!(
                presence_states(&bridge, &mut reader, sid),
                ["live", "exited"]
            );
        }
        push.deliver(&mut bridge, &["EVENT 1 agent busy rev=2 gen=0.1 fp=0"]);
        bridge.roster_backstop();
        assert!(bridge.attach_broker(), "the re-attach");
        lane.sessions.lock().unwrap().push((3, C.to_string()));
        bridge.refresh_sessions().unwrap();
        bridge.admit_fresh_sessions();
        lane.sessions.lock().unwrap().retain(|(_, sid)| sid != A);
        push.deliver(&mut bridge, &[&format!("EVENT * session-exited {A}")]);
        for sid in [A, B] {
            assert_eq!(
                presence_states(&bridge, &mut reader, sid),
                ["live", "exited"],
                "{sid} was advertised again, or withdrawn twice, after the quit"
            );
        }
        assert_eq!(
            presence_states(&bridge, &mut reader, C),
            Vec::<String>::new(),
            "a session born after the quit was advertised"
        );
        let quits = |bridge: &Bridge, reader: &mut Conn| -> Vec<String> {
            node_ev_words(bridge, reader)
                .into_iter()
                .filter(|e| e.starts_with("bye reason=quit"))
                .collect()
        };
        assert_eq!(quits(&bridge, &mut reader), ["bye reason=quit withdrawn=2"]);

        bridge.on_aterm_closed();
        assert_eq!(presence_states(&bridge, &mut reader, C), ["exited"]);
        for sid in [A, B] {
            assert_eq!(
                presence_states(&bridge, &mut reader, sid),
                ["live", "exited"]
            );
        }
        assert_eq!(
            quits(&bridge, &mut reader),
            ["bye reason=quit withdrawn=2", "bye reason=quit withdrawn=3"],
            "the lanes closing withdraws the session born since, and says the new count"
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_refused_withdrawal_row_stops_only_itself() {
        const A: &str = "s-a";
        const B: &str = "s-b";
        const C: &str = "s-c";
        let secret = [7u8; 32];
        let (lane, mut bridge) = FakeVerbLane::bridge(&[(1, A), (2, B), (3, C)]);
        let socket = lane.dir.path().join("broker.sock");
        let broker = astream_broker::Broker::open_guarded(
            lane.dir.path().join("broker.log"),
            secret.to_vec(),
        )
        .unwrap();
        let _server = broker.serve(&socket).unwrap();
        bridge.cfg.broker = socket.to_string_lossy().into_owned();
        let node = bridge.node.clone();
        bridge.caps = [
            format!("rw,p={node}:/f/f/pub/{node}/node/>"),
            format!("rw,p={node}:/f/f/pub/{node}/{A}/>"),
            format!("rw,p={node}:/f/f/pub/{node}/{C}/>"),
            "ro:/f/f/pub/>".to_string(),
            format!("ro:/f/f/in/{node}/>"),
        ]
        .iter()
        .map(|grant| {
            let cap = astream_cap::mint(&secret, grant).unwrap();
            Cap {
                grant: cap.filter,
                tag: cap.tag,
            }
        })
        .collect();
        let (conn, _) = bridge.connect().unwrap();
        bridge.conn = Some(conn.into());
        let mut push = FakePushLane::attach(&bridge);
        bridge.refresh_sessions().unwrap();
        bridge.admit_fresh_sessions();
        let (mut reader, _) = bridge.connect().unwrap();
        for (sid, rows) in [(A, &["live"][..]), (B, &[]), (C, &["live"])] {
            assert_eq!(presence_states(&bridge, &mut reader, sid), rows, "{sid}");
        }

        push.deliver(&mut bridge, &["EVENT * bye quit"]);
        assert!(bridge.conn.is_some(), "a refusal keeps the link");
        for (sid, rows) in [
            (A, &["live", "exited"][..]),
            (B, &[]),
            (C, &["live", "exited"]),
        ] {
            assert_eq!(presence_states(&bridge, &mut reader, sid), rows, "{sid}");
        }
        let evs = node_ev_words(&bridge, &mut reader);
        assert_eq!(
            evs.iter()
                .filter(|e| e.starts_with("bye reason=quit"))
                .collect::<Vec<_>>(),
            ["bye reason=quit withdrawn=2"],
            "{evs:#?}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_quit_the_loop_never_took_is_withdrawn_when_the_lanes_close() {
        const A: &str = "s-a";
        let (_lane, mut bridge, mut push, mut reader, _server) = advertising(&[(1, A)]);
        bridge.on_aterm_closed();
        assert_eq!(presence_states(&bridge, &mut reader, A), ["live"]);

        push.write(&["EVENT * bye quit"]);
        drop(push);
        let deadline = Instant::now() + Duration::from_secs(60);
        loop {
            match bridge.mailbox.take(Duration::from_millis(50)) {
                Some(Item::Closed(Source::Aterm)) => break,
                Some(_) => {}
                None => assert!(Instant::now() < deadline, "the lane never closed"),
            }
        }
        assert_eq!(bridge.mailbox.bye(), Some(Bye::Quit));
        bridge.on_aterm_closed();
        assert_eq!(presence_states(&bridge, &mut reader, A), ["live", "exited"]);
    }

    #[cfg(unix)]
    #[test]
    fn only_a_withdrawn_address_is_refused_and_an_away_one_routes() {
        let (lane, mut bridge) = FakeVerbLane::bridge(&[]);
        let socket = lane.dir.path().join("broker.sock");
        let broker = astream_broker::Broker::open(lane.dir.path().join("broker.log")).unwrap();
        let _server = broker.serve(&socket).unwrap();
        bridge.cfg.broker = socket.to_string_lossy().into_owned();
        let (conn, _) = bridge.connect().unwrap();
        bridge.conn = Some(conn.into());
        let (mut peer, _) = bridge.connect().unwrap();
        let mut seq = 0;
        let mut row = |subject: &str, body: &str| {
            seq += 1;
            peer.publish(9_000, seq, subject, body.as_bytes()).unwrap();
        };
        row("/f/f/pub/n-far/node/presence", "v=1 t=1 state=away inc=3");
        row("/f/f/pub/n-far/s-away/presence", "v=1 t=1 inc=3 state=live");
        row("/f/f/pub/n-far/s-quit/presence", "v=1 t=1 inc=3 state=live");
        for sid in ["s-away", "s-quit"] {
            assert_eq!(
                bridge.resolve_to(&format!("@{sid}"), Some("s-me")).unwrap(),
                Route::To(format!("/f/f/in/n-far/{sid}/{}", bridge.node))
            );
        }
        row(
            "/f/f/pub/n-far/s-quit/presence",
            "v=1 t=2 inc=3 state=exited",
        );
        assert_eq!(
            bridge.resolve_to("@s-away", Some("s-me")).unwrap(),
            Route::To(format!("/f/f/in/n-far/s-away/{}", bridge.node)),
            "an away node's session still routes: its mail queues for the node"
        );
        assert_eq!(
            bridge.resolve_to("@s-quit", Some("s-me")).unwrap(),
            Route::Unroutable,
            "a pinned sid its node withdrew routed to a lane nobody fetches"
        );
        assert_eq!(
            bridge.resolve_to("@s-quit@n-far", Some("s-me")).unwrap(),
            Route::Unroutable,
            "an address naming the node that withdrew it routed all the same"
        );
        assert_eq!(
            bridge.resolve_to("@s-away@n-far", Some("s-me")).unwrap(),
            Route::To(format!("/f/f/in/n-far/s-away/{}", bridge.node))
        );
    }

    #[test]
    fn the_node_is_the_state_dirs_and_a_cap_for_another_node_is_refused() {
        let ring = |node: &str| -> Vec<Cap> {
            crate::enable::node_grants("f1", node)
                .into_iter()
                .map(|grant| Cap {
                    grant,
                    tag: [0; 32],
                })
                .collect()
        };
        let person: Vec<Cap> = crate::enable::person_grants("f1", "h-x")
            .into_iter()
            .map(|grant| Cap {
                grant,
                tag: [0; 32],
            })
            .collect();
        let at = |tag: &str, node: Option<&str>| {
            let dir =
                std::env::temp_dir().join(format!("atlink-node-for-{tag}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            let state = StateDir::open(&dir).expect("state dir");
            if let Some(n) = node {
                std::fs::write(dir.join("node"), format!("{n}\n")).expect("seed node");
            }
            (dir, state)
        };
        let (s, n) = ("n-00000000000000bb", "n-00000000000000aa");

        let (dir, state) = at("same", Some(s));
        assert_eq!(node_for(&state, &ring(s)).unwrap(), s);
        let mut both = ring(s);
        both.extend(person.clone());
        assert_eq!(node_for(&state, &both).unwrap(), s);
        let e = node_for(&state, &ring(n)).unwrap_err();
        assert_eq!(e.kind(), io::ErrorKind::InvalidInput);
        assert!(
            e.to_string().contains(s) && e.to_string().contains(n),
            "{e}"
        );
        assert_eq!(state.node().as_deref(), Some(s));
        let _ = std::fs::remove_dir_all(&dir);

        let (dir, state) = at("empty", None);
        let e = node_for(&state, &ring(n)).unwrap_err();
        assert!(e.to_string().contains(n), "{e}");
        assert_eq!(state.node(), None, "a refused start writes no node id");
        let minted = node_for(&state, &person).unwrap();
        assert!(crate::enable::is_node_id(&minted), "{minted}");
        assert_eq!(node_for(&state, &[]).unwrap(), minted);
        let _ = std::fs::remove_dir_all(&dir);

        let (dir, state) = at("person", Some("h-x"));
        assert!(node_for(&state, &[]).is_err());
        assert!(node_for(&state, &person).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn the_deadline_sweep_counts_a_receipt_only_from_the_node_the_ask_went_to() {
        const SID: &str = "s-asker";
        let (lane, mut bridge) = FakeVerbLane::bridge(&[]);
        let socket = lane.dir.path().join("broker.sock");
        let broker = astream_broker::Broker::open(lane.dir.path().join("broker.log")).unwrap();
        let _server = broker.serve(&socket).unwrap();
        bridge.cfg.broker = socket.to_string_lossy().into_owned();
        let (conn, _) = bridge.connect().unwrap();
        bridge.conn = Some(conn.into());
        bridge
            .per_sid
            .epochs
            .insert(SID.to_string(), "e".to_string());
        let node = bridge.node.clone();
        let (mut peer, _) = bridge.connect().unwrap();

        let mut asks = Vec::new();
        for seq in 1..=3 {
            let lane = format!("/f/f/in/n-far/s-far/{node}/task");
            let (off, _) = peer.publish(9_100, seq, &lane, b"v=1 t=1 text=q").unwrap();
            bridge.note_deadline(SID, off, 1_000, Some("n-far".to_string()));
            asks.push(off);
        }
        let mut seq = 0;
        let mut reply = |src: &str, kind: &str, re: u64| {
            let mut body = Body::new(1);
            body.re = Some(re);
            body.verdict = (kind == "ack").then(|| "handled".to_string());
            seq += 1;
            let subject = format!("/f/f/in/{node}/{SID}/{src}/{kind}");
            peer.publish(9_200, seq, &subject, &body.encode()).unwrap();
        };
        reply("n-evil", "ack", asks[0]);
        reply("n-evil", "expired", asks[1]);
        reply("n-far", "ack", asks[2]);

        bridge.expire_deadlines(true);

        let verdicts = format!("/f/f/in/{node}/{SID}/{node}/expired");
        let (rows, _) = peer.fetch(0, &verdicts, 64).unwrap();
        let expired: Vec<u64> = rows
            .iter()
            .filter(|(_, s, _)| *s == verdicts)
            .filter_map(|(_, _, raw)| Body::decode(raw).0.re)
            .collect();
        assert_eq!(
            expired,
            [asks[0], asks[1]],
            "a stray receipt settled a deadline, or the recipient's did not"
        );
        assert!(bridge.deadlines.is_empty(), "{:?}", bridge.deadlines);
    }

    #[test]
    fn a_ring_read_again_is_taken_only_when_it_is_still_this_nodes() {
        let ring = |node: &str, tag: u8, exp: Option<&str>| -> Vec<Cap> {
            crate::enable::node_grants("f1", node)
                .into_iter()
                .map(|grant| Cap {
                    grant: exp.map_or_else(
                        || grant.clone(),
                        |e| grant.replacen(':', &format!(",exp={e}:"), 1),
                    ),
                    tag: [tag; 32],
                })
                .collect()
        };
        let person: Vec<Cap> = crate::enable::person_grants("f1", "h-x")
            .into_iter()
            .map(|grant| Cap {
                grant,
                tag: [0; 32],
            })
            .collect();
        let (s, n) = ("n-00000000000000bb", "n-00000000000000aa");
        let held = ring(s, 0, Some("1"));
        assert_eq!(
            ring_refusal(&ring(s, 1, Some("99999999999999")), &held, s),
            None
        );
        assert_eq!(ring_refusal(&ring(s, 1, None), &held, s), None);
        let mut both = ring(s, 1, None);
        both.extend(person.clone());
        assert_eq!(ring_refusal(&both, &held, s), None);
        let other = ring_refusal(&ring(n, 1, None), &held, s);
        assert!(other.as_deref().is_some_and(|w| w.contains(n)), "{other:?}");
        let mut mixed = ring(s, 1, None);
        mixed.extend(ring(n, 1, None));
        assert!(ring_refusal(&mixed, &held, s).is_some());
        assert!(ring_refusal(&[], &held, s).is_some());
        assert!(ring_refusal(&person, &held, s).is_some());
        assert_eq!(ring_refusal(&[], &[], s), None);
        assert_eq!(ring_refusal(&ring(s, 1, None), &[], s), None);
    }

    mod mirror_conformance {
        include!("bridge_mirror_tests.rs");
    }

    mod reclaim {
        include!("bridge_reclaim_tests.rs");
    }
}
