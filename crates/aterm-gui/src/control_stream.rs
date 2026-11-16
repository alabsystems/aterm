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
//! Every harness record is acknowledged. What is not applied says why (`dropped`): a line over
//! the bound, a record over the stream's budget, one the claim's grant does not cover, one
//! applied before. A sequence gap, a line that is no record (it has no number to acknowledge), or
//! the claim leaving the connection closes the stream; the harness reopens and resends from
//! `acked`, which the session keeps beside the claim ([`crate::session_timeline::StreamState`]).
//!
//! A record is data: nothing in one is typed, run or opened. This module is Unix only, as the
//! claim's proof is; the state it keeps names no wire type.

use std::io::{Read, Write};
use std::time::Duration;

use alab_wire::Scope as WireScope;
use alab_wire::record::{
    Body, Dropped, LINE_MAX, RATE_BURST, RATE_PER_S, Record, STREAM_VERSION, Side,
};
use aterm_agent::harness::contract::UNIVERSAL;
use aterm_slog::Bucket;
use aterm_uds::CtlStream;

use super::control_lanes::Outbox;
use super::{Request, Scope, Selector, ServeDisposition, Sid, Store};
use crate::SessionCtx;
use crate::harness_claim::{Proven, Scoped};
use crate::session_timeline::{SessionMeta, StreamState};

/// The verb's one spelling: verb first, as `subscribe @<sid> events` is, so `@<sid> stream v=1`
/// is answered this too.
const USAGE: &str = "ERR usage: stream @<sid> v=1\n";
/// No live scoped contract 4 claim is bound to the connection that asked.
const NO_CLAIM: &str =
    "ERR usage: stream opens only for a contract 4 claim placed on this connection\n";
const NOT_OWNER: &str = "ERR denied: stream is owner-only\n";
const NO_SESSION: &str = "ERR no such session\n";

/// How often the reader looks at the claim while its peer sends nothing.
const TICK: Duration = Duration::from_millis(250);
/// How long the peer has to take an acknowledgement before the stream ends: a hang detector.
const ACK_WRITE: Duration = Duration::from_secs(60);
/// How much one read takes from the socket.
const CHUNK: usize = 16 * 1024;

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

/// The live scoped contract 4 claim bound to connection `conn`, if `meta` holds one.
fn claim_on(meta: &SessionMeta, conn: u64, now_us: u64) -> Option<&Scoped> {
    meta.live_claim(now_us)
        .filter(|claim| claim.conn == Some(conn))
        .and_then(|claim| claim.scoped.as_ref())
        .filter(|scoped| scoped.claim.contract == UNIVERSAL)
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
    claim_on(&meta, conn, crate::metrics::now_us())
        .is_none()
        .then_some(NO_CLAIM)
}

/// One line off the stream.
enum Line {
    /// A whole line, without its newline.
    Whole(Vec<u8>),
    /// A line longer than [`LINE_MAX`], discarded up to its newline and never parsed.
    Over,
    /// Nothing arrived for [`TICK`].
    Tick,
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
    const fn new(buffered: Vec<u8>) -> Self {
        Self {
            buffer: buffered,
            scanned: 0,
            over: false,
        }
    }

    fn next(&mut self, stream: &CtlStream) -> Line {
        loop {
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
            let mut chunk = [0u8; CHUNK];
            let mut reader = stream;
            match reader.read(&mut chunk) {
                Ok(0) => return Line::Ended,
                Ok(read) => self.buffer.extend_from_slice(&chunk[..read]),
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
                Err(error)
                    if matches!(
                        error.kind(),
                        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                    ) =>
                {
                    return Line::Tick;
                }
                Err(_) => return Line::Ended,
            }
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
/// `now` (the awake clock), and move the state.
///
/// In order: a line over the bound (`None`) is record `acked+1` and is dropped unparsed, because
/// sequence numbers have no gaps. A line that is no record has no number: nothing can be
/// acknowledged for it, so it closes the stream as a gap does. A `seq` above `acked+1` is a gap
/// and closes the stream. A `beat` counts as liveness as it arrives. A `seq` at or below `acked`
/// was applied before. A record with less than one token in the budget is dropped; only a record
/// that has one spends it, so the tokens that came back while records were dropped stay. A
/// record aterm sends, or one the grant does not cover, is dropped. `beat` is applied; `title`,
/// an unknown kind and every kind this aterm does not apply yet are acknowledged and counted,
/// and are not added to the applied kinds.
fn judge(
    state: &mut StreamState,
    line: Option<&[u8]>,
    granted: &[WireScope],
    bucket: &mut Bucket,
    now: u64,
) -> Step {
    let next = state.acked.saturating_add(1);
    let Some(line) = line else {
        state.acked = next;
        state.dropped += 1;
        return Step::Ack {
            seq: next,
            dropped: Some(Dropped::Bound),
        };
    };
    let Record { seq, body, .. } = match Record::read(line) {
        Ok(decoded) => decoded.record,
        Err(error) => {
            return Step::Close(format!("no record ({error}) acked={}", state.acked));
        }
    };
    if seq > next {
        return Step::Close(format!("seq={seq} gap acked={}", state.acked));
    }
    if matches!(body, Body::Beat) {
        state.last_beat_ms = Some(now);
    }
    let drop = |state: &mut StreamState, why: Dropped| {
        state.dropped += 1;
        Step::Ack {
            seq,
            dropped: Some(why),
        }
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
    match body {
        Body::Beat => {
            state.applied.insert(alab_wire::Kind::Beat.word());
        }
        Body::Title { .. } => state.titles += 1,
        Body::Unknown { .. } => state.unknown += 1,
        _ => state.unapplied += 1,
    }
    Step::Ack { seq, dropped: None }
}

/// Judge `line` for the stream of `proven` on connection `conn`, under the session's metadata
/// lock: the claim is read again for every line, so a record is held to the grant that stands
/// when it arrives, and a claim that left this connection ends the stream.
fn step(
    ctx: &SessionCtx,
    conn: u64,
    proven: &Proven,
    line: Option<&[u8]>,
    bucket: &mut Bucket,
    now: u64,
) -> Step {
    let mut meta = ctx.meta.lock().unwrap_or_else(|p| p.into_inner());
    let Some(granted) = claim_on(&meta, conn, crate::metrics::now_us())
        .filter(|scoped| scoped.proven == *proven)
        .map(granted)
    else {
        return Step::Close("the claim left this connection".to_string());
    };
    match meta.stream.as_mut().filter(|state| state.proven == *proven) {
        Some(state) => judge(state, line, &granted, bucket, now),
        None => Step::Close("the stream's state went to another claimant".to_string()),
    }
}

/// Whether the claim the stream opened under still stands on connection `conn`.
fn bound(ctx: &SessionCtx, conn: u64, proven: &Proven) -> bool {
    let meta = ctx.meta.lock().unwrap_or_else(|p| p.into_inner());
    claim_on(&meta, conn, crate::metrics::now_us()).is_some_and(|scoped| scoped.proven == *proven)
}

fn write_line(stream: &CtlStream, line: &str) -> std::io::Result<()> {
    let mut writer = stream;
    writer.write_all(line.as_bytes())?;
    writer.write_all(b"\n")
}

/// The acknowledgement of harness record `seq`, as the wire library writes it.
fn ack_line(seq: u64, dropped: Option<Dropped>) -> Option<String> {
    let ms = aterm_time::since_epoch().map_or(0, |since| {
        u64::try_from(since.as_millis()).unwrap_or(u64::MAX)
    });
    Record {
        seq,
        ms,
        body: Body::Ack { dropped },
    }
    .line()
    .ok()
}

/// SERVE ONE RECORD STREAM for its whole life, on the stream lane that entered the connection
/// (its serial, descriptor, direct peer, principal and caller). `buffered` is what was read
/// past the verb line; `now_ms` is the awake clock. Returning ends the connection.
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
    now_ms: &dyn Fn() -> u64,
) {
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
    let opened = {
        let mut meta = ctx.meta.lock().unwrap_or_else(|p| p.into_inner());
        let proven = claim_on(&meta, conn, crate::metrics::now_us()).map(|s| s.proven.clone());
        proven.map(|proven| {
            let state = meta.open_stream(&proven, epoch);
            (proven, state.acked, state.sent)
        })
    };
    let Some((proven, acked, sent)) = opened else {
        aterm_log::info!("stream @{sid} closed before it opened: the claim left this connection");
        return;
    };
    // The request lanes' 250 ms read poll gives way to this reader's own tick.
    if stream.set_read_timeout(Some(TICK)).is_err()
        || stream.set_write_timeout(Some(ACK_WRITE)).is_err()
    {
        return;
    }
    let ok = format!("OK stream v={STREAM_VERSION} epoch={epoch} acked={acked} in={sent}");
    if write_line(stream, &ok).is_err() {
        return;
    }
    let refill_ms = 1000 / u64::from(RATE_PER_S.max(1));
    let mut bucket = Bucket::new(RATE_BURST, refill_ms, now_ms());
    let mut lines = Lines::new(buffered);
    let why = loop {
        let line = match lines.next(stream) {
            Line::Whole(line) => Some(line),
            Line::Over => None,
            Line::Tick if bound(ctx, conn, &proven) => continue,
            Line::Tick => break "the claim left this connection".to_string(),
            Line::Ended => break "the peer closed".to_string(),
        };
        match step(ctx, conn, &proven, line.as_deref(), &mut bucket, now_ms()) {
            Step::Ack { seq, dropped } => {
                let written = ack_line(seq, dropped).map(|line| write_line(stream, &line));
                if !matches!(written, Some(Ok(()))) {
                    break "the acknowledgement was not taken".to_string();
                }
            }
            Step::Close(why) => break why,
        }
    };
    aterm_log::info!("stream @{sid} closed: {why}");
}

#[cfg(test)]
#[path = "control_stream_tests.rs"]
mod tests;
