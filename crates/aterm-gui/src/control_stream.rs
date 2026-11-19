// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE RECORD STREAM: `stream @<sid> v=1` on a contract 4 claim's own connection
//! (`docs/HARNESS-CONTRACT.md`, "The record stream").
//!
//! The verb opens only for the session whose live scoped contract 4 claim is bound to the
//! connection that asks ([`open`]). The lanes then move that connection whole (its tenure, so its
//! claims stand, its scope, principal, caller and unread bytes) to a stream lane, where [`serve`]
//! answers `OK stream v=1 epoch=<instance> acked=<seq> in=<seq>` and reads one JSON record per
//! line with the wire library's own codec ([`alab_wire::record`]), the one parser both sides run.
//!
//! Every harness line is acknowledged. What is not applied says why (`dropped`): a line over
//! the bound, a line this codec does not read (each is record `acked+1`, so a resend never brings
//! it back), a record over the stream's budget, one the claim's grant does not cover, one
//! applied before. A sequence gap or the claim leaving the connection closes the stream; the
//! harness reopens and resends from `acked`, which the session keeps beside the claim
//! ([`crate::session_timeline::StreamState`]).
//!
//! Applied so far: `beat`, which counts as liveness; `took` and `refused` (below); `state`,
//! which moves the session's current-state record
//! ([`crate::session_timeline::AgentPublication`]) and is written as
//! `mark agent word= rev= hseq= holder=` by the stream's one write arm
//! ([`crate::session_log::SessionLog::stream_mark`]); `claim`, which is the claim placed
//! again; and `usage`, `account`, `progress`, `goal` and `compact`, each kept in the
//! current-state record and (all but `account`, which writes no row) written under its own
//! word through the same arm ([`crate::session_timeline::SessionTimeline::
//! publish_stream_record`]). From the first record of a kind applied, that kind's built-in
//! source stands down for that tab (the screen reader for `state` and the composer, the usage
//! tap and the context-left mark for `usage`, the `auth=` and `token=` readers for `account`,
//! the goal readers for `goal`, the transcript's compact step for `compact`), until the claim
//! ends (through the re-exec grace of a closed connection, to the grace's end); every source
//! goes on feeding each kind the stream has not applied.
//!
//! The acts (HX-15j): `act`, `need`, `clear` and `land` are admitted by the scope of the
//! `meta set` each replaces (an `act` by its rule's, `acts::act_scope`; `wait` under `state`,
//! `held`, `land` under `turn-end`; `need` and `clear` under `approvals`, `questions` or
//! `walls`) and applied as those lines are, through the stream's write arm (the four stamps,
//! `hseq=`, no mark limiter): an act by its rule as `meta set harness_act` (its row, its face),
//! `wait` kept as the claim's continuation observation (`meta set harness_activity`, the record's
//! `seq` its sequence, no row), `held` as `meta set harness_held` (no row), `need` as a
//! `mark need` of its kind with the NEEDS YOU set `meta set harness_needs` tells, `clear` as the
//! `mark clear` that lifts it, and `land` as `meta set harness_land` (`held`, `open`); a `land`
//! saying `landed` is the `landed@v1` act, and finishes the task only where the repository
//! reader and `sys left` agree ([`finish_landed`]). `model` and `upgrade` are kept in the
//! current-state record and write no row. Once the stream has applied one of these kinds, the
//! same claimant's `meta set harness_*` line for it is answered `ERR usage: stream`
//! (`control_session::STREAM_WRITES_IT`), the footer's model and effort readers stand down
//! under `model`, and the engine-filled `upgrade=` under `upgrade`.
//!
//! A `usage` or `account` record is taken only for the tab's own account, the claim's `login=`
//! (the pool's assignment, once HX-29 makes one): a record that names another account, and
//! any such record under a claim that states no `login=`, is acknowledged
//! `dropped=refused:account`, is kept nowhere and stands no reader down.
//!
//! A `claim` record is a claim placement ([`claim_line`], [`ServeClaim`]): aterm checks each of
//! its fields as the claim grammar checks a word, builds the one line `meta set supervisor
//! <holder> contract=4 …` from them and from what the standing claim says for a field the record
//! leaves out, and serves that line as the same connection's own `meta set` is served, so the
//! proof, the kept child, the placement count and the change wakes are the claim line's. The
//! reply is not sent back: the record is acknowledged, `dropped=refused:<the ERR word>` where
//! the line was refused (the claim then stands as a refused `meta set` leaves it), and a
//! placement that moved the claim's grant or revision owes the stream a `policy` record
//! ([`StreamState::policy_due`]). A record that names no scope asks again for the scope the
//! claim asked for, not the grant, so a claim that was granted nothing is still placed again.
//! Claim records have a small budget of their own (`CLAIM_BURST`, one back each
//! `CLAIM_REFILL_MS`): one over it is acknowledged `dropped=rate` and proves nothing.
//!
//! ATERM'S OWN RECORDS ([`owed`]), each numbered in this epoch (`in`) and written by the same
//! codec: as the stream opens, the current `hand` and a `policy`; then a `hand` when the
//! person's gesture count, the composer or the hold moves, at most one every `HAND_EVERY_MS` of
//! the awake clock and always the latest ([`HandGate`]); a `policy` when the person's policy
//! moves or a placement moves the claim's grant or revision; and a `restart` for every
//! `restart` row written while the stream is open, the row kept for subscribers. What moves any
//! of them rings the stream's doorbell (`Watchers::ring_stream`), which the reader waits on
//! beside its socket, so an idle stream wakes for nothing else. Nothing aterm sent is sent
//! again, and what was owed when a stream closed is dropped (D8). The harness answers each
//! with `took` or `refused`; both are counted, and a refusal is logged by the stream's write
//! arm as `mark - refused in=<n> text=<why>`.
//!
//! A record is data: nothing in one is typed, run or opened. This module is Unix only, as the
//! claim's proof is; the state it keeps names no wire type.

use std::io::{Read, Write};
use std::os::fd::{AsFd as _, BorrowedFd};
use std::sync::Arc;
use std::time::{Duration, Instant};

use alab_wire::Scope as WireScope;
use alab_wire::record::{
    Body, CompactState, Composer, Did, Dropped, HAND_EVERY_MS, ItemState, LINE_MAX, Landing,
    LineError, NeedKind, RATE_BURST, RATE_PER_S, Record, STREAM_VERSION, Side, State,
};
use aterm_agent::harness::contract::{self, UNIVERSAL};
use aterm_agent::harness::leftover::LeftRead;
use aterm_agent::harness::repo_work::RepoRead;
use aterm_slog::Bucket;
use aterm_uds::CtlStream;
use aterm_uds::ready::Doorbell;
use aterm_uds::stop::Stop;

use super::control_lanes::Outbox;
use super::{Request, Scope, Selector, ServeDisposition, Sid, Store};
use crate::harness_claim::{Proven, Scoped};
use crate::session_store::SessionHandle;
use crate::session_timeline::{
    ACT_KIND, CLEAR_KIND, LAND_KIND, NEED_KIND, SessionMeta, StreamBinding, StreamFed, StreamLimit,
    StreamNeed, StreamRecord, StreamStamp, StreamState, StreamedItem, StreamedState,
};

/// The verb's one spelling: verb first, as `subscribe @<sid> events` is, so `@<sid> stream v=1`
/// is answered this too.
const USAGE: &str = "ERR usage: stream @<sid> v=1\n";
/// No live scoped contract 4 claim is bound to the connection that asked.
const NO_CLAIM: &str =
    "ERR usage: stream opens only for a contract 4 claim placed on this connection\n";
const NOT_OWNER: &str = "ERR denied: stream is owner-only\n";
const NO_SESSION: &str = "ERR no such session\n";

/// How long the peer has to take an acknowledgement before the stream ends: a hang detector.
const ACK_WRITE: Duration = Duration::from_secs(60);
/// How much one read takes from the socket.
const CHUNK: usize = 16 * 1024;
/// How many `claim` records a stream serves in a row before it holds them to their pace.
pub(super) const CLAIM_BURST: u32 = 3;
/// How long one `claim` record's token takes to come back.
pub(super) const CLAIM_REFILL_MS: u64 = 1000;

/// A stream's two budgets. Every record spends from `records`, the wire's own rate. A `claim`
/// record that the first admits spends from `claims` too: serving one proves the claimant's
/// image in full (a signature check and a fresh hash of its build's tree), on the lane that
/// reads the stream's `beat`s, so a harness that sent one with every beat would keep its own
/// beats unread. A harness places a claim when its child or its scope changes, far inside
/// [`CLAIM_BURST`] in a row and one each [`CLAIM_REFILL_MS`].
struct Budget {
    records: Bucket,
    claims: Bucket,
}

impl Budget {
    fn new(now: u64) -> Self {
        let refill_ms = 1000 / u64::from(RATE_PER_S.max(1));
        Self {
            records: Bucket::new(RATE_BURST, refill_ms, now),
            claims: Bucket::new(CLAIM_BURST, CLAIM_REFILL_MS, now),
        }
    }
}

/// The awake clock in milliseconds, the stream's clock for its budget and its `beat`.
pub(super) fn awake_now() -> u64 {
    aterm_uds::process::awake_ms().unwrap_or(0)
}

/// The session `stream`'s `rest` names: exactly `@<sid> v=1`.
fn target(rest: &str) -> Option<Sid<'_>> {
    let mut words = rest.split_ascii_whitespace();
    let sid = Sid::parse(words.next()?.strip_prefix('@')?)?;
    let version = words.next()?.strip_prefix("v=")?;
    (version == STREAM_VERSION.to_string() && words.next().is_none()).then_some(sid)
}

/// The session `sid` names on this instance.
fn hosted(store: &Store, sid: Sid<'_>) -> Option<crate::session_store::SessionHandle> {
    store
        .read()
        .unwrap_or_else(|p| p.into_inner())
        .by_selector(&Selector::Sid(sid))
        .cloned()
}

/// DECIDE A `stream` VERB, beside the subscribe flip ([`super::stream_verb`]). A refusal is one
/// `ERR` line with the connection left in request mode (so a claim on it stands); an open is
/// [`ServeDisposition::Stream`] with nothing written yet, because the lanes may still answer
/// `ERR busy streams` before any `OK`.
pub(super) fn open(
    request: Request,
    scope: Scope,
    store: &Store,
    out: &mut Outbox,
) -> Option<ServeDisposition> {
    let refusal = refusal(&request, scope, store);
    match refusal {
        Some(refusal) => {
            out.extend_from_slice(refusal.as_bytes());
            None
        }
        None => Some(ServeDisposition::Stream { request }),
    }
}

fn refusal(request: &Request, scope: Scope, store: &Store) -> Option<&'static str> {
    if !scope.is_owner_class() {
        return Some(NOT_OWNER);
    }
    let head = request.head();
    if head.selector.is_some() {
        return Some(USAGE);
    }
    let Some(sid) = target(head.rest) else {
        return Some(USAGE);
    };
    let Some(handle) = hosted(store, sid) else {
        return Some(NO_SESSION);
    };
    let Some(conn) = super::serving_connection() else {
        return Some(NO_CLAIM);
    };
    let meta = handle.ctx.meta.lock().unwrap_or_else(|p| p.into_inner());
    meta.stream_claim(conn).is_none().then_some(NO_CLAIM)
}

/// One line off the stream.
enum Line {
    /// A whole line, without its newline.
    Whole(Vec<u8>),
    /// A line longer than [`LINE_MAX`], discarded up to its newline and never parsed.
    Over,
    /// The open claim lost this connection.
    Revoked,
    /// No whole line yet, and aterm may owe the stream a record: its doorbell rang, or the
    /// deadline of a `hand` the gate held came.
    Owed,
    /// The peer closed, or the socket failed.
    Ended,
}

/// The stream's own line reader, bounded at [`LINE_MAX`]: it holds at most one line being
/// gathered and what one read brought after it.
struct Lines {
    buffer: Vec<u8>,
    /// Looked at already and holding no newline, so the next search starts past it.
    scanned: usize,
    /// The line being gathered passed the bound: its bytes are discarded up to its newline.
    over: bool,
}

impl Lines {
    fn new(buffered: Vec<u8>) -> Self {
        Self {
            buffer: buffered,
            scanned: 0,
            over: false,
        }
    }

    /// Wait for input or the claim's stop, and, while no whole line is buffered, for `owed`
    /// (the stream's doorbell) or `until`. Check the stop even while bytes are already
    /// buffered, so incomplete or queued lines cannot postpone the end of the stream.
    fn next(
        &mut self,
        stream: &CtlStream,
        revoked: &Stop,
        bell: BorrowedFd<'_>,
        owed: BorrowedFd<'_>,
        until: Option<Instant>,
    ) -> Line {
        loop {
            if revoked.is_set() {
                return Line::Revoked;
            }
            if let Some(at) = self.buffer[self.scanned..]
                .iter()
                .position(|byte| *byte == b'\n')
            {
                let end = self.scanned + at;
                let mut line: Vec<u8> = self.buffer.drain(..=end).collect();
                line.pop();
                self.scanned = 0;
                if std::mem::take(&mut self.over) || line.len() > LINE_MAX {
                    return Line::Over;
                }
                return Line::Whole(line);
            }
            if self.buffer.len() > LINE_MAX {
                self.over = true;
                self.buffer.clear();
            }
            self.scanned = self.buffer.len();
            match aterm_uds::ready::any(&[stream.as_fd(), bell, owed], until) {
                Ok(ready) if ready.contains(&1) => return Line::Revoked,
                // Bytes are read first: a doorbell rung while records arrive is answered after
                // the record ([`serve`]), so neither waits on the other.
                Ok(ready) if ready.contains(&0) => {}
                Ok(_) => return Line::Owed,
                Err(_) => return Line::Ended,
            }
            let mut chunk = [0u8; CHUNK];
            let mut reader = stream;
            match reader.read(&mut chunk) {
                Ok(0) => return Line::Ended,
                Ok(read) => self.buffer.extend_from_slice(&chunk[..read]),
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
                Err(_) => return Line::Ended,
            }
        }
    }
}

/// What one line was, read before any lock is taken.
enum Sent {
    /// A line over the bound, discarded unparsed.
    Over,
    /// A line within the bound that the codec does not read.
    Unread(LineError),
    /// A record.
    Record(Record),
}

impl Sent {
    /// Read `line` with the wire library's codec; `None` is a line over the bound.
    fn read(line: Option<&[u8]>) -> Self {
        match line.map(Record::read) {
            None => Self::Over,
            Some(Ok(decoded)) => Self::Record(decoded.record),
            Some(Err(error)) => Self::Unread(error),
        }
    }
}

/// What one line comes to.
#[derive(Debug, PartialEq, Eq)]
enum Step {
    /// Acknowledge harness record `seq`, saying why when it was not applied.
    Ack { seq: u64, dropped: Option<Dropped> },
    /// Close the stream, and log why.
    Close(String),
}

/// A record [`judge`] applied, for [`step`] to carry to the session in the same hold.
#[derive(Debug, PartialEq, Eq)]
enum Applied {
    /// A `beat`: liveness, and nothing of the current-state record.
    Beat,
    /// A `state` record, for the session's current-state record.
    State(StateRecord),
    /// Record `seq` of one of the other kinds the current-state record keeps (`usage`,
    /// `account`, `progress`, `goal`, `compact`), under the every-platform record's own names.
    Kept { seq: u64, record: StreamRecord },
    /// A `claim` record, for [`step`] to serve as a claim line once the session's lock is
    /// dropped.
    Claim(ClaimRecord),
    /// Record `seq`, an `act`: what the harness did.
    Act { seq: u64, did: Did },
    /// Record `seq`, a `need` whose set reads as one `meta set harness_needs` tells.
    Need { seq: u64, need: StreamNeed },
    /// Record `seq`, a `clear` of the need record `need` raised.
    Clear { seq: u64, need: u64 },
    /// Record `seq`, a `land`: Land's state as aterm keeps it.
    Land { seq: u64, land: Land },
    /// A `took`: the harness took aterm's record. Counted.
    Took,
    /// A `refused`: the harness cannot act on aterm's record `of`, for `why`. Counted, and
    /// logged by the stream's write arm.
    Refused { of: u64, why: String },
}

/// A `land` record's state, as `meta set harness_land` tells it and aterm keeps it.
#[derive(Debug, PartialEq, Eq)]
enum Land {
    /// `held`: why Land still holds.
    Held(aterm_agent::harness::acts::LandHeld),
    /// `landed`: the commit on the remote main branch.
    Landed { commit: String },
    /// `open`: nothing holds it.
    Open,
}

/// WHAT A RECORD [`step`] APPLIED LEAVES TO DO once the session's locks are let go, done by
/// [`serve`] before the record's acknowledgement is written: the NEEDS YOU set's tail
/// (`control_session::needs_told`: the `owner=needs` entry aterm stands, the credential lines),
/// the land check of a `land` saying `landed` ([`Services::landed`]), and the window's wakes.
#[derive(Default)]
struct After {
    wakes: Vec<crate::Wake>,
    needs: Option<Told>,
    landed: Option<Proven>,
}

/// A NEEDS YOU set the stream's claim told: its holder, how many needs it holds (`None`: taken
/// down) and what `SessionMeta::tell_self_needs` answered.
struct Told {
    holder: String,
    count: Option<usize>,
    told: (bool, Vec<String>, Option<Option<String>>),
}

/// WHAT [`serve`] SERVES WITH: the awake clock (the budget's and the `beat`'s), the claim line
/// server a `claim` record's line goes through ([`ServeClaim`]), the window's wake sink, and the
/// land check a `land` record saying `landed` asks for ([`check_landed_live`] in the window,
/// [`finish_landed`] over a test's reads).
pub(super) struct Services<'a> {
    pub(super) now_ms: &'a dyn Fn() -> u64,
    pub(super) claims: &'a ServeClaim<'a>,
    pub(super) post: &'a dyn Fn(crate::Wake),
    pub(super) landed: &'a dyn Fn(&SessionHandle, &Proven),
}

/// The claim a record of the act kinds is applied for, as [`step`] read it under the session's
/// metadata lock.
struct Claimant<'a> {
    proven: &'a Proven,
    holder: &'a str,
    /// The tab's own session, the rows' author.
    by: aterm_slog::Actor,
    /// The program of the claim's registry row (D15).
    agent: aterm_phase::Program,
    /// The session: its id for the window's harness facts, its local id for a meta change.
    sid: &'a str,
    session: u64,
    /// The stream's epoch and the record's `seq`.
    epoch: &'a str,
    seq: u64,
}

impl Claimant<'_> {
    fn stamp(&self) -> StreamStamp {
        StreamStamp {
            epoch: self.epoch.to_string(),
            hseq: self.seq,
            holder: self.holder.to_string(),
            by: self.by,
        }
    }

    fn meta_changed(&self) -> crate::Wake {
        crate::Wake::MetaChanged {
            session: self.session,
        }
    }

    fn fact(&self, fact: crate::HarnessFact) -> crate::Wake {
        crate::Wake::HarnessFact {
            sid: self.sid.to_string(),
            fact,
        }
    }
}

/// A `claim` record [`judge`] took: what it names of the claim, each field as the record's
/// codec read it. Nothing here has been checked as a claim word yet ([`claim_line`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct ClaimRecord {
    /// The vendor process to keep (`child=`).
    pub(super) child: Option<alab_wire::ChildProcess>,
    /// The scope it claims (`scope=`).
    pub(super) scope: Option<Vec<WireScope>>,
    /// Its own version (`version=`).
    pub(super) version: Option<String>,
    /// The vendor build (`vendor=`).
    pub(super) vendor: Option<String>,
    /// The account label (`login=`).
    pub(super) login: Option<String>,
}

/// What the standing claim says, for the words a `claim` record leaves out.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Standing {
    /// The holder, `<id>@<pid>`: a record names none, so the line is always the claimant's own.
    pub(super) holder: String,
    /// The scope words the claim asked for, comma-joined in canonical order: not the grant,
    /// which may hold fewer of them or none. A record that names no scope asks these again, so
    /// a claim granted nothing is placed again and granted by the rule that stands then.
    pub(super) scope: String,
    /// The words the claim stated.
    pub(super) version: Option<String>,
    pub(super) vendor: Option<String>,
    pub(super) login: Option<String>,
    pub(super) child: Option<String>,
}

impl Standing {
    fn of(holder: &str, scoped: &Scoped) -> Self {
        let claim = &scoped.claim;
        Self {
            holder: holder.to_string(),
            scope: claim.claimed.clone(),
            version: claim.version.as_ref().map(|version| version.text.clone()),
            vendor: claim.vendor.clone(),
            login: claim.login.clone(),
            child: claim.child.map(contract::ChildWord::word),
        }
    }
}

/// How a claim line is served for the stream's session: `rest` is what follows `meta `, and the
/// reply is the control reply (`OK…` or `ERR <word>…`). Production serves it as the same
/// connection's own `meta set supervisor` is served ([`super::serve_stream_claim`]), under the
/// connection, principal and caller the stream lane entered.
pub(super) type ServeClaim<'a> = dyn Fn(&SessionHandle, &str) -> String + 'a;

/// BUILD THE ONE CLAIM LINE a `claim` record asks for, or `None` where a field is not a claim
/// word: `set supervisor <holder> contract=4 scope=<words> version=<v> [vendor=<v>]
/// [child=<pid>:<start_us>] [login=<label>]`, what follows `meta `.
///
/// Every field is checked as the claim grammar checks a word before any text is joined: the
/// scope words are the wire table's own, non-empty; `version=` is `MAJOR.MINOR.0[+meta]`;
/// `vendor=` a vendor version word; `login=` an account label; `child=` two canonical decimals
/// with a pid above 1. None of those grammars admits a space, a newline or a control character,
/// and the finished line is held to that once more, so a record's text never becomes a second
/// word or a second control line. A field the record leaves out is the standing claim's, since
/// the line replaces the claim whole: a record that names a new child asks for the scope the
/// claim asked for (so it is granted what that ask is granted now, nothing included), and one
/// that names a scope keeps the child.
pub(super) fn claim_line(standing: &Standing, record: &ClaimRecord) -> Option<String> {
    let scope = match &record.scope {
        Some(words) => words
            .iter()
            .map(|scope| scope.word())
            .collect::<Vec<_>>()
            .join(","),
        None => standing.scope.clone(),
    };
    // A scope word's shape, as the claim grammar holds it: `[a-z][a-z-]*`. An empty list
    // spells no `scope=`.
    let shaped = |word: &str| {
        let mut bytes = word.bytes();
        bytes.next().is_some_and(|b| b.is_ascii_lowercase())
            && bytes.all(|b| b.is_ascii_lowercase() || b == b'-')
    };
    if !scope.split(',').all(shaped) {
        return None;
    }
    let version = record.version.as_ref().or(standing.version.as_ref())?;
    contract::ClaimVersion::parse(version)?;
    let vendor = record.vendor.as_ref().or(standing.vendor.as_ref());
    if vendor.is_some_and(|vendor| !contract::vendor_ok(vendor)) {
        return None;
    }
    let login = record.login.as_ref().or(standing.login.as_ref());
    if login.is_some_and(|label| !aterm_agent::harness::accounts::admissible_label(label)) {
        return None;
    }
    let child = match record.child {
        // A pid of 0 or 1 has no word: no such process is a harness's child.
        Some(child) => Some(child.word()?),
        None => standing.child.clone(),
    };
    if child
        .as_deref()
        .is_some_and(|word| contract::ChildWord::parse(word).is_none())
    {
        return None;
    }
    if !crate::session_timeline::valid_owner_token(&standing.holder) {
        return None;
    }
    let mut line = format!(
        "set supervisor {} contract={UNIVERSAL} scope={scope} version={version}",
        standing.holder
    );
    for (key, value) in [
        ("vendor", vendor),
        ("child", child.as_ref()),
        ("login", login),
    ] {
        if let Some(value) = value {
            line.push_str(&format!(" {key}={value}"));
        }
    }
    let words = 6 + [vendor, child.as_ref(), login].iter().flatten().count();
    let one_line = line
        .bytes()
        .all(|byte| byte.is_ascii_graphic() || byte == b' ')
        && line.split(' ').count() == words;
    one_line.then_some(line)
}

/// The `ERR` word of a refused claim line, for `dropped=refused:<word>`: `usage`, `unproven`,
/// `busy`, `denied`. `None` for a line that was served.
fn refusal_word(reply: &str) -> Option<String> {
    if reply.starts_with("OK") {
        return None;
    }
    let word: String = reply
        .strip_prefix("ERR ")
        .unwrap_or_default()
        .chars()
        .take_while(|c| c.is_ascii_lowercase() || *c == '-')
        .collect();
    Some(if word.is_empty() {
        "error".to_string()
    } else {
        word
    })
}

/// A `state` record [`judge`] took, for [`step`] to apply to the session's current-state
/// record.
#[derive(Debug, PartialEq, Eq)]
struct StateRecord {
    seq: u64,
    state: State,
    composer: Option<Composer>,
    /// The record as the codec writes it: the line kept as text.
    line: String,
}

/// The record's state under the name the every-platform record keeps it by.
fn streamed(state: State) -> StreamedState {
    match state {
        State::Starting => StreamedState::Starting,
        State::Idle => StreamedState::Idle,
        State::Working => StreamedState::Working,
        State::Asking => StreamedState::Asking,
        State::Walled => StreamedState::Walled,
        State::Ended => StreamedState::Ended,
    }
}

/// What the record says the input box holds. A `state` with no `composer` says nothing of it,
/// and the stream is the composer's one source once it feeds the state: it reads `-`.
fn composer(composer: Option<Composer>) -> aterm_messages::presence::ComposerState {
    use aterm_messages::presence::ComposerState;
    match composer {
        Some(Composer::Empty) => ComposerState::Empty,
        Some(Composer::Draft) => ComposerState::Draft,
        None => ComposerState::Unknown,
    }
}

/// The word a `usage` or `account` record for another account than the tab's own is refused
/// with (`dropped=refused:account`).
const NOT_OWN_ACCOUNT: &str = "account";
/// What follows `refused:` in the acknowledgement of a record whose row the session's log
/// refused (a closed session log): the record is not applied and its kind's built-in source
/// goes on feeding.
const NO_ROW: &str = "log";

/// A record of a kind the current-state record keeps beside `state`, under the names the
/// every-platform record keeps it by; `None` for every other kind. `line` writes the record
/// back as the codec does, for the kinds whose line is kept (`account`, `model`, `upgrade`):
/// `None` there is a record the codec read and cannot write back.
fn kept(body: &Body, line: impl FnOnce() -> Option<String>) -> Option<StreamRecord> {
    Some(match body {
        Body::Usage {
            account,
            limits,
            context_left,
        } => StreamRecord::Usage {
            account: account.clone(),
            limits: limits
                .iter()
                .map(|limit| StreamLimit {
                    name: limit.name.clone(),
                    used: limit.used,
                    resets_ms: limit.resets_ms,
                })
                .collect(),
            context_left: *context_left,
        },
        Body::Account { account, state } => StreamRecord::Account {
            account: account.clone(),
            state: state.clone(),
            line: line()?,
        },
        Body::Progress { items } => StreamRecord::Progress {
            items: items
                .iter()
                .map(|item| {
                    let state = match item.state {
                        ItemState::Pending => StreamedItem::Pending,
                        ItemState::Active => StreamedItem::Active,
                        ItemState::Done => StreamedItem::Done,
                    };
                    (item.text.clone(), state)
                })
                .collect(),
        },
        Body::Goal { text } => StreamRecord::Goal { text: text.clone() },
        Body::Compact {
            state,
            context_left,
        } => StreamRecord::Compact {
            done: *state == CompactState::Done,
            context_left: *context_left,
        },
        Body::Model {
            model,
            effort,
            key,
            offers,
            permission,
        } => StreamRecord::Model {
            model: model.clone(),
            effort: effort.clone(),
            key: *key,
            offers: offers
                .iter()
                .map(|offer| (offer.model.clone(), offer.levels.clone()))
                .collect(),
            permission: permission.clone(),
            line: line()?,
        },
        Body::Upgrade { phase, to } => StreamRecord::Upgrade {
            phase: phase.word(),
            to: to.clone(),
            line: line()?,
        },
        _ => return None,
    })
}

/// A `need` record's need as the stream keeps it, read as `meta set harness_needs` reads a
/// set ([`super::control_session::needs_told_of`]: one to 32 needs, each a need a set may tell,
/// and an entry left once sanitized); `None` for one that line would refuse, which is
/// acknowledged `dropped=refused:usage` and applies nothing.
fn stream_need(kind: NeedKind, entry: &str, needs: &[String]) -> Option<StreamNeed> {
    super::control_session::needs_told_of(needs, entry)?;
    Some(StreamNeed {
        word: kind.word(),
        entry: entry.to_string(),
        needs: needs.to_vec(),
    })
}

/// A `land` record's state as `meta set harness_land` would tell it ([`Land`]): a hold's
/// `since_ms` as the signed ms that line reads (`None`: past it), its counts held at their
/// field's largest value, its legs by the hold legs' words (`None` for a word no leg has, as
/// that line refuses one) and the first four of its open words aterm knows. `None` is
/// acknowledged `dropped=refused:usage` and applies nothing.
fn land_of(landing: &Landing) -> Option<Land> {
    use aterm_agent::harness::acts::{HoldLeg, LandHeld, OpenWord};
    Some(match landing {
        Landing::Open => Land::Open,
        Landing::Landed { commit } => Land::Landed {
            commit: commit.clone(),
        },
        Landing::Held(hold) => {
            let short =
                |count: Option<u64>| count.map_or(0, |n| u16::try_from(n).unwrap_or(u16::MAX));
            let mut held = LandHeld {
                since_ms: Some(i64::try_from(hold.since_ms).ok()?),
                nudges: u32::try_from(hold.nudges).unwrap_or(u32::MAX),
                ..LandHeld::default()
            };
            if let Some(read) = &hold.read {
                held.read = true;
                for leg in &read.legs {
                    held.legs |= HoldLeg::of_word(leg)?.bit();
                }
                let known = read.open.iter().filter_map(|word| OpenWord::of(word));
                for (slot, word) in held.open.iter_mut().zip(known) {
                    *slot = Some(word);
                }
                held.changed = short(read.changed);
                held.untracked = short(read.untracked);
                held.stashes = short(read.stashes);
                held.unpushed = short(read.unpushed);
                held.unlanded = short(read.unlanded);
                held.left = short(read.left);
            }
            Land::Held(held)
        }
    })
}

/// The account a `usage` or `account` record is of; `None` for every other kind.
fn account_of(body: &Body) -> Option<&str> {
    match body {
        Body::Usage { account, .. } | Body::Account { account, .. } => Some(account),
        _ => None,
    }
}

/// The scope aterm's act table names for `rule`, as the wire library spells it.
fn act_scope(rule: &str) -> Option<WireScope> {
    aterm_agent::harness::acts::act_scope(rule).and_then(|scope| WireScope::from_word(scope.word()))
}

/// The claim's grant, as the wire library spells it.
fn granted(scoped: &Scoped) -> Vec<WireScope> {
    scoped
        .claim
        .scopes
        .iter()
        .filter_map(|scope| WireScope::from_word(scope.word()))
        .collect()
}

/// JUDGE ONE LINE against the stream's state, the claim's grant and the stream's budget at
/// `now` (the awake clock), and move the state. It is given the line already read, so nothing
/// is parsed under the lock its caller holds.
///
/// In order: a line over the bound is record `acked+1` and is dropped unparsed, because
/// sequence numbers have no gaps. A line the codec does not read is record `acked+1` too,
/// dropped `refused:usage`: the harness keeps every record above `acked` and sends it again, so
/// a line that took no number would come back on every reopen and nothing behind it would ever
/// arrive. A `seq` above `acked+1` is a gap
/// and closes the stream. A `beat` counts as liveness as it arrives. A `seq` at or below `acked`
/// was applied before. A record with less than one token in the budget is dropped; only a record
/// that has one spends it, so the tokens that came back while records were dropped stay. A
/// record aterm sends, or one the grant does not cover, is dropped. A `usage` or `account`
/// record of another account than `login` (the claim's `login=`, the tab's own account), and
/// any such record where the claim states none, is dropped `refused:account`. `beat` is
/// applied; so is `state`, which needs the scope `state` and is handed back for [`step`] to
/// apply; so are `usage`, `account`, `progress`, `goal`, `compact`, `model` and `upgrade`,
/// handed back the same way; `claim`, handed back for [`step`] to serve as a claim line (no
/// built-in source stands down for it, so it is not added to the applied kinds); `act`,
/// `need`, `clear` and `land`, handed back for [`step`] to apply, which adds each to the
/// applied kinds only once it is applied (a `need` or `land` that `meta set harness_needs` or
/// `harness_land` would refuse is dropped `refused:usage`); and `took` and `refused`, the
/// harness's answers to aterm's own records, each counted (a `refused` is handed back for
/// [`step`] to log), and dropped `refused:in` where it names no record aterm sent in this
/// epoch; `title`, an unknown kind and every kind this aterm does not apply yet are
/// acknowledged and counted, and are not added to the applied kinds.
fn judge(
    state: &mut StreamState,
    sent: Sent,
    granted: &[WireScope],
    login: Option<&str>,
    bucket: &mut Bucket,
    now: u64,
) -> (Step, Option<Applied>) {
    let next = state.acked.saturating_add(1);
    let record = match sent {
        Sent::Record(record) => record,
        unnumbered => {
            state.acked = next;
            state.dropped += 1;
            let why = match unnumbered {
                Sent::Over => Dropped::Bound,
                _ => Dropped::Refused("usage".to_string()),
            };
            return (
                Step::Ack {
                    seq: next,
                    dropped: Some(why),
                },
                None,
            );
        }
    };
    judge_record(state, record, granted, login, bucket, now)
}

/// [`judge`] for a line that read as a record.
fn judge_record(
    state: &mut StreamState,
    record: Record,
    granted: &[WireScope],
    login: Option<&str>,
    bucket: &mut Bucket,
    now: u64,
) -> (Step, Option<Applied>) {
    let next = state.acked.saturating_add(1);
    let seq = record.seq;
    if seq > next {
        return (
            Step::Close(format!("seq={seq} gap acked={}", state.acked)),
            None,
        );
    }
    let body = &record.body;
    if matches!(body, Body::Beat) {
        state.last_beat_ms = Some(now);
    }
    let drop = |state: &mut StreamState, why: Dropped| {
        state.dropped += 1;
        (
            Step::Ack {
                seq,
                dropped: Some(why),
            },
            None,
        )
    };
    if seq <= state.acked {
        return drop(state, Dropped::Applied);
    }
    state.acked = seq;
    if bucket.peek(now).is_err() {
        return drop(state, Dropped::Rate);
    }
    bucket.spend(now);
    if body
        .kind()
        .is_some_and(|kind| kind.sent_by() == Side::Aterm)
    {
        return drop(state, Dropped::Refused("kind".to_string()));
    }
    if !body.admitted(granted, act_scope) {
        return drop(state, Dropped::Scope);
    }
    if account_of(body).is_some_and(|account| login != Some(account)) {
        // Not the tab's own account (or the claim names none): nothing is kept, no row is
        // written and the kind is not applied, so its built-in readers go on feeding it.
        return drop(state, Dropped::Refused(NOT_OWN_ACCOUNT.to_string()));
    }
    let mut applied = None;
    match body {
        Body::Beat => {
            state.applied.insert(alab_wire::Kind::Beat.word());
            applied = Some(Applied::Beat);
        }
        Body::State {
            state: said,
            composer,
        } => match record.line() {
            Ok(line) => {
                state.applied.insert(alab_wire::Kind::State.word());
                applied = Some(Applied::State(StateRecord {
                    seq,
                    state: *said,
                    composer: *composer,
                    line,
                }));
            }
            // A record the codec read and cannot write back is kept as no line: not applied.
            Err(_) => state.unapplied += 1,
        },
        Body::Claim {
            child,
            scope,
            version,
            vendor,
            login,
        } => {
            applied = Some(Applied::Claim(ClaimRecord {
                child: *child,
                scope: scope.clone(),
                version: version.clone(),
                vendor: vendor.clone(),
                login: login.clone(),
            }));
        }
        // The act kinds enter the applied kinds only once [`step`] applied them, so a record
        // the session's log or its own reading refuses never stands a writer down.
        Body::Act { did } => {
            applied = Some(Applied::Act {
                seq,
                did: did.clone(),
            });
        }
        Body::Need { kind, entry, needs } => match stream_need(*kind, entry, needs) {
            Some(need) => applied = Some(Applied::Need { seq, need }),
            None => return drop(state, Dropped::Refused("usage".to_string())),
        },
        Body::Clear { need } => applied = Some(Applied::Clear { seq, need: *need }),
        Body::Land { state: landing } => match land_of(landing) {
            Some(land) => applied = Some(Applied::Land { seq, land }),
            None => return drop(state, Dropped::Refused("usage".to_string())),
        },
        // An answer names one of aterm's records of this epoch, `1..=in` (the codec reads no
        // `in` of 0): one past `in` answers nothing aterm sent.
        Body::Took { of } | Body::Refused { of, .. } if *of > state.sent => {
            return drop(state, Dropped::Refused("in".to_string()));
        }
        Body::Took { .. } => {
            state.took += 1;
            state.applied.insert(alab_wire::Kind::Took.word());
            applied = Some(Applied::Took);
        }
        Body::Refused { of, why } => {
            state.refused += 1;
            state.applied.insert(alab_wire::Kind::Refused.word());
            applied = Some(Applied::Refused {
                of: *of,
                why: why.clone(),
            });
        }
        Body::Title { .. } => state.titles += 1,
        Body::Unknown { .. } => state.unknown += 1,
        _ => match (body.kind(), kept(body, || record.line().ok())) {
            (Some(kind), Some(record)) => {
                state.applied.insert(kind.word());
                applied = Some(Applied::Kept { seq, record });
            }
            // A kind this aterm does not apply yet, and a record the codec read and cannot
            // write back.
            _ => state.unapplied += 1,
        },
    }
    (Step::Ack { seq, dropped: None }, applied)
}

/// Judge `sent` for the stream of `proven` on connection `conn`, under the session's metadata
/// lock: the claim is read again for every line, so a record is held to the grant that stands
/// when it arrives, and a claim that left this connection ends the stream. A `state` record
/// that was taken is applied in the same hold (meta, then the timeline), so no reader sees the
/// kind applied and the record not yet moved, and so is a `refused`'s log row. An applied
/// record of any kind opens the agent run of the claim's registry row where the tab has none
/// (D15).
///
/// A `claim` record is the exception to the one hold: its line is served with the lock dropped
/// ([`serve_claim`]), because serving a claim line takes that lock itself. One over the claim
/// budget ([`Budget`]) is acknowledged `dropped=rate` and nothing is served or proven.
///
/// An `act`, `need`, `clear` or `land` record is applied in the same hold ([`apply_act`],
/// [`apply_need`], [`apply_clear`], [`apply_land`]); what it leaves for no lock is put in
/// `after`.
fn step(
    handle: &SessionHandle,
    binding: &StreamBinding,
    sent: Sent,
    budget: &mut Budget,
    now: u64,
    claims: &ServeClaim<'_>,
    after: &mut After,
) -> Step {
    let ctx = handle.ctx.as_ref();
    let mut meta = ctx.meta.lock().unwrap_or_else(|p| p.into_inner());
    let Some((claim, scoped)) = binding.claim(&meta) else {
        return Step::Close("the claim left this connection".to_string());
    };
    let granted = granted(scoped);
    let standing = Standing::of(&claim.holder, scoped);
    let agent = scoped.claim.row.program;
    let proven = &binding.proven;
    let Some(state) = meta.stream.as_mut().filter(|state| state.proven == *proven) else {
        return Step::Close("the stream's state went to another claimant".to_string());
    };
    let (step, applied) = judge(
        state,
        sent,
        &granted,
        standing.login.as_deref(),
        &mut budget.records,
        now,
    );
    let by =
        crate::session_log::sid_actor(ctx.self_id.as_str()).unwrap_or(aterm_slog::Actor::Nobody);
    let epoch = state.epoch.clone();
    let claimant = |seq: u64| Claimant {
        proven,
        holder: &standing.holder,
        by,
        agent,
        sid: handle.sid.as_str(),
        session: handle.local_id,
        epoch: &epoch,
        seq,
    };
    match applied {
        None => step,
        Some(Applied::Act { seq, did }) => {
            let refused = apply_act(ctx, &mut meta, &claimant(seq), did, after);
            acknowledged(&mut meta, proven, step, refused)
        }
        Some(Applied::Need { seq, need }) => {
            let refused = apply_need(ctx, &mut meta, &claimant(seq), need, after);
            acknowledged(&mut meta, proven, step, refused)
        }
        Some(Applied::Clear { seq, need }) => {
            let refused = apply_clear(ctx, &mut meta, &claimant(seq), need, after);
            acknowledged(&mut meta, proven, step, refused)
        }
        Some(Applied::Land { seq, land }) => {
            let refused = apply_land(ctx, &mut meta, &claimant(seq), land, after);
            acknowledged(&mut meta, proven, step, refused)
        }
        Some(Applied::Claim(record)) => {
            if budget.claims.peek(now).is_err() {
                state.dropped += 1;
                return match step {
                    Step::Ack { seq, .. } => Step::Ack {
                        seq,
                        dropped: Some(Dropped::Rate),
                    },
                    close @ Step::Close(_) => close,
                };
            }
            budget.claims.spend(now);
            drop(meta);
            serve_claim(handle, proven, &standing, &record, claims, step)
        }
        Some(applied) => {
            let mut timeline = ctx.timeline.lock().unwrap_or_else(|p| p.into_inner());
            let kept = match applied {
                Applied::State(record) => {
                    timeline.publish_stream_state(StreamFed {
                        state: streamed(record.state),
                        composer: composer(record.composer),
                        epoch: state.epoch.clone(),
                        hseq: record.seq,
                        holder: standing.holder.clone(),
                        by,
                        agent,
                        line: record.line,
                    });
                    None
                }
                Applied::Refused { of, why } => {
                    timeline.stream_refused(of, &why, &standing.holder, by);
                    None
                }
                Applied::Kept { seq, record } => Some((seq, record)),
                // The act kinds and `claim` were served above.
                Applied::Beat
                | Applied::Took
                | Applied::Claim(_)
                | Applied::Act { .. }
                | Applied::Need { .. }
                | Applied::Clear { .. }
                | Applied::Land { .. } => None,
            };
            // D15: an applied record of any kind opens the agent run of the claim's row where
            // none is open, whether or not it moved the agent word.
            timeline.stream_applied_record(agent, by);
            // After the run is open: a `progress`, `goal` or `compact` row is written on it.
            if let Some((seq, record)) = kept {
                let stamp = StreamStamp {
                    epoch: state.epoch.clone(),
                    hseq: seq,
                    holder: standing.holder,
                    by,
                };
                let kind = record.kind();
                if !timeline.publish_stream_record(stamp, record) {
                    // The log refused the record's row: the timeline kept nothing of the kind
                    // and its built-in source feeds, so the kind is not applied and the
                    // harness is told.
                    state.applied.remove(kind);
                    state.dropped += 1;
                    return Step::Ack {
                        seq,
                        dropped: Some(Dropped::Refused(NO_ROW.to_string())),
                    };
                }
            }
            step
        }
    }
}

/// The stream's state of claimant `proven`, under the session's metadata lock.
fn own_state<'a>(meta: &'a mut SessionMeta, proven: &Proven) -> Option<&'a mut StreamState> {
    meta.stream.as_mut().filter(|state| state.proven == *proven)
}

/// Record that the stream of `proven` applied a record of `kind` (its wire word): from here the
/// same claimant's `meta set` writer of it stands down (D29).
fn mark_applied(meta: &mut SessionMeta, proven: &Proven, kind: &'static str) {
    if let Some(state) = own_state(meta, proven) {
        state.applied.insert(kind);
    }
}

/// `step`, or where the record's apply refused it (`why`, a word of `refused:`), its
/// acknowledgement saying so, counted as dropped.
fn acknowledged(
    meta: &mut SessionMeta,
    proven: &Proven,
    step: Step,
    refused: Option<&'static str>,
) -> Step {
    let (Some(why), Step::Ack { seq, .. }) = (refused, &step) else {
        return step;
    };
    let seq = *seq;
    if let Some(state) = own_state(meta, proven) {
        state.dropped += 1;
    }
    Step::Ack {
        seq,
        dropped: Some(Dropped::Refused(why.to_string())),
    }
}

/// D15 for a record that writes no row of its own: the agent run of the claim's row opens
/// where none is open.
fn opens_run(ctx: &crate::SessionCtx, who: &Claimant<'_>) {
    ctx.timeline
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .stream_applied_record(who.agent, who.by);
}

/// AN `act` RECORD, as the `meta set` line it replaces: an act by its rule as `meta set
/// harness_act <rule>` ([`super::control_session::stream_act`]: its row through the stream's
/// arm, its face); `wait` as `meta set harness_activity`, kept as the claim's continuation
/// observation with the record's `seq` as its sequence and its wait from now (no row); `held`
/// as `meta set harness_held` (no row). The refusal word where it applied nothing: `log` where
/// the session's log refused the act's row.
fn apply_act(
    ctx: &crate::SessionCtx,
    meta: &mut SessionMeta,
    who: &Claimant<'_>,
    did: Did,
    after: &mut After,
) -> Option<&'static str> {
    match did {
        Did::Rule { rule, .. } => {
            let face = {
                let mut timeline = ctx.timeline.lock().unwrap_or_else(|p| p.into_inner());
                timeline.stream_applied_record(who.agent, who.by);
                super::control_session::stream_act(
                    &mut timeline,
                    &rule,
                    who.holder,
                    who.by,
                    who.seq,
                )
            };
            let Some(face) = face else {
                return Some(NO_ROW);
            };
            after.wakes.push(who.meta_changed());
            if let Some(face) = face {
                after
                    .wakes
                    .push(who.fact(crate::HarnessFact::SelfTask(face)));
            }
        }
        Did::Wait { reason, wait_ms } => {
            let now = aterm_time::unix_millis().unwrap_or(0);
            let changed = meta.keep_continuation_activity(crate::autopilot_activity::Activity {
                sequence: who.seq,
                reason: reason.word().to_string(),
                until_ms: wait_ms.map(|ms| now.saturating_add(ms)),
                observed_ms: now,
                from_stream: true,
            });
            opens_run(ctx, who);
            if changed {
                after.wakes.push(who.meta_changed());
            }
        }
        Did::Held { held } => {
            let held = match held {
                Some(by) => match aterm_agent::harness::acts::HeldBy::of_word(by.word()) {
                    Some(arm) => Some(arm),
                    // A stop `meta set harness_held` has no arm for: applied as nothing.
                    None => return Some("usage"),
                },
                None => None,
            };
            let moved = meta.tell_self_held(who.proven, held);
            opens_run(ctx, who);
            if moved {
                after.wakes.push(who.meta_changed());
            }
        }
    }
    mark_applied(meta, who.proven, ACT_KIND);
    None
}

/// TELL THE CLAIM'S NEEDS YOU SET from the needs its stream holds open: the newest open need's
/// set, as `meta set harness_needs` tells one, or the set taken down where none stays open
/// ([`SessionMeta::tell_self_needs`]). Only where the claim takes `turn-end`, the duty that set
/// is read under ([`SessionMeta::self_needs`]); without it the need's row is the need. The
/// tail that takes no lock is left in `after`.
fn tell_needs(meta: &mut SessionMeta, who: &Claimant<'_>, after: &mut After) {
    let turn_end = Some(aterm_agent::harness::contract::Scope::TurnEnd);
    let takes = meta
        .live_claim(crate::metrics::now_us())
        .and_then(|claim| claim.act_proof_for(turn_end))
        == Some(who.proven);
    if !takes {
        return;
    }
    let newest = own_state(meta, who.proven)
        .and_then(|state| state.needs.values().next_back())
        .and_then(|need| super::control_session::needs_told_of(&need.needs, &need.entry));
    let count = newest.as_ref().map(|told| told.needs().len());
    let told = meta.tell_self_needs(who.proven, newest);
    after.needs = Some(Told {
        holder: who.holder.to_string(),
        count,
        told,
    });
}

/// A `need` RECORD: `mark - need word=<its kind> text=<its entry> hseq=` through the stream's
/// arm (the fold raises it as the harness's), kept open in the stream's state under the
/// record's `seq`, and its set told as `meta set harness_needs` tells one ([`tell_needs`]).
/// `log` where the session's log refused the row: nothing is kept.
fn apply_need(
    ctx: &crate::SessionCtx,
    meta: &mut SessionMeta,
    who: &Claimant<'_>,
    need: StreamNeed,
    after: &mut After,
) -> Option<&'static str> {
    {
        let mut timeline = ctx.timeline.lock().unwrap_or_else(|p| p.into_inner());
        timeline.stream_applied_record(who.agent, who.by);
        if !timeline.note_stream_need(false, need.word, Some(&need.entry), &who.stamp()) {
            return Some(NO_ROW);
        }
    }
    if let Some(state) = own_state(meta, who.proven) {
        state.needs.insert(who.seq, need);
        state.applied.insert(NEED_KIND);
    }
    tell_needs(meta, who, after);
    after.wakes.push(who.fact(crate::HarnessFact::Needs));
    None
}

/// A `clear` RECORD of the need record `need` raised: that need is no longer open, and where
/// no other open need of its word stands, `mark - clear word=<its word> hseq=` through the
/// stream's arm lifts the harness's need of that word (the fold lifts by word, and a `clear`
/// from the stream lifts only what the stream raised); the claim's set is told again from the
/// needs left open. A `clear` of a need this stream holds no open record of is applied and
/// moves nothing. `log` where the session's log refused the row: the need stays open.
fn apply_clear(
    ctx: &crate::SessionCtx,
    meta: &mut SessionMeta,
    who: &Claimant<'_>,
    need: u64,
    after: &mut After,
) -> Option<&'static str> {
    let raised = own_state(meta, who.proven).and_then(|state| {
        let raised = state.needs.get(&need)?.word;
        let alone = !state
            .needs
            .iter()
            .any(|(seq, other)| *seq != need && other.word == raised);
        Some((raised, alone))
    });
    {
        let mut timeline = ctx.timeline.lock().unwrap_or_else(|p| p.into_inner());
        timeline.stream_applied_record(who.agent, who.by);
        if let Some((word, true)) = raised
            && !timeline.note_stream_need(true, word, None, &who.stamp())
        {
            return Some(NO_ROW);
        }
    }
    if let Some(state) = own_state(meta, who.proven) {
        state.needs.remove(&need);
        state.applied.insert(CLEAR_KIND);
    }
    if raised.is_some() {
        tell_needs(meta, who, after);
        after.wakes.push(who.fact(crate::HarnessFact::Needs));
    }
    None
}

/// A `land` RECORD, as `meta set harness_land` (`held`) and `meta unset harness_land` (`open`)
/// tell it. One saying `landed` lifts the hold and is the `landed@v1` act, written through the
/// stream's act arm ([`super::control_session::stream_act`]), and asks for the land check that
/// alone finishes the task ([`After::landed`], [`finish_landed`]). `log` where the session's
/// log refused the act's row: nothing moved.
fn apply_land(
    ctx: &crate::SessionCtx,
    meta: &mut SessionMeta,
    who: &Claimant<'_>,
    land: Land,
    after: &mut After,
) -> Option<&'static str> {
    let held = match land {
        Land::Held(held) => Some(held),
        Land::Open => None,
        Land::Landed { commit } => {
            let face = {
                let mut timeline = ctx.timeline.lock().unwrap_or_else(|p| p.into_inner());
                timeline.stream_applied_record(who.agent, who.by);
                super::control_session::stream_act(
                    &mut timeline,
                    aterm_agent::harness::acts::RULE_LANDED,
                    who.holder,
                    who.by,
                    who.seq,
                )
            };
            let Some(face) = face else {
                return Some(NO_ROW);
            };
            let _ = meta.tell_self_land(who.proven, None);
            mark_applied(meta, who.proven, LAND_KIND);
            aterm_log::info!(
                "stream @{}: {} says {commit} landed: its task is checked",
                who.sid,
                who.holder
            );
            after.wakes.push(who.meta_changed());
            if let Some(face) = face {
                after
                    .wakes
                    .push(who.fact(crate::HarnessFact::SelfTask(face)));
            }
            after.landed = Some(who.proven.clone());
            return None;
        }
    };
    let moved = meta.tell_self_land(who.proven, held);
    opens_run(ctx, who);
    mark_applied(meta, who.proven, LAND_KIND);
    if moved {
        after.wakes.push(who.meta_changed());
    }
    None
}

/// WHAT THE LAND CHECK READS ([`finish_landed`]): the claimant's repository, read in process as
/// `harness probe repo` reads it (`repo_work`, [`live_repo`]), the processes its agent left
/// running, read as `sys left` reads them ([`super::control_end::agents_left_now`]), and the
/// window's harness posture ([`crate::session_store::SessionStore::harness_posture`]), under
/// which the session's own Autopilot word resolves to the mode it runs.
pub(super) struct LandReads<'a> {
    pub(super) repo: &'a dyn Fn(&str, &Proven) -> RepoRead,
    pub(super) left: &'a dyn Fn(&str) -> LeftRead,
    pub(super) posture: crate::autopilot::HarnessPosture,
}

/// A `land` RECORD SAYING `landed` FINISHES THE TASK only where both readers agree: the
/// claimant's repository reads clean (nothing no remote main has, or a folder that is gone;
/// a repository that was not read, no folder named included, is not clean) and `sys left` read
/// the agent's left rows and found none. Then the session's task, while it is open to a
/// landing, the claim of `proven` still stands and the session runs under Land (or under no
/// known mode, as a proven self-claim's landing is held,
/// [`crate::autopilot::AutopilotView::may_land`]: Cruise and Off end no task), is marked `done
/// why=landed left=0`; else nothing is marked and
/// the task stays open, so the harness's Land goes on. Whether it was marked.
pub(super) fn finish_landed(
    handle: &SessionHandle,
    proven: &Proven,
    reads: &LandReads<'_>,
) -> bool {
    use crate::task_record::{AttentionEdit, TaskMark, TaskState};
    let sid = handle.sid.as_str();
    let repo = (reads.repo)(sid, proven);
    let left = (reads.left)(sid);
    let none_left = matches!(&left, LeftRead::Rows(rows) if rows.is_empty());
    if !repo.is_clean() || !none_left {
        aterm_log::info!(
            "stream @{sid}: a landed record finished nothing: the repository read clean={} and \
             sys left read {left:?}",
            repo.is_clean()
        );
        return false;
    }
    let mut mark = TaskMark::new(TaskState::Done);
    mark.why = Some(crate::task_record::WHY_LANDED);
    mark.left = Some("0".to_string());
    let ctx = handle.ctx.as_ref();
    let posture = reads.posture.for_agent(crate::harness_host::admitted(ctx));
    let marked = crate::task_record::mark_session_task_with_context(
        ctx,
        handle.local_id,
        Some(&mark),
        AttentionEdit::Keep,
        ctx.fabric.fence_wait(),
        |_, meta, timeline, _| {
            let word = meta.autopilot_word();
            meta.live_scoped(crate::metrics::now_us())
                .is_some_and(|scoped| scoped.proven == *proven)
                && crate::autopilot::AutopilotView::of(posture, word).may_land(word)
                && crate::harness_task::open_to_landing(timeline.task())
        },
    );
    let finished = matches!(marked, Ok(Some(_)));
    aterm_log::info!(
        "stream @{sid}: a landed record agreed with the repository and sys left: task done={finished}"
    );
    finished
}

/// THE CLAIMANT'S REPOSITORY AS THE WINDOW'S LAND CHECK READS IT: the live subject's folder
/// ([`crate::harness_probe_verb::live_subject`]) read by `repo_work` under the person's `home`
/// and aterm's `state` root ([`crate::harness_host::session_repo_work_under`]). Where no subject
/// is known (no snapshot names the claimant, and its working directory cannot be read: the
/// process exited or is no longer the one proven) nothing was read, so it reads
/// [`RepoRead::Unread`], which never agrees, and the harness's next `landed` asks again.
pub(super) fn live_repo(
    sid: &str,
    proven: &Proven,
    home: Option<&std::path::Path>,
    state: Option<&std::path::Path>,
) -> RepoRead {
    crate::harness_probe_verb::live_subject(sid, proven).map_or(RepoRead::Unread, |subject| {
        crate::harness_host::session_repo_work_under(&subject.dir, home, state, &subject.used)
    })
}

/// [`finish_landed`] IN THE WINDOW, off the stream lane (the repository read runs `git`, the
/// left read sweeps the process table), over [`live_repo`], the host's left view and the
/// window's published harness posture ([`spawn_land_check`]).
pub(super) fn check_landed_live(store: &Store, handle: &SessionHandle, proven: &Proven) {
    let store = Arc::clone(store);
    spawn_land_check(handle, proven, move |handle, proven| {
        let (home, state) = (aterm_primer::home_dir(), aterm_types::dirs::state_dir());
        let posture = store
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .harness_posture();
        let repo =
            |sid: &str, proven: &Proven| live_repo(sid, proven, home.as_deref(), state.as_deref());
        let left = |sid: &str| super::control_end::agents_left_now(&store, sid);
        finish_landed(
            handle,
            proven,
            &LandReads {
                repo: &repo,
                left: &left,
                posture,
            },
        );
    });
}

/// The sessions whose land check runs, each with the claimant of the newest `landed` that
/// arrived while it ran (`None`: none did).
static CHECKING: std::sync::Mutex<Vec<(String, Option<Proven>)>> =
    std::sync::Mutex::new(Vec::new());

/// RUN THE LAND `check` FOR `proven` on a background thread, one per session at a time. A
/// `landed` that arrives while the session's check runs is not dropped: its claimant is kept
/// (the newest one's) and the running thread checks once more when it ends, so a `landed`
/// acknowledged while an earlier check that may have read the repository before the push was
/// still running is checked against what holds now. The `check` of a call that found one
/// running is not run; the running thread's own runs again.
fn spawn_land_check(
    handle: &SessionHandle,
    proven: &Proven,
    check: impl Fn(&SessionHandle, &Proven) + Send + 'static,
) {
    let sid = handle.sid.as_str().to_string();
    {
        let mut checking = CHECKING
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some((_, again)) = checking.iter_mut().find(|(held, _)| *held == sid) {
            *again = Some(proven.clone());
            return;
        }
        checking.push((sid.clone(), None));
    }
    let mut slot = Slot(Some(sid));
    let (handle, first) = (handle.clone(), proven.clone());
    let spawned = std::thread::Builder::new()
        .name("aterm-stream-landed".into())
        .spawn(move || {
            crate::qos::set_self(crate::qos::Role::Background);
            let mut proven = first;
            loop {
                check(&handle, &proven);
                match slot.next() {
                    Some(next) => proven = next,
                    None => return,
                }
            }
        });
    if let Err(error) = spawned {
        aterm_log::warn!("stream: the land check could not start: {error}");
    }
}

/// A session's place in [`CHECKING`], held by the thread that checks it: given back by
/// [`Slot::next`] once no ask is left, or however the thread ends (a panic, a spawn that
/// failed) where it was not.
struct Slot(Option<String>);

impl Slot {
    /// The claimant of a `landed` that arrived while this session's check ran, taken for the
    /// next run; `None` where none did, and then the place is given back under the same lock,
    /// so a `landed` after it starts a check of its own.
    fn next(&mut self) -> Option<Proven> {
        let sid = self.0.as_deref()?;
        let mut checking = CHECKING
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let at = checking.iter().position(|(held, _)| held == sid)?;
        let again = checking[at].1.take();
        if again.is_none() {
            checking.remove(at);
            self.0 = None;
        }
        again
    }
}

impl Drop for Slot {
    fn drop(&mut self) {
        if let Some(sid) = self.0.take() {
            CHECKING
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .retain(|(held, _)| *held != sid);
        }
    }
}

/// SERVE A `claim` RECORD [`judge`] took (`acked`, its acknowledgement), with no lock held: build
/// its line from the record and the `standing` claim ([`claim_line`]) and serve it through
/// `claims`, as the same connection's `meta set supervisor` is served. A record that builds no
/// line is acknowledged `dropped=refused:usage` and nothing is served; a line aterm refuses is
/// acknowledged `dropped=refused:<the ERR word>` and the claim stands as the refusal left it.
/// A line that was served moved the claim's placements; where it moved the claim's grant or
/// revision, the placement itself owed the stream a `policy` record
/// ([`crate::session_timeline::SessionMeta::set_supervisor`]).
fn serve_claim(
    handle: &SessionHandle,
    proven: &Proven,
    standing: &Standing,
    record: &ClaimRecord,
    claims: &ServeClaim<'_>,
    acked: Step,
) -> Step {
    let Step::Ack { seq, .. } = acked else {
        return acked;
    };
    let refused = match claim_line(standing, record) {
        None => Some("usage".to_string()),
        Some(line) => refusal_word(&claims(handle, &line)),
    };
    let ctx = handle.ctx.as_ref();
    let mut meta = ctx.meta.lock().unwrap_or_else(|p| p.into_inner());
    let agent = meta
        .live_scoped(crate::metrics::now_us())
        .filter(|scoped| scoped.proven == *proven)
        .map(|scoped| scoped.claim.row.program);
    if refused.is_some()
        && let Some(state) = meta.stream.as_mut().filter(|state| state.proven == *proven)
    {
        state.dropped += 1;
    }
    if refused.is_none()
        && let Some(agent) = agent
    {
        let by = crate::session_log::sid_actor(ctx.self_id.as_str())
            .unwrap_or(aterm_slog::Actor::Nobody);
        ctx.timeline
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .stream_applied_record(agent, by);
    }
    Step::Ack {
        seq,
        dropped: refused.map(Dropped::Refused),
    }
}

/// Do what a record left for no lock ([`After`]): the NEEDS YOU set's tail (a moved
/// `owner=needs` entry is a meta change), the land check, then the window's wakes.
fn finish(handle: &SessionHandle, after: After, services: &Services<'_>) {
    let After {
        mut wakes,
        needs,
        landed,
    } = after;
    if let Some(Told {
        holder,
        count,
        told,
    }) = needs
        && super::control_session::needs_told(handle.ctx.as_ref(), &holder, count, told).entry
    {
        wakes.push(crate::Wake::MetaChanged {
            session: handle.local_id,
        });
    }
    if let Some(proven) = landed {
        (services.landed)(handle, &proven);
    }
    for wake in wakes {
        (services.post)(wake);
    }
}

fn write_line(stream: &CtlStream, line: &str) -> std::io::Result<()> {
    let mut writer = stream;
    writer.write_all(line.as_bytes())?;
    writer.write_all(b"\n")
}

/// The wall clock in milliseconds, a record's `ms`.
fn wall_ms() -> u64 {
    aterm_time::since_epoch().map_or(0, |since| {
        u64::try_from(since.as_millis()).unwrap_or(u64::MAX)
    })
}

/// The acknowledgement of harness record `seq`, as the wire library writes it.
fn ack_line(seq: u64, dropped: Option<Dropped>) -> Option<String> {
    Record {
        seq,
        ms: wall_ms(),
        body: Body::Ack { dropped },
    }
    .line()
    .ok()
}

/// What a person is doing in the tab, as a `hand` record carries it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct HandNow {
    /// aterm's count of a person's gestures (`human_seq=`).
    human_seq: u64,
    /// What the input box holds.
    composer: Composer,
    /// Whether a halt stands (`hold=1`).
    hold: bool,
}

impl HandNow {
    /// The tab's hand now: the gesture count, the published composer and the hold, each read
    /// under its own lock and none under the session's metadata (a move of any of them after
    /// this read rings the stream again, so the next look reads it). The wire has only `empty`
    /// and `draft`: a placeholder is empty, and a composer aterm cannot read is a draft, so a
    /// harness that holds its typing for a draft holds it for one aterm cannot rule out
    /// (`status`'s `composer=-` holds a safe point the same way).
    fn read(ctx: &crate::SessionCtx) -> Self {
        use aterm_messages::presence::ComposerState;
        let composer = ctx
            .timeline
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .agent()
            .composer;
        Self {
            human_seq: ctx.human_input.seq(),
            composer: match composer {
                ComposerState::Empty | ComposerState::Placeholder => Composer::Empty,
                ComposerState::Draft | ComposerState::Unknown => Composer::Draft,
            },
            hold: ctx.fabric.hold().is_some(),
        }
    }

    fn body(self) -> Body {
        Body::Hand {
            human_seq: self.human_seq,
            composer: self.composer,
            hold: self.hold,
        }
    }
}

/// THE GATE ON `hand`, one stream's: one every [`HAND_EVERY_MS`] of the awake clock at most,
/// each the latest. The first is sent as the stream opens; a change inside the gap is held
/// until it ends and then sent as it stands then, and a hand back where it was sends nothing.
#[derive(Debug, Default)]
struct HandGate {
    /// The hand last sent, and when.
    sent: Option<(HandNow, u64)>,
    /// When a held change may go: the end of the gap.
    held_until: Option<u64>,
}

impl HandGate {
    /// Whether `now`, the hand at `at`, is to be sent. A change the gap holds is noted in
    /// [`Self::held_until`].
    fn admits(&mut self, now: HandNow, at: u64) -> bool {
        self.held_until = None;
        match self.sent {
            Some((last, _)) if last == now => false,
            Some((_, sent_at)) if at < sent_at.saturating_add(HAND_EVERY_MS) => {
                self.held_until = Some(sent_at.saturating_add(HAND_EVERY_MS));
                false
            }
            _ => true,
        }
    }

    /// The wait's end for a held change, on the monotonic clock: `None` with none held.
    fn deadline(&self, now: u64) -> Option<Instant> {
        self.held_until
            .map(|until| Instant::now() + Duration::from_millis(until.saturating_sub(now)))
    }
}

/// WHAT ATERM OWES ITS HARNESS ON THIS STREAM NOW, as lines the wire library wrote, taken under
/// the session's metadata lock (the hand is read just before it): the `hand` where the gate admits it ([`HandGate`]), a `policy`
/// where one is owed, and a `restart` for each `restart` row written since the last look, in
/// that order; as the stream opens, the current `hand` and a `policy` always (D8). Each takes
/// the next number of this epoch (`in`), never the number of a record that could not be
/// written, and is taken off what is owed here: aterm never sends its own record again. A
/// stream whose claim left this connection, or whose state is another claimant's or another
/// connection's, is closed (`Err`, why).
fn owed(
    handle: &SessionHandle,
    binding: &StreamBinding,
    hand: &mut HandGate,
    opening: bool,
    now: u64,
) -> Result<Vec<String>, String> {
    let ctx = handle.ctx.as_ref();
    let now_hand = HandNow::read(ctx);
    let mut meta = ctx.meta.lock().unwrap_or_else(|p| p.into_inner());
    if binding.claim(&meta).is_none() {
        return Err("the claim left this connection".to_string());
    }
    let Some(state) = meta
        .stream
        .as_mut()
        .filter(|state| state.proven == binding.proven && state.open == Some(binding.conn))
    else {
        return Err("the stream's state went to another claimant".to_string());
    };
    let policy = std::mem::take(&mut state.policy_due) || opening;
    let restarts = std::mem::take(&mut state.restarts);
    let mut bodies = Vec::with_capacity(2 + restarts.len());
    let send_hand = hand.admits(now_hand, now);
    if send_hand {
        bodies.push(now_hand.body());
    }
    if policy {
        bodies.push(Body::Policy);
    }
    for payload in restarts {
        // The row's own text, read by the wire library's grammar: aterm wrote both.
        match alab_wire::Restart::parse(payload.split_ascii_whitespace()) {
            Some(restart) => bodies.push(Body::Restart {
                cause: restart.cause,
                account: restart.account,
                model: restart.model,
            }),
            None => aterm_log::info!(
                "stream @{}: the restart row `{payload}` reads as no restart record; not sent",
                handle.sid.as_str()
            ),
        }
    }
    let ms = wall_ms();
    let mut lines = Vec::with_capacity(bodies.len());
    for body in bodies {
        let is_hand = matches!(body, Body::Hand { .. });
        let seq = state.sent.saturating_add(1);
        match (Record { seq, ms, body }).line() {
            Ok(line) => {
                state.sent = seq;
                lines.push(line);
                if is_hand {
                    hand.sent = Some((now_hand, now));
                }
            }
            Err(error) => aterm_log::info!(
                "stream @{}: a record of aterm's could not be written ({error})",
                handle.sid.as_str()
            ),
        }
    }
    Ok(lines)
}

/// [`owed`], written to the stream after every lock is dropped: `Err` (why) closes it, a
/// record of aterm's the peer did not take included. Its number stays spent, so the stream's
/// numbers may skip it; the harness reads such a gap as allowed.
fn send_owed(
    stream: &CtlStream,
    handle: &SessionHandle,
    binding: &StreamBinding,
    hand: &mut HandGate,
    opening: bool,
    now: u64,
) -> Result<(), String> {
    for line in owed(handle, binding, hand, opening, now)? {
        if write_line(stream, &line).is_err() {
            return Err("a record of aterm's was not taken".to_string());
        }
    }
    Ok(())
}

/// The stream's end, whichever way [`serve`] leaves: the stream of `binding` is no longer open,
/// and what it was owed is dropped ([`crate::session_timeline::SessionMeta::close_stream`]).
struct Closing<'a> {
    handle: &'a SessionHandle,
    binding: Arc<StreamBinding>,
}

impl Drop for Closing<'_> {
    fn drop(&mut self) {
        self.handle
            .ctx
            .meta
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .close_stream(&self.binding.proven, self.binding.conn);
    }
}

/// SERVE ONE RECORD STREAM for its whole life, on the stream lane that entered the connection
/// (its serial, descriptor, direct peer, principal and caller). `buffered` is what was read
/// past the verb line; `services` holds the awake clock (which the budget, `beat` and the
/// `hand` gate read), the server of a `claim` record's line ([`ServeClaim`]), the window's
/// wake sink and the land check ([`Services`]). What a record left to do with no lock held
/// ([`After`]) is done before its acknowledgement is written. Returning ends the connection,
/// and the stream is no longer open ([`Closing`]).
///
/// The claim was read on a request lane ([`open`]) and is read again here. Where it left this
/// connection in between (released, placed from another connection, lapsed), the connection
/// ends with no line, as an open stream ends when its claim leaves: it is on a stream lane and
/// serves no request any more, so an `ERR` line here would read as a refusal that kept it.
pub(super) fn serve(
    request: &Request,
    store: &Store,
    stream: &CtlStream,
    buffered: Vec<u8>,
    services: &Services<'_>,
) {
    let now_ms = services.now_ms;
    let Some(conn) = super::serving_connection() else {
        return;
    };
    let head = request.head();
    let Some(handle) = target(head.rest).and_then(|sid| hosted(store, sid)) else {
        aterm_log::info!(
            "stream {} closed before it opened: no such session",
            head.rest
        );
        return;
    };
    let sid = handle.sid.as_str();
    let ctx = handle.ctx.as_ref();
    let epoch = crate::control_auth::process_instance_id();
    // Replace the request lane's read timeout with the descriptor wait. Make the stop pipe
    // before taking either session lock.
    let revoked = Stop::new();
    // macOS may refuse a socket option after a peer has closed with bytes still to read.
    let _ = stream.set_read_timeout(None);
    if revoked.bell().is_err() || stream.set_write_timeout(Some(ACK_WRITE)).is_err() {
        return;
    }
    // What rings when aterm may owe the stream a record (`Watchers::ring_stream`).
    let Ok(doorbell) = Doorbell::new().map(Arc::new) else {
        return;
    };
    let opened = {
        let mut meta = ctx.meta.lock().unwrap_or_else(|p| p.into_inner());
        let proven = meta
            .stream_claim(conn)
            .map(|(_, scoped)| scoped.proven.clone());
        proven.map(|proven| {
            let state = meta.open_stream(&proven, epoch, conn);
            ctx.watchers.watch_stream(&doorbell);
            let binding = Arc::new(StreamBinding {
                conn,
                proven,
                revoked,
            });
            ctx.timeline
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .watch_stream_binding(&binding);
            (binding, state.acked, state.sent)
        })
    };
    let Some((binding, acked, sent)) = opened else {
        aterm_log::info!("stream @{sid} closed before it opened: the claim left this connection");
        return;
    };
    let _closing = Closing {
        handle: &handle,
        binding: Arc::clone(&binding),
    };
    let Ok(bell) = binding.revoked.bell() else {
        return;
    };
    let ok = format!("OK stream v={STREAM_VERSION} epoch={epoch} acked={acked} in={sent}");
    if binding.revoked.is_set() || write_line(stream, &ok).is_err() {
        return;
    }
    let mut hand = HandGate::default();
    if let Err(why) = send_owed(stream, &handle, &binding, &mut hand, true, now_ms()) {
        aterm_log::info!("stream @{sid} closed: {why}");
        return;
    }
    let mut budget = Budget::new(now_ms());
    let mut lines = Lines::new(buffered);
    // The first line of this connection the codec did not read is logged; the rest are counted.
    let mut unread_logged = false;
    let why = loop {
        let until = hand.deadline(now_ms());
        let line = match lines.next(stream, &binding.revoked, bell, doorbell.fd(), until) {
            Line::Whole(line) => Some(line),
            Line::Over => None,
            Line::Owed => {
                doorbell.clear();
                match send_owed(stream, &handle, &binding, &mut hand, false, now_ms()) {
                    Ok(()) => continue,
                    Err(why) => break why,
                }
            }
            Line::Revoked => break "the claim left this connection".to_string(),
            Line::Ended => break "the peer closed".to_string(),
        };
        // Read outside the session's lock: a line is up to 256 KiB of JSON.
        let sent = Sent::read(line.as_deref());
        if let Sent::Unread(error) = &sent
            && !std::mem::replace(&mut unread_logged, true)
        {
            aterm_log::info!("stream @{sid} dropped a line that is no record ({error})");
        }
        let mut after = After::default();
        let stepped = step(
            &handle,
            &binding,
            sent,
            &mut budget,
            now_ms(),
            services.claims,
            &mut after,
        );
        finish(&handle, after, services);
        match stepped {
            Step::Ack { seq, dropped } => {
                let written = ack_line(seq, dropped).map(|line| write_line(stream, &line));
                if !matches!(written, Some(Ok(()))) {
                    break "the acknowledgement was not taken".to_string();
                }
            }
            Step::Close(why) => break why,
        }
        // What the record moved (a composer, a grant), or what rang while it was read, is sent
        // after its acknowledgement; so is a held `hand` whose gap ended meanwhile.
        let now = now_ms();
        let held_due = hand.held_until.is_some_and(|until| now >= until);
        if (doorbell.clear() || held_due)
            && let Err(why) = send_owed(stream, &handle, &binding, &mut hand, false, now)
        {
            break why;
        }
    };
    aterm_log::info!("stream @{sid} closed: {why}");
}

#[cfg(test)]
#[path = "control_stream_tests.rs"]
mod tests;
