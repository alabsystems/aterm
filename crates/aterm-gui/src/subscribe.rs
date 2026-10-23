// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The real-time PUSH face (design P1.3) — the additive layer that turns the poll-only control socket into a server-PUSH face, so one agent watches its OWN and OTHER sessions LIVE without busy-polling.

use std::io::{IoSlice, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use aterm_core::terminal::Terminal;

use crate::cast::{ByteFanout, ByteSubscription};
use crate::control::Scope;
use crate::frame_key::{FrameKey, TextKey};
use crate::frame_memo::{CellsHold, FrameSlot, ScreenText, Styled, StyledFrame};
use crate::session_store::Store;
use crate::session_watch::{Subscription, Watchers};
use crate::turn_ledger::TurnLedger;

/// The per-target streams a subscription may watch: exactly the frames the per-target `ReadScreen` gate in [`crate::control`] authorizes, one resolved selector at a time.
#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
pub(crate) struct TargetStreams {
    /// Emit `DELTA <sid> seq=<n> screen <changed rows>` when content advances.
    pub screen: bool,
    /// Emit `DELTA <sid> seq=<n> cursor <row> <col> <visible> <style>` on any caret change (position, DECSCUSR style, or DECTCEM visibility) — even at an unchanged seq.
    pub cursor: bool,
    /// The lifecycle digest: `EVENT <sid> block-complete <id> exit=<code>` on OSC 133 D, `EVENT <sid> turn <id> submitted=<0|1> status=<..> dur_ms=<..>` per completed turn (the turn ledger), `EVENT <sid> title <pct>` on an OSC 0/2 title change, `EVENT <sid> bell total=<n>` on a BEL, the timeline kinds [`timeline_wire_kind`] carries (`meta field=<f> value=<pct|->`, `agent`, `status`, `human`, `render`, the fabric digest), and at retirement `EVENT <sid> closing reason=<token> by=<sid|human|ctl|bridge|->` then `EVENT <sid> exited`.
    pub events: bool,
    /// Emit `DELTA <sid> seq=<n> cells <nbytes>\n<styled-json>` when the styled frame changes — the LOSSLESS styled-screen frame (Item 1's payload) pushed live, so an outer agent sees the inner TUI's colour/attrs, not plaintext.
    pub cells: bool,
    /// Emit `BYTES <sid> <len>\n<raw bytes>` for every program-output burst: live, byte-lossless, never coalesced (the per-subscriber queue holds every burst).
    pub bytes: bool,
    /// The `trim` modifier: a `screen` DELTA stops after the last non-blank row (`DELTA <sid> seq=<n> screen <nrows>`, `<nrows>` the count sent), by the rule of `text trim` ([`crate::control::trimmed_len`]).
    pub trim: bool,
    /// Emit `MAIL <local> id=<n> off=<n> from=<p> kind=<k> [re=<n>]`, one line per row delivered into the watched session's inbox, and a `GAP` when the ring evicted rows this subscriber had not been shown.
    pub mail: bool,
    /// Emit `LOG <local> <record>`, one line per record appended to the watched session's SESSION LOG (phase 3A), at the host tier `log` shows by default (never the content tier: no OSC title), filtered as `log lvl= class=turn` filter ([`crate::session_log::shows`]), and `GAP <local> log-lost=<n> why=<w> resync_to=<seq> ep=<e>` when the records past the watch's cursor cross a loss, exactly as the read counts it (seqs have holes across a carried update, so no id arithmetic would do).
    pub log: bool,
}

impl TargetStreams {
    #[must_use]
    fn wants_content(self) -> bool {
        self.screen || self.cursor || self.cells
    }

    #[must_use]
    fn any_frame_source(self) -> bool {
        self.screen
            || self.cursor
            || self.events
            || self.cells
            || self.bytes
            || self.mail
            || self.log
    }
}

/// What the client asked for at instance scope, before any authority check.
#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
pub(crate) struct RequestedInstance {
    /// The instance lifecycle stream: `EVENT * session-created <sid>` when any session spawns and `EVENT * session-exited <sid> reason=<shell-exit|ctl-close|ui-close|window-close|app-quit|unknown>` when one closes, so a supervisor learns of siblings without polling.
    pub sessions: bool,
}

/// The instance-scoped streams a subscription was granted.
#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
pub(crate) struct InstanceStreams {
    sessions: bool,
}

impl InstanceStreams {
    /// Grant the instance-scoped streams `req` asked for: all of them to Owner (the scope the `sessions` and `who` verbs demand), the empty set to every other scope.
    #[must_use]
    pub(crate) fn authorize(req: RequestedInstance, scope: Scope) -> Self {
        InstanceStreams {
            sessions: req.sessions && scope.is_owner_class(),
        }
    }

    /// Whether the instance lifecycle stream was granted.
    #[must_use]
    pub(crate) fn sessions(self) -> bool {
        self.sessions
    }
}

/// A parsed `<streams>` token, split by scope.
#[derive(Clone, Default, PartialEq, Eq, Debug)]
pub(crate) struct Requested {
    /// The per-target frame sources, gated once per resolved selector.
    pub targets: TargetStreams,
    /// `mail`'s `kinds=`/`from=` filter.
    pub mail: crate::fabric::MailFilter,
    /// `log`'s `lvl=`/`class=turn` filter.
    pub log: LogFilter,
    /// The instance-wide frame sources, gated once for the connection.
    pub instance: RequestedInstance,
    /// A modifier, not a frame source: prefix a wake's frames with `T <tag> <t_us>` lines stamped on the same clock as `video`'s `index.json`, so a driver can align the stream against a `video` take.
    pub timestamps: bool,
}

fn fold_mail_params(s: &str) -> Vec<String> {
    let is_stream = |t: &str| {
        let head = t.split_once(':').map_or(t, |(head, _)| head);
        head == "ts" || aterm_types::control_verbs::SUBSCRIBE_STREAMS.contains(&head)
    };
    let mut out: Vec<String> = Vec::new();
    for tok in s.split([',', ' ', '\t']).filter(|t| !t.is_empty()) {
        let continues_mail = out
            .last()
            .is_some_and(|last| last.starts_with("mail:") && !is_stream(tok));
        if continues_mail {
            let last = out.last_mut().expect("checked above");
            last.push(',');
            last.push_str(tok);
        } else {
            out.push(tok.to_string());
        }
    }
    out
}

fn is_sender_selector(v: &str) -> bool {
    if matches!(v, "human" | "agent" | "service" | "other") {
        return true;
    }
    let shaped = v
        .strip_prefix("h-")
        .or_else(|| v.strip_prefix("s-"))
        .or_else(|| v.strip_prefix("a-"));
    shaped.is_some_and(|rest| {
        !rest.is_empty()
            && rest
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '@'))
    })
}

fn parse_mail_params(spec: &str) -> Option<crate::fabric::MailFilter> {
    let mut want = crate::fabric::MailFilter::default();
    if spec.is_empty() {
        return Some(want);
    }
    for part in spec.strip_prefix(':')?.split(':') {
        if let Some(v) = part.strip_prefix("kinds=") {
            if !want.kinds.is_empty() || v.is_empty() {
                return None;
            }
            for k in v.split(',') {
                if !aterm_types::fabric_grammar::is_kind(k) {
                    return None;
                }
                want.kinds.push(k.to_string());
            }
        } else if let Some(v) = part.strip_prefix("topic=") {
            if want.topic.is_some() || !aterm_types::fabric_grammar::is_topic(v) {
                return None;
            }
            want.topic = Some(v.to_string());
        } else {
            let v = part.strip_prefix("from=")?;
            if want.from.is_some() || !is_sender_selector(v) {
                return None;
            }
            want.from = Some(v.to_string());
        }
    }
    Some(want)
}

/// `log`'s filter (phase 3A): the deepest level shown (`log:lvl=<l>`) and the turn class only (`log:class=turn`), as the `log` verb's `lvl=` and `class=turn` read ([`crate::session_log::shows`]).
#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
pub(crate) struct LogFilter {
    /// `lvl=`: no record of a deeper level.
    pub lvl: Option<aterm_slog::Lvl>,
    /// `class=turn`: only the turn class.
    pub class_turn: bool,
}

fn parse_log_params(spec: &str) -> Option<LogFilter> {
    let mut want = LogFilter::default();
    if spec.is_empty() {
        return Some(want);
    }
    let (mut lvl, mut class) = (false, false);
    for part in spec.strip_prefix(':')?.split(':') {
        match part.split_once('=')? {
            ("lvl", v) if !lvl => {
                lvl = true;
                want.lvl = Some(match v {
                    "session" => aterm_slog::Lvl::Session,
                    "run" => aterm_slog::Lvl::Run,
                    "turn" => aterm_slog::Lvl::Turn,
                    "step" => aterm_slog::Lvl::Step,
                    _ => return None,
                });
            }
            ("class", "turn") if !class => {
                class = true;
                want.class_turn = true;
            }
            _ => return None,
        }
    }
    Some(want)
}

impl Requested {
    /// Parse a whitespace-or-comma separated stream list (`screen,cursor,events`).
    #[must_use]
    pub fn parse(s: &str) -> Option<Requested> {
        let mut out = Requested::default();
        for tok in fold_mail_params(s) {
            let tok = tok.as_str();
            if let Some(spec) = tok.strip_prefix("mail") {
                out.targets.mail = true;
                out.mail = parse_mail_params(spec)?;
                continue;
            }
            if let Some(spec) = tok.strip_prefix("log") {
                if out.targets.log {
                    return None;
                }
                out.targets.log = true;
                out.log = parse_log_params(spec)?;
                continue;
            }
            match tok {
                "screen" => out.targets.screen = true,
                "cursor" => out.targets.cursor = true,
                "events" => out.targets.events = true,
                "cells" => out.targets.cells = true,
                "bytes" => out.targets.bytes = true,
                "timestamps" | "ts" => out.timestamps = true,
                "trim" => out.targets.trim = true,
                "sessions" => out.instance.sessions = true,
                _ => return None,
            }
        }
        let any_source = out.targets.any_frame_source() || out.instance.sessions;
        any_source.then_some(out)
    }
}

/// The subscription knobs that carry no authority: where each stream resumes from and how a wake is rendered.
#[derive(Clone, Default, PartialEq, Eq, Debug)]
pub(crate) struct PushOptions {
    /// `since=<seq>`: the client's last-seen `content_seq`.
    pub since: Option<u64>,
    /// `since-turn=<id>`: resume the `events` turn stream from that ledger id.
    pub since_turn: Option<u64>,
    /// `since-block=<id>`: resume the `events` block stream from that block id.
    pub since_block: Option<u64>,
    /// `every-frame`: re-read the `cells` frame on every wake of its session even when the frame key stands (animation fidelity), sending it only when its body changed.
    pub non_coalesced: bool,
    /// The `timestamps`/`ts` modifier — see [`Requested::timestamps`].
    pub timestamps: bool,
    /// `mail`'s row filter ([`Requested::mail`]).
    pub mail: crate::fabric::MailFilter,
    /// `log`'s filter ([`Requested::log`]), no authority, like `mail`'s.
    pub log: LogFilter,
    /// `since-log=<seq> log-ep=<e>`: resume the `log` stream after that record of that epoch (a `log`/`brief`/`status`/`await log` reply's), the read saying any loss in between as a `GAP`.
    pub log_since: Option<aterm_slog::Cursor>,
    /// `@*`: the roster journal's seq when the target set was resolved.
    pub adopt_after: Option<u64>,
}

struct Watch {
    local_id: u64,
    term: Arc<Mutex<Terminal>>,
    src: WatchSource,
    last_sent_seq: u64,
    last_alt: bool,
    last_cursor: (u16, u16, bool, &'static str),
    last_text: TextKey,
    last_rows_hash: u64,
    last_cells_etag: u64,
    last_cells_body: Option<(u64, usize)>,
    _cells_hold: Option<CellsHold>,
    last_block_id: Option<u64>,
    last_turn_id: Option<u64>,
    last_timeline_id: Option<u64>,
    last_resize_id: u64,
    last_title: Option<String>,
    last_bell: u64,
    byte_sub: Option<ByteSubscription>,
    non_coalesced: bool,
    pace: Paces,
    records_read: u64,
    last_mail_id: u64,
    mail: crate::fabric::MailFilter,
    dirty: Arc<AtomicBool>,
    log: Option<LogWatch>,
    closing: bool,
    timeline_held: bool,
}

#[derive(Clone, Copy, Debug)]
struct LogWatch {
    cursor: aterm_slog::Cursor,
    filter: LogFilter,
}

/// Authority for the `@*` live target set: whether this subscription may adopt sessions created after it subscribed.
#[derive(Clone, Copy)]
pub(crate) struct AdoptScope(bool);

impl AdoptScope {
    /// Grant `@*` adoption iff it was requested and the connection is Owner.
    #[must_use]
    pub(crate) fn authorize(requested: bool, scope: Scope) -> Self {
        AdoptScope(requested && scope.is_owner_class())
    }

    /// The refusing default — no adoption: `authorize(false, _)` for the tests.
    #[cfg(test)]
    #[must_use]
    pub(crate) fn none() -> Self {
        AdoptScope(false)
    }

    fn on(self) -> bool {
        self.0
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Tag {
    Channel(u64),
    Instance,
}

impl std::fmt::Display for Tag {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Tag::Channel(id) => write!(f, "{id}"),
            Tag::Instance => f.write_str("*"),
        }
    }
}

fn sid_tag(local_id: u64) -> String {
    Tag::Channel(local_id).to_string()
}

fn sid_on_stack(local_id: u64, buf: &mut [u8; 20]) -> &str {
    let len = {
        let mut w = &mut buf[..];
        let _ = write!(w, "{local_id}");
        20 - w.len()
    };
    std::str::from_utf8(&buf[..len]).unwrap_or("")
}

enum Seg {
    Text(String),
    Shared(Arc<StyledFrame>),
    Bytes(Arc<[u8]>),
    Lit(&'static [u8]),
}

impl Seg {
    fn bytes(&self) -> &[u8] {
        match self {
            Seg::Text(text) => text.as_bytes(),
            Seg::Shared(frame) => frame.body().as_bytes(),
            Seg::Bytes(burst) => burst,
            Seg::Lit(lit) => lit,
        }
    }
}

struct Frame {
    tag: Tag,
    t_us: u64,
    segs: Vec<Seg>,
}

impl Frame {
    fn text(tag: Tag, t_us: u64, body: String) -> Frame {
        Frame {
            tag,
            t_us,
            segs: vec![Seg::Text(body)],
        }
    }

    fn is_empty(&self) -> bool {
        self.segs.iter().all(|seg| seg.bytes().is_empty())
    }
}

#[derive(Clone, Copy, Debug)]
struct Gone;

struct Egress<'w, W: Write> {
    writer: &'w mut W,
    timestamps: bool,
    stamped: Vec<Tag>,
}

impl<W: Write> Egress<'_, W> {
    fn new(writer: &mut W, timestamps: bool) -> Egress<'_, W> {
        Egress {
            writer,
            timestamps,
            stamped: Vec::new(),
        }
    }

    fn begin_wake(&mut self) {
        self.stamped.clear();
    }

    fn send(&mut self, frames: Vec<Frame>) -> Result<(), Gone> {
        let mut stamps: Vec<(usize, String)> = Vec::new();
        if self.timestamps {
            for (i, frame) in frames.iter().enumerate() {
                if frame.is_empty() || self.stamped.contains(&frame.tag) {
                    continue;
                }
                self.stamped.push(frame.tag);
                let t_us = frames[i..]
                    .iter()
                    .filter(|f| f.tag == frame.tag && !f.is_empty())
                    .map(|f| f.t_us)
                    .min()
                    .unwrap_or(frame.t_us);
                stamps.push((i, format!("T {} {t_us}\n", frame.tag)));
            }
        }
        let segs: usize = frames.iter().map(|f| f.segs.len()).sum();
        let mut slices = Vec::with_capacity(stamps.len() + segs);
        let mut stamps = stamps.iter().peekable();
        for (i, frame) in frames.iter().enumerate() {
            if let Some((_, line)) = stamps.next_if(|(at, _)| *at == i) {
                slices.push(IoSlice::new(line.as_bytes()));
            }
            slices.extend(
                frame
                    .segs
                    .iter()
                    .map(Seg::bytes)
                    .filter(|bytes| !bytes.is_empty())
                    .map(IoSlice::new),
            );
        }
        write_all_vectored(self.writer, &mut slices).map_err(|_| Gone)
    }
}

fn write_all_vectored<W: Write>(
    writer: &mut W,
    mut slices: &mut [IoSlice<'_>],
) -> std::io::Result<()> {
    while !slices.is_empty() {
        match writer.write_vectored(slices) {
            Ok(0) => return Err(std::io::ErrorKind::WriteZero.into()),
            Ok(n) => IoSlice::advance_slices(&mut slices, n),
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    Ok(())
}

fn frame_screen(sid: &str, seq: u64, text: &ScreenText, rows: usize) -> String {
    let body = text.span(0, rows);
    let mut out = String::with_capacity(body.len() + 48);
    let _ = std::fmt::Write::write_fmt(
        &mut out,
        format_args!("DELTA {sid} seq={seq} screen {rows}\n"),
    );
    out.push_str(body);
    out
}

fn frame_cursor(sid: &str, seq: u64, row: u16, col: u16, visible: bool, style: &str) -> String {
    format!(
        "DELTA {sid} seq={seq} cursor {row} {col} {} {style}\n",
        u8::from(visible)
    )
}

fn frame_cells(sid: &str, seq: u64, frame: Arc<StyledFrame>, head: &mut String) -> [Seg; 3] {
    let _ = std::fmt::Write::write_fmt(
        head,
        format_args!("DELTA {sid} seq={seq} cells {}\n", frame.body().len()),
    );
    [
        Seg::Text(std::mem::take(head)),
        Seg::Shared(frame),
        Seg::Lit(b"\n"),
    ]
}

fn drain_bytes_frames(watch: &Watch) -> Vec<Frame> {
    let Some(bs) = &watch.byte_sub else {
        return Vec::new();
    };
    let (bursts, dropped) = bs.drain();
    if dropped == 0 && bursts.is_empty() {
        return Vec::new();
    }
    let mut sid_buf = [0u8; 20];
    let sid = sid_on_stack(watch.local_id, &mut sid_buf);
    let t_us = bursts
        .first()
        .map_or_else(crate::metrics::now_us, |burst| burst.at_us);
    let mut segs = Vec::with_capacity(2 * bursts.len() + 1);
    let mut text = String::new();
    if dropped > 0 {
        let _ = std::fmt::Write::write_fmt(
            &mut text,
            format_args!("GAP {sid} bytes-dropped={dropped}\n"),
        );
    }
    let mut lead = "";
    for burst in bursts {
        let _ = std::fmt::Write::write_fmt(
            &mut text,
            format_args!("{lead}BYTES {sid} {}\n", burst.bytes.len()),
        );
        segs.push(Seg::Text(std::mem::take(&mut text)));
        segs.push(Seg::Bytes(burst.bytes));
        lead = "\n";
    }
    segs.push(if lead.is_empty() {
        Seg::Text(text)
    } else {
        Seg::Lit(b"\n")
    });
    vec![Frame {
        tag: Tag::Channel(watch.local_id),
        t_us,
        segs,
    }]
}

fn frame_block_complete(sid: &str, id: u64, exit: Option<i32>) -> String {
    let exit = exit.map_or_else(|| "-".to_string(), |c| c.to_string());
    format!("EVENT {sid} block-complete {id} exit={exit}\n")
}

fn drain_title_event(
    sid: &str,
    title: Option<String>,
    last_title: Option<String>,
    out: &mut String,
) -> Option<String> {
    if let Some(title) = title {
        out.push_str(&format!(
            "EVENT {sid} title {}\n",
            crate::control::pct_encode(&title)
        ));
        Some(title)
    } else {
        last_title
    }
}

/// Scan the target's event timeline past `last_id` and push each record kind [`timeline_wire_kind`] carries as `EVENT <sid> <kind>[ <payload>]`, the payload being the record's own pre-encoded tail (`meta`'s is `field=<f> value=<pct|->`).
pub(crate) fn drain_timeline_events(
    tl: &crate::session_timeline::SessionTimeline,
    sid: &str,
    last_id: Option<u64>,
    out: &mut String,
) -> Option<u64> {
    let hi = match tl.high_id() {
        None => return last_id,
        Some(hi) if last_id.is_some_and(|a| hi <= a) => return Some(hi),
        Some(hi) => hi,
    };
    let missed = match (last_id, tl.low_id()) {
        (Some(a), Some(lo)) => lo.saturating_sub(a + 1),
        _ => 0,
    };
    if missed > 0 {
        out.push_str(&format!("GAP {sid} events-dropped={missed}\n"));
    }
    for e in tl.since(last_id) {
        #[cfg(test)]
        crate::work_counts::retained_record_touched();
        let Some(kind) = timeline_wire_kind(e.kind) else {
            continue;
        };
        if e.payload.is_empty() {
            out.push_str(&format!("EVENT {sid} {kind}\n"));
        } else {
            out.push_str(&format!("EVENT {sid} {kind} {}\n", e.payload));
        }
    }
    Some(hi)
}

fn drain_resize_events(
    tl: &mut crate::session_timeline::SessionTimeline,
    sid: &str,
    last_id: u64,
    probe: crate::resize_ledger::EngineProbe,
    out: &mut String,
) -> u64 {
    tl.evaluate_resizes(probe, crate::turn_ledger::now_ms());
    let ledger = tl.resizes();
    let high = ledger.high_row();
    if high <= last_id {
        return high;
    }
    let dropped = ledger
        .low_row()
        .map_or(0, |low| low.saturating_sub(last_id.saturating_add(1)));
    let n = ledger.rows_since(last_id).count();
    let fresh = ledger
        .rows_since(last_id)
        .next_back()
        .map(|row| (n, row.id, row.report.to, row.run));
    if let Some((n, id, (rows, cols), run)) = fresh {
        out.push_str(&format!(
            "EVENT {sid} resize n={n} id={id} to={cols}x{rows} run={run}"
        ));
        if dropped > 0 {
            out.push_str(&format!(" dropped={dropped}"));
        }
        out.push('\n');
    }
    high
}

/// The `EVENT <sid> <kind>` token a timeline record is pushed under on the `events` digest, or `None` for a kind the digest does not carry.
pub(crate) fn timeline_wire_kind(kind: &str) -> Option<&'static str> {
    match kind {
        "meta-change" => Some("meta"),
        "closing" => Some("closing"),
        "agent-change" => Some("agent"),
        "task-change" => Some("task"),
        crate::session_status::STATUS_CHANGE => Some("status"),
        "human" => Some("human"),
        "render" => Some("render"),
        crate::harness_policy_verb::HARNESS_POLICY_EVENT => Some("harness-policy"),
        crate::sys_host::JOB_CHANGE => Some("job"),
        other => crate::fabric::fabric_event_wire_kind(other),
    }
}

fn drain_bell_event(sid: &str, total: u64, last_bell: u64, out: &mut String) -> u64 {
    if total > last_bell {
        out.push_str(&format!("EVENT {sid} bell total={total}\n"));
    }
    total
}

struct RosterCursor {
    seq: u64,
    known: std::collections::HashSet<String>,
}

fn drain_roster(
    g: &crate::session_store::SessionStore,
    cursor: &mut RosterCursor,
    watched: Option<&dyn Fn(u64) -> bool>,
    out: &mut String,
) {
    let high = g.roster_seq();
    if high == cursor.seq {
        // The idle tick: nothing entered or left the registry; one compare.
        return;
    }
    let mark = |sid: &str| {
        let deferred = watched.is_some_and(|w| {
            g.by_sid(&aterm_session::SessionId::new(sid))
                .is_some_and(|h| !w(h.local_id))
        });
        if deferred { " watch=deferred" } else { "" }
    };
    match g.roster_low_seq() {
        Some(low) if cursor.seq + 1 >= low => {
            for rec in g.roster_since(cursor.seq) {
                match rec.change {
                    crate::session_store::RosterChange::Created => {
                        out.push_str(&format!(
                            "EVENT * session-created {}{}\n",
                            rec.sid,
                            mark(&rec.sid)
                        ));
                        cursor.known.insert(rec.sid.clone());
                    }
                    crate::session_store::RosterChange::Exited => {
                        out.push_str(&format!(
                            "EVENT * session-exited {} reason={}\n",
                            rec.sid,
                            rec.reason.as_str()
                        ));
                        cursor.known.remove(&rec.sid);
                    }
                    crate::session_store::RosterChange::RetireRequested => {
                        out.push_str(&format!("EVENT * fabric-retire {}\n", rec.sid));
                    }
                    #[cfg(any(unix, test))]
                    crate::session_store::RosterChange::Bye(bye) => {
                        out.push_str(&format!("EVENT * bye {}\n", bye.word()));
                    }
                }
            }
        }
        _ => {
            let live = g.live_sids();
            diff_session_events(&live, &cursor.known, &mark, out);
            cursor.known = live;
        }
    }
    cursor.seq = high;
}

#[cfg(test)]
fn drain_session_events(store: &Store, cursor: &mut RosterCursor) -> Vec<Frame> {
    let mut out = String::new();
    drain_roster(
        &store.read().unwrap_or_else(|p| p.into_inner()),
        cursor,
        None,
        &mut out,
    );
    if out.is_empty() {
        return Vec::new();
    }
    vec![Frame::text(Tag::Instance, crate::metrics::now_us(), out)]
}

fn diff_session_events(
    live: &std::collections::HashSet<String>,
    known: &std::collections::HashSet<String>,
    mark: &dyn Fn(&str) -> &'static str,
    out: &mut String,
) {
    for sid in live.difference(known) {
        out.push_str(&format!("EVENT * session-created {sid}{}\n", mark(sid)));
    }
    for sid in known.difference(live) {
        out.push_str(&format!(
            "EVENT * session-exited {sid} reason={}\n",
            crate::session_store::ExitReason::Unknown.as_str()
        ));
    }
}

fn drain_turn_events(
    turns: &Arc<Mutex<TurnLedger>>,
    sid: &str,
    last_turn_id: Option<u64>,
    out: &mut String,
) -> Option<u64> {
    let fresh: Vec<(u64, bool, &'static str, u64, String)> = {
        let l = turns.lock().unwrap_or_else(|p| p.into_inner());
        match l.high_id() {
            None => return last_turn_id,
            Some(hi) if last_turn_id.is_some_and(|a| hi <= a) => return last_turn_id,
            Some(_) => l
                .since(last_turn_id)
                .map(|r| {
                    #[cfg(test)]
                    crate::work_counts::retained_record_touched();
                    (
                        r.id,
                        r.submitted,
                        r.status,
                        r.dur_ms,
                        r.by.as_wire().to_string(),
                    )
                })
                .collect(),
        }
    };
    let mut high = last_turn_id;
    for (id, submitted, status, dur_ms, by) in fresh {
        out.push_str(&format!(
            "EVENT {sid} turn {id} submitted={} status={status} dur_ms={dur_ms} by={by}\n",
            u8::from(submitted)
        ));
        high = Some(high.map_or(id, |h| h.max(id)));
    }
    high
}

fn frame_exited(sid: &str) -> String {
    format!("EVENT {sid} exited\n")
}

fn frame_gap(sid: &str, seq: u64) -> String {
    format!("GAP {sid} resync={seq}\n")
}

fn drain_mail_events(watch: &mut Watch, sid: &str, out: &mut String) {
    let since = watch.src.fabric.mail_since(watch.last_mail_id, &watch.mail);
    let issued = since.high.saturating_sub(watch.last_mail_id);
    let missed = issued.saturating_sub(since.present);
    if missed > 0 {
        out.push_str(&format!("GAP {sid} mail-dropped={missed}\n"));
    }
    for row in &since.rows {
        let re = row.re.map_or_else(String::new, |off| format!(" re={off}"));
        let topic = row
            .topic
            .as_ref()
            .map_or_else(String::new, |t| format!(" topic={t}"));
        out.push_str(&format!(
            "MAIL {sid} id={} off={} from={} kind={}{re}{topic}\n",
            row.id, row.off, row.from, row.kind
        ));
    }
    watch.last_mail_id = watch.last_mail_id.max(since.high);
}

fn log_anchor(watch: &Watch) -> String {
    watch.log.as_ref().map_or_else(String::new, |lw| {
        format!(" log_seq={} log_ep={}", lw.cursor.seq, lw.cursor.ep)
    })
}

fn with_log_anchors<'h>(handshake: &'h str, watches: &[Watch]) -> std::borrow::Cow<'h, str> {
    if watches.iter().all(|w| w.log.is_none()) {
        return std::borrow::Cow::Borrowed(handshake);
    }
    let mut out = String::with_capacity(handshake.len() + 32 * watches.len());
    for line in handshake.split_inclusive('\n') {
        let watch = line
            .strip_prefix("sub ")
            .and_then(|rest| rest.split_whitespace().next())
            .and_then(|local| local.parse::<u64>().ok())
            .and_then(|local| watches.iter().find(|w| w.local_id == local));
        match (watch.map(log_anchor), line.strip_suffix('\n')) {
            (Some(anchor), Some(body)) if !anchor.is_empty() => {
                out.push_str(body);
                out.push_str(&anchor);
                out.push('\n');
            }
            _ => out.push_str(line),
        }
    }
    std::borrow::Cow::Owned(out)
}

fn drain_log_events(watch: &mut Watch, sid: &str, out: &mut String) {
    let Some(lw) = watch.log.as_mut() else {
        return;
    };
    let (gap, recs, next) = {
        let tl = watch.src.timeline.lock().unwrap_or_else(|p| p.into_inner());
        let log = tl.slog().log();
        if log.reader_last_seq() == lw.cursor.seq && log.ep() == lw.cursor.ep {
            return;
        }
        let read = log.read_shared(lw.cursor, aterm_slog::MAX_RECORDS);
        let recs: Vec<Arc<aterm_slog::Rec>> = read
            .recs
            .into_iter()
            .filter(|r| crate::session_log::shows(r, lw.filter.lvl, lw.filter.class_turn))
            .map(Arc::clone)
            .collect();
        (read.gap, recs, read.next)
    };
    lw.cursor = next;
    if let Some(g) = gap {
        out.push_str(&format!(
            "GAP {sid} log-lost={} why={} resync_to={} ep={}\n",
            g.lost,
            g.why.word(),
            g.resync_to,
            next.ep
        ));
    }
    for r in &recs {
        let line = aterm_slog::encode(r, aterm_slog::Tier::Host)
            .unwrap_or_else(|_| format!("{} {} {}", r.seq, r.op.word(), r.node));
        out.push_str("LOG ");
        out.push_str(sid);
        out.push(' ');
        out.push_str(&line);
        out.push('\n');
    }
}

fn frame_gap_events(tag: Tag, t_us: u64, floor: u64) -> Frame {
    Frame::text(tag, t_us, format!("GAP {tag} events-resync={floor}\n"))
}

fn drain_block_events(
    sid: &str,
    completed: &[(u64, Option<i32>)],
    last_block_id: Option<u64>,
    out: &mut String,
) -> Option<u64> {
    let mut high = last_block_id;
    for &(id, exit) in completed {
        out.push_str(&frame_block_complete(sid, id, exit));
        high = Some(high.map_or(id, |h| h.max(id)));
    }
    high
}

struct EngineSample {
    blocks: Vec<(u64, Option<i32>)>,
    title: Option<String>,
    bell: u64,
    resize: crate::resize_ledger::EngineProbe,
}

fn sample_engine_events(
    term: &Arc<Mutex<Terminal>>,
    last_block_id: Option<u64>,
    last_title: Option<&str>,
) -> EngineSample {
    #[cfg(test)]
    crate::work_counts::engine_sampled();
    let t = crate::term_lock(term);
    let blocks = match t.newest_completed_block().map(|b| b.id) {
        None => Vec::new(),
        Some(hi) if last_block_id.is_some_and(|h| hi <= h) => Vec::new(),
        Some(_) => {
            let mut v: Vec<(u64, Option<i32>)> = Vec::new();
            for b in t.all_blocks().rev() {
                #[cfg(test)]
                crate::work_counts::retained_record_touched();
                if last_block_id.is_some_and(|h| b.id <= h) {
                    break;
                }
                if b.is_complete() {
                    v.push((b.id, b.exit_code));
                }
            }
            v.reverse();
            v
        }
    };
    EngineSample {
        blocks,
        title: (last_title != Some(t.title())).then(|| t.title().to_string()),
        bell: t.bell_total(),
        resize: crate::resize_ledger::EngineProbe::sample(&t),
    }
}

fn frames_for_watch(watch: &mut Watch, streams: TargetStreams, woke: bool) -> Vec<Frame> {
    #[cfg(test)]
    crate::work_counts::watch_visited();
    let mut sid_buf = [0u8; 20];
    let sid = sid_on_stack(watch.local_id, &mut sid_buf);
    let tag = Tag::Channel(watch.local_id);
    let mut frames = Vec::new();
    let mut out = String::new();

    if streams.wants_content() {
        let mut look = watch.src.frame.look_frame();
        let (t_us, seq, alt, gap, advance, cur, text, rows, cells_etag, styled) = {
            let t = crate::term_lock(&watch.term);
            let t_us = crate::metrics::now_us();
            let seq = t.content_seq();
            let alt = t.is_alternate_screen();
            let gap = seq < watch.last_sent_seq || alt != watch.last_alt;
            let advance = gap || seq != watch.last_sent_seq;
            let cur = streams.cursor.then(|| {
                let c = t.cursor();
                (
                    c.row,
                    c.col,
                    t.cursor_visible(),
                    crate::control::cursor_style_name(t.cursor_style()),
                )
            });
            let key = streams.cells.then(|| FrameKey::read(&t));
            let text = streams
                .screen
                .then(|| key.map_or_else(|| TextKey::read(&t), |k| k.text()));
            let rows = text
                .filter(|text| advance || *text != watch.last_text)
                .map(|_| look.text(&t));
            let cells_etag = key.map(|k| k.etag());
            let styled = key
                .filter(|k| {
                    advance || k.etag() != watch.last_cells_etag || (watch.non_coalesced && woke)
                })
                .map(|k| (k, look.styled(&t, &k)));
            (
                t_us, seq, alt, gap, advance, cur, text, rows, cells_etag, styled,
            )
        };
        let cells_payload = styled
            .map(|(key, styled)| match styled {
                Styled::Hit(frame) => frame,
                Styled::Miss(snap) => look.publish_styled(key, &snap),
            })
            .filter(|frame| {
                let fresh = advance || watch.last_cells_body != Some(frame.id());
                watch.last_cells_body = Some(frame.id());
                fresh
            });
        let screen_rows = rows.filter(|text| {
            let moved = advance || text.fnv() != watch.last_rows_hash;
            watch.last_rows_hash = text.fnv();
            moved
        });
        if let Some(text) = text {
            watch.last_text = text;
        }
        if let Some(etag) = cells_etag {
            watch.last_cells_etag = etag;
        }
        if gap {
            out.push_str(&frame_gap(sid, seq));
        }
        if advance {
            watch.last_sent_seq = seq;
            watch.last_alt = alt;
        }
        if let Some(text) = screen_rows.as_deref() {
            let n = if streams.trim {
                crate::control::trimmed_len(text.rows())
            } else {
                text.row_count()
            };
            out.push_str(&frame_screen(sid, seq, text, n));
        }
        if let Some(cur) = cur
            && (advance || cur != watch.last_cursor)
        {
            let (cr, cc, cv, cs) = cur;
            out.push_str(&frame_cursor(sid, seq, cr, cc, cv, cs));
            watch.last_cursor = cur;
        }
        let segs = match cells_payload {
            Some(frame) => Vec::from(frame_cells(sid, seq, frame, &mut out)),
            None if out.is_empty() => Vec::new(),
            None => vec![Seg::Text(std::mem::take(&mut out))],
        };
        if !segs.is_empty() {
            frames.push(Frame { tag, t_us, segs });
        }
    }

    let drained_at = (streams.events || streams.mail || streams.log).then(crate::metrics::now_us);
    if streams.events {
        let engine = sample_engine_events(
            &watch.term,
            watch.last_block_id,
            watch.last_title.as_deref(),
        );
        watch.last_block_id =
            drain_block_events(sid, &engine.blocks, watch.last_block_id, &mut out);
        watch.last_turn_id = drain_turn_events(&watch.src.turns, sid, watch.last_turn_id, &mut out);
        {
            let mut tl = watch.src.timeline.lock().unwrap_or_else(|p| p.into_inner());
            watch.last_resize_id =
                drain_resize_events(&mut tl, sid, watch.last_resize_id, engine.resize, &mut out);
            watch.timeline_held = !watch.closing && timeline_held(&tl);
            if !watch.timeline_held {
                watch.last_timeline_id =
                    drain_timeline_events(&tl, sid, watch.last_timeline_id, &mut out);
            }
        }
        watch.last_title = drain_title_event(sid, engine.title, watch.last_title.take(), &mut out);
        watch.last_bell = drain_bell_event(sid, engine.bell, watch.last_bell, &mut out);
    }

    if streams.mail {
        drain_mail_events(watch, sid, &mut out);
    }

    if streams.log {
        drain_log_events(watch, sid, &mut out);
    }

    if let Some(t_us) = drained_at
        && !out.is_empty()
    {
        frames.push(Frame::text(tag, t_us, out));
    }
    frames
}

/// What a watch reads from its session besides the engine: the byte fan-out (the `bytes` stream), the turn ledger and the event timeline (the `events` digest), the inbox (`mail`), and the published frame (`screen`, `cells`); and the watcher list its subscription registers on, which the writes to all of these ring.
#[derive(Clone)]
pub(crate) struct WatchSource {
    byte_fanout: Arc<ByteFanout>,
    turns: Arc<Mutex<TurnLedger>>,
    timeline: Arc<Mutex<crate::session_timeline::SessionTimeline>>,
    fabric: Arc<crate::fabric::SessionFabric>,
    frame: Arc<FrameSlot>,
    watchers: Arc<Watchers>,
}

impl WatchSource {
    /// The leaves of `ctx` a watch reads, and nothing else of it.
    pub(crate) fn of(ctx: &crate::SessionCtx) -> Self {
        WatchSource {
            byte_fanout: ctx.byte_fanout.clone(),
            turns: ctx.turns.clone(),
            timeline: ctx.timeline.clone(),
            fabric: ctx.fabric.clone(),
            frame: ctx.frame.clone(),
            watchers: ctx.watchers.clone(),
        }
    }
}

/// A resolved subscribe target, cloned OUT of the store at subscribe time: the process-local id, the live engine handle, and the session's [`WatchSource`].
pub(crate) struct ResolvedTarget {
    pub(crate) local_id: u64,
    pub(crate) term: Arc<Mutex<Terminal>>,
    pub(crate) src: WatchSource,
}

#[derive(Default)]
struct EngineSeed {
    title: Option<String>,
    bell: u64,
    newest_block: Option<u64>,
    alt: bool,
    text: TextKey,
    rows_hash: u64,
    cells_etag: u64,
}

fn engine_seed(
    term: &Arc<Mutex<Terminal>>,
    frame: &FrameSlot,
    streams: TargetStreams,
) -> EngineSeed {
    let (events, content) = (streams.events, streams.wants_content());
    if !events && !content {
        return EngineSeed::default();
    }
    let mut look = streams.screen.then(|| frame.look_frame());
    let t = crate::term_lock(term);
    EngineSeed {
        title: events.then(|| t.title().to_string()),
        bell: if events { t.bell_total() } else { 0 },
        newest_block: if events {
            t.newest_completed_block().map(|b| b.id)
        } else {
            None
        },
        alt: content && t.is_alternate_screen(),
        text: if streams.screen {
            TextKey::read(&t)
        } else {
            TextKey::default()
        },
        rows_hash: look.as_mut().map_or(0, |look| look.text(&t).fnv()),
        cells_etag: if streams.cells {
            FrameKey::read(&t).etag()
        } else {
            0
        },
    }
}

fn new_watch(target: &ResolvedTarget, streams: TargetStreams, opts: &PushOptions) -> Watch {
    new_watch_at(target, streams, opts, false)
}

fn new_watch_at(
    target: &ResolvedTarget,
    streams: TargetStreams,
    opts: &PushOptions,
    log_from_start: bool,
) -> Watch {
    let ResolvedTarget {
        local_id,
        term,
        src,
    } = target;
    let records_read = src.watchers.records_rung();
    let engine = engine_seed(term, &src.frame, streams);
    let (last_timeline_id, last_resize_id) = initial_timeline_watermarks(&src.timeline, streams);
    Watch {
        local_id: *local_id,
        term: term.clone(),
        last_mail_id: if streams.mail {
            src.fabric.last_msg_id()
        } else {
            0
        },
        mail: opts.mail.clone(),
        log: streams.log.then(|| LogWatch {
            cursor: opts.log_since.unwrap_or_else(|| {
                let tl = src.timeline.lock().unwrap_or_else(|p| p.into_inner());
                let log = tl.slog().log();
                let from = if log_from_start {
                    0
                } else {
                    log.reader_last_seq()
                };
                aterm_slog::Cursor::new(log.ep(), from)
            }),
            filter: opts.log,
        }),
        closing: false,
        timeline_held: false,
        last_turn_id: opts
            .since_turn
            .filter(|anchor| *anchor <= crate::control::turn_ids_minted())
            .or_else(|| initial_turn_watermark(&src.turns, streams)),
        last_timeline_id,
        last_resize_id,
        last_title: engine.title,
        last_bell: engine.bell,
        last_sent_seq: opts.since.unwrap_or(0),
        last_alt: engine.alt,
        last_cursor: (u16::MAX, u16::MAX, false, ""),
        last_text: engine.text,
        last_rows_hash: engine.rows_hash,
        last_cells_etag: engine.cells_etag,
        last_cells_body: None,
        _cells_hold: streams.cells.then(|| FrameSlot::hold_cells(&src.frame)),
        last_block_id: resume_block_watermark(opts.since_block, engine.newest_block),
        byte_sub: streams.bytes.then(|| src.byte_fanout.subscribe()),
        non_coalesced: opts.non_coalesced,
        pace: Paces::default(),
        records_read,
        src: src.clone(),
        dirty: Arc::default(),
    }
}

/// The PUSH LOOP for a `subscribe` connection.
#[derive(Clone, Copy)]
pub(crate) struct PushScopes {
    /// Per-target `ReadScreen` authority, one check per entry of `targets`.
    pub streams: TargetStreams,
    /// Connection-wide authority; the only key to the whole-instance roster.
    pub instance: InstanceStreams,
    /// Whether targets that appear after subscription may be adopted.
    pub adopt: AdoptScope,
}

struct PushCursors {
    roster: Option<RosterCursor>,
    adopt_seq: u64,
    pruned_at: Option<u64>,
    instance_dirty: Option<Arc<AtomicBool>>,
    reader: Option<RosterReader>,
}

struct RosterReaders {
    next: u64,
    written: std::collections::BTreeMap<u64, (usize, u64)>,
}

static ROSTER_READERS: Mutex<RosterReaders> = Mutex::new(RosterReaders {
    next: 0,
    written: std::collections::BTreeMap::new(),
});

static ROSTER_WRITTEN: std::sync::Condvar = std::sync::Condvar::new();

struct RosterReader {
    key: u64,
    store: usize,
    written: u64,
}

fn store_key(store: &Store) -> usize {
    Arc::as_ptr(store).addr()
}

impl RosterReader {
    fn join(store: &Store, seq: u64) -> Self {
        let store = store_key(store);
        let mut readers = ROSTER_READERS.lock().unwrap_or_else(|p| p.into_inner());
        let key = readers.next;
        readers.next += 1;
        readers.written.insert(key, (store, seq));
        Self {
            key,
            store,
            written: seq,
        }
    }

    fn wrote(&mut self, seq: u64) {
        if seq <= self.written {
            return;
        }
        self.written = seq;
        let mut readers = ROSTER_READERS.lock().unwrap_or_else(|p| p.into_inner());
        readers.written.insert(self.key, (self.store, seq));
        drop(readers);
        ROSTER_WRITTEN.notify_all();
    }
}

impl Drop for RosterReader {
    fn drop(&mut self) {
        let mut readers = ROSTER_READERS.lock().unwrap_or_else(|p| p.into_inner());
        readers.written.remove(&self.key);
        drop(readers);
        ROSTER_WRITTEN.notify_all();
    }
}

/// Wait up to `within` until every live `sessions` stream of `store` has written its roster row `seq` (and everything before it) to its client, or has ended; `true` when all had.
#[cfg(any(unix, test))]
pub(crate) fn await_roster_written(store: &Store, seq: u64, within: Duration) -> bool {
    let store = store_key(store);
    let behind = |r: &RosterReaders| {
        r.written
            .values()
            .any(|(of, written)| *of == store && *written < seq)
    };
    let readers = ROSTER_READERS.lock().unwrap_or_else(|p| p.into_inner());
    let (readers, _) = ROSTER_WRITTEN
        .wait_timeout_while(readers, within, |r| behind(r))
        .unwrap_or_else(|p| p.into_inner());
    !behind(&readers)
}

const LIVENESS: Duration = Duration::from_millis(250);

/// The least time between two content reads (`screen`, `cursor`, `cells`) that wakes drive on one watch.
pub(crate) const CONTENT_PACE: Duration = Duration::from_millis(16);

/// The least time between two `events` reads that the PTY reader's wakes drive on one watch.
pub(crate) const EVENTS_PACE: Duration = CONTENT_PACE;

/// One watch's paced reads: its content streams' ([`CONTENT_PACE`]) and its `events` digest's ([`EVENTS_PACE`]).
#[derive(Default)]
pub(crate) struct Paces {
    content: ReadPace,
    events: ReadPace,
}

impl Paces {
    fn due(&self) -> Option<Instant> {
        let content = self.content.due(CONTENT_PACE);
        let events = self.events.due(EVENTS_PACE);
        content.into_iter().chain(events).min()
    }
}

/// One paced read of a watch: when a wake last drove it, and whether one is owed ([`CONTENT_PACE`], [`EVENTS_PACE`]).
#[derive(Default)]
pub(crate) struct ReadPace {
    read_at: Option<Instant>,
    owed: bool,
}

impl ReadPace {
    /// Whether the pass at `now` makes the read: `Some(true)` for a read a wake drove (a genuine wake to `every-frame`), `Some(false)` for a liveness pass with no wake owed, `None` while a wake's read is paced.
    pub(crate) fn visit(&mut self, now: Instant, woke: bool, pace: Duration) -> Option<bool> {
        self.owed |= woke;
        if !self.owed {
            return Some(false);
        }
        if self.read_at.is_some_and(|at| now < at + pace) {
            return None;
        }
        self.owed = false;
        self.read_at = Some(now);
        Some(true)
    }

    /// The read at `now` that a record drove ([`EVENTS_PACE`]): made whatever the window, and covering any read owed, since it reads the latest state.
    pub(crate) fn read_now(&mut self, now: Instant) {
        self.owed = false;
        self.read_at = Some(now);
    }

    /// When the owed read comes due, if one is owed.
    pub(crate) fn due(&self, pace: Duration) -> Option<Instant> {
        self.read_at.filter(|_| self.owed).map(|at| at + pace)
    }
}

fn next_wait(watches: &[Watch], now: Instant, timed: bool) -> Option<Duration> {
    let owed = watches
        .iter()
        .filter_map(|w| w.pace.due())
        .min()
        .map(|due| due.saturating_duration_since(now));
    owed.into_iter().chain(timed.then_some(LIVENESS)).min()
}

/// How a push loop learns that its client hung up.
pub(crate) trait PeerLiveness {
    /// Whether the client is gone.
    fn gone(&mut self) -> bool;

    /// The list rung when the client goes, while that ring is how the loop learns it: the loop watches it with its sessions' lists and keeps no timer for its peer.
    fn rings(&self) -> Option<&Arc<Watchers>>;
}

impl<F: FnMut() -> bool> PeerLiveness for F {
    fn gone(&mut self) -> bool {
        self()
    }

    fn rings(&self) -> Option<&Arc<Watchers>> {
        None
    }
}

fn membership_owed(cursors: &PushCursors) -> bool {
    cursors
        .instance_dirty
        .as_ref()
        .is_some_and(|dirty| dirty.load(Ordering::Acquire))
}

fn timeline_owed(watches: &[Watch]) -> bool {
    watches.iter().any(|w| w.timeline_held)
}

/// The production push loop, ended by `liveness` ([`PeerLiveness`]) when its client hangs up.
pub(crate) fn push_loop_with_peer_probe<W: Write, L: PeerLiveness>(
    store: &Store,
    targets: &[ResolvedTarget],
    scopes: PushScopes,
    opts: PushOptions,
    handshake: &str,
    writer: &mut W,
    mut liveness: L,
) {
    let PushScopes {
        streams, instance, ..
    } = scopes;

    let (roster, reader) = instance
        .sessions()
        .then(|| {
            let g = store.read().unwrap_or_else(|p| p.into_inner());
            let seq = g.roster_seq();
            (
                RosterCursor {
                    seq,
                    known: g.live_sids(),
                },
                RosterReader::join(store, seq),
            )
        })
        .unzip();

    let mut watches: Vec<Watch> = targets
        .iter()
        .map(|t| new_watch(t, streams, &opts))
        .collect();

    let mut sub = Subscription::new();
    for w in &mut watches {
        w.dirty = watch_session(&mut sub, &w.src.watchers, streams);
    }
    let instance_dirty = (roster.is_some() || scopes.adopt.on()).then(|| {
        sub.watch(
            &store
                .read()
                .unwrap_or_else(|p| p.into_inner())
                .instance_watchers(),
        )
    });
    if let Some(peer) = liveness.rings() {
        let _ = sub.watch(peer);
    }

    let handshake = with_log_anchors(handshake, &watches);
    if !handshake.is_empty()
        && (writer.write_all(handshake.as_bytes()).is_err() || writer.flush().is_err())
    {
        return;
    }

    let mut egress = Egress::new(writer, opts.timestamps);
    let mut cursors = PushCursors {
        roster,
        adopt_seq: 0,
        pruned_at: None,
        instance_dirty,
        reader,
    };
    let _ = pump(
        store,
        &mut sub,
        &mut watches,
        scopes,
        &opts,
        &mut cursors,
        &mut egress,
        &mut liveness,
    );
}

#[allow(
    clippy::too_many_arguments,
    reason = "the internal pump borrows each independently-owned subscription cursor and the peer's liveness"
)]
fn pump<W: Write, L: PeerLiveness>(
    store: &Store,
    sub: &mut Subscription,
    watches: &mut Vec<Watch>,
    scopes: PushScopes,
    opts: &PushOptions,
    cursors: &mut PushCursors,
    egress: &mut Egress<'_, W>,
    liveness: &mut L,
) -> Result<(), Gone> {
    let PushScopes { streams, adopt, .. } = scopes;
    let since_turn = opts.since_turn;
    egress.begin_wake();

    if streams.events
        && let Some(anchor) = since_turn
    {
        let mut gaps = Vec::new();
        for w in watches.iter() {
            let low = w
                .src
                .turns
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .low_id();
            if let Some(low) = low
                && anchor + 1 < low
            {
                let tag = Tag::Channel(w.local_id);
                gaps.push(frame_gap_events(tag, crate::metrics::now_us(), low));
            }
        }
        egress.send(gaps)?;
    }

    visit_watches(watches, streams, egress)?;

    loop {
        let timed =
            liveness.rings().is_none() || membership_owed(cursors) || timeline_owed(watches);
        let _ = sub.wait(next_wait(watches, Instant::now(), timed));
        if liveness.gone() {
            return Err(Gone);
        }
        egress.begin_wake();

        for closing in prune_closed(store, watches, cursors) {
            sub.unwatch(closing.watchers());
            egress.send(closing.drain(streams))?;
        }

        egress.send(drain_membership(
            store, watches, sub, streams, opts, adopt, cursors,
        ))?;
        if let (Some(reader), Some(cursor)) = (cursors.reader.as_mut(), cursors.roster.as_ref()) {
            reader.wrote(cursor.seq);
        }

        if watches.is_empty() && !adopt.on() {
            return Ok(());
        }

        visit_watches(watches, streams, egress)?;
    }
}

fn watch_session(
    sub: &mut Subscription,
    watchers: &Arc<Watchers>,
    streams: TargetStreams,
) -> Arc<AtomicBool> {
    if streams.wants_content() {
        sub.watch_frame(watchers)
    } else {
        sub.watch(watchers)
    }
}

fn visit_watches<W: Write>(
    watches: &mut [Watch],
    streams: TargetStreams,
    egress: &mut Egress<'_, W>,
) -> Result<(), Gone> {
    let now = Instant::now();
    for w in watches.iter_mut() {
        let dirty = w.dirty.swap(false, Ordering::AcqRel) || w.timeline_held;
        let records = w.src.watchers.records_rung();
        let recorded = records != w.records_read;
        let Some(visit) = pass_streams(streams, &mut w.pace, dirty, recorded, now) else {
            continue;
        };
        if visit.events {
            w.records_read = records;
        }
        let mut frames = frames_for_watch(w, visit, true);
        if dirty && streams.bytes {
            frames.extend(drain_bytes_frames(w));
        }
        egress.send(frames)?;
    }
    Ok(())
}

fn pass_streams(
    streams: TargetStreams,
    pace: &mut Paces,
    dirty: bool,
    recorded: bool,
    now: Instant,
) -> Option<TargetStreams> {
    let content =
        streams.wants_content() && pace.content.visit(now, dirty, CONTENT_PACE) == Some(true);
    let events = streams.events
        && if recorded {
            pace.events.read_now(now);
            true
        } else {
            let read = pace.events.visit(now, dirty, EVENTS_PACE) == Some(true);
            #[cfg(test)]
            if read && !dirty {
                crate::work_counts::owed_events_read();
            }
            read
        };
    let drained = dirty && (streams.bytes || streams.mail || streams.log);
    if !(content || events || drained) {
        return None;
    }
    Some(TargetStreams {
        screen: content && streams.screen,
        cursor: content && streams.cursor,
        cells: content && streams.cells,
        events,
        bytes: dirty && streams.bytes,
        mail: dirty && streams.mail,
        log: dirty && streams.log,
        ..streams
    })
}

#[cfg(debug_assertions)]
fn push_held(step: &str) -> bool {
    #[cfg(test)]
    if HELD_HERE.with(|held| held.get().contains(&step)) {
        return true;
    }
    static DIR: std::sync::OnceLock<Option<std::path::PathBuf>> = std::sync::OnceLock::new();
    DIR.get_or_init(|| aterm_types::dev_seam!("ATERM_TEST_PUSH_HOLD").map(std::path::PathBuf::from))
        .as_ref()
        .is_some_and(|dir| dir.join(step).exists())
}

#[cfg(not(debug_assertions))]
const fn push_held(_step: &str) -> bool {
    false
}

fn timeline_held(_under: &crate::session_timeline::SessionTimeline) -> bool {
    push_held("timeline")
}

#[cfg(test)]
thread_local! {
    static HELD_HERE: std::cell::Cell<&'static [&'static str]> = const { std::cell::Cell::new(&[]) };
}

fn resume_block_watermark(anchor: Option<u64>, live: Option<u64>) -> Option<u64> {
    match (anchor, live) {
        (Some(anchor), Some(live)) => Some(anchor.min(live)),
        (Some(_), None) => None,
        (None, live) => live,
    }
}

fn initial_turn_watermark(turns: &Arc<Mutex<TurnLedger>>, streams: TargetStreams) -> Option<u64> {
    if !streams.events {
        return None;
    }
    turns.lock().unwrap_or_else(|p| p.into_inner()).high_id()
}

fn initial_timeline_watermarks(
    timeline: &Arc<Mutex<crate::session_timeline::SessionTimeline>>,
    streams: TargetStreams,
) -> (Option<u64>, u64) {
    if !streams.events {
        return (None, 0);
    }
    let tl = timeline.lock().unwrap_or_else(|p| p.into_inner());
    (tl.high_id(), tl.resizes().high_row())
}

fn pick_adoptions(
    g: &crate::session_store::SessionStore,
    watches: &[Watch],
    adopt_seq: &mut u64,
) -> Vec<(ResolvedTarget, String)> {
    let room = crate::control::MAX_SUBSCRIBE_TARGETS.saturating_sub(watches.len());
    if room == 0 {
        return Vec::new();
    }
    let high = g.roster_seq();
    if high == *adopt_seq {
        // The idle tick: nothing entered or left the registry.
        return Vec::new();
    }
    let unwatched = |h: &&crate::session_store::SessionHandle| {
        !watches.iter().any(|w| w.local_id == h.local_id)
    };
    let carve = |h: &crate::session_store::SessionHandle| {
        (
            ResolvedTarget {
                local_id: h.local_id,
                term: h.term.clone(),
                src: WatchSource::of(&h.ctx),
            },
            h.sid.as_str().to_string(),
        )
    };
    let mut picked: Vec<(ResolvedTarget, String)> = match g.roster_low_seq() {
        Some(low) if adopt_seq.saturating_add(1) >= low => g
            .roster_since(*adopt_seq)
            .filter(|r| r.change == crate::session_store::RosterChange::Created)
            .filter_map(|r| g.by_sid(&aterm_session::SessionId::new(&r.sid)))
            .filter(|h| unwatched(h))
            .map(carve)
            .collect(),
        _ => g
            .live_handles()
            .filter(|h| unwatched(h))
            .map(carve)
            .collect(),
    };
    if picked.len() <= room {
        *adopt_seq = high;
    } else {
        picked.truncate(room);
    }
    picked
}

fn drain_membership(
    store: &Store,
    watches: &mut Vec<Watch>,
    sub: &mut Subscription,
    streams: TargetStreams,
    opts: &PushOptions,
    adopt: AdoptScope,
    cursors: &mut PushCursors,
) -> Vec<Frame> {
    if !adopt.on() && cursors.roster.is_none() {
        return Vec::new();
    }
    if let Some(dirty) = &cursors.instance_dirty
        && !dirty.swap(false, Ordering::AcqRel)
    {
        return Vec::new();
    }
    let (adopt_held, roster_held) = (push_held("adopt"), push_held("sessions"));
    let held = (adopt.on() && adopt_held) || (cursors.roster.is_some() && roster_held);
    if held && let Some(dirty) = &cursors.instance_dirty {
        dirty.store(true, Ordering::Release);
    }
    let (t_us, picked, roster) = {
        let g = store.read().unwrap_or_else(|p| p.into_inner());
        let t_us = crate::metrics::now_us();
        let picked = if adopt.on() && !adopt_held {
            pick_adoptions(&g, watches, &mut cursors.adopt_seq)
        } else {
            Vec::new()
        };
        let picked: Vec<(ResolvedTarget, String, bool)> = picked
            .into_iter()
            .map(|(target, sid)| {
                let fresh = opts.adopt_after.is_some_and(|after| {
                    g.roster_since(after).any(|r| {
                        r.change == crate::session_store::RosterChange::Created && r.sid == sid
                    })
                });
                (target, sid, fresh)
            })
            .collect();
        let mut roster = String::new();
        if let Some(cursor) = cursors.roster.as_mut()
            && !roster_held
        {
            let watched = |local: u64| {
                watches.iter().any(|w| w.local_id == local)
                    || picked.iter().any(|(t, _, _)| t.local_id == local)
            };
            let watched: Option<&dyn Fn(u64) -> bool> = adopt.on().then_some(&watched);
            drain_roster(&g, cursor, watched, &mut roster);
        }
        (t_us, picked, roster)
    };
    let mut out = Vec::with_capacity(picked.len() + 1);
    for (target, sid, fresh) in picked {
        let mut watch = new_watch_at(&target, streams, opts, fresh);
        watch.dirty = watch_session(sub, &target.src.watchers, streams);
        let anchor = log_anchor(&watch);
        watches.push(watch);
        out.push(Frame::text(
            Tag::Instance,
            t_us,
            format!("sub {} {sid}{anchor}\n", target.local_id),
        ));
    }
    if !roster.is_empty() {
        out.push(Frame::text(Tag::Instance, t_us, roster));
    }
    out
}

#[must_use = "a closing watch still holds an undelivered tail — call drain()"]
struct Closing(Watch);

impl Closing {
    fn watchers(&self) -> &Arc<Watchers> {
        &self.0.src.watchers
    }

    fn drain(mut self, streams: TargetStreams) -> Vec<Frame> {
        let tag = Tag::Channel(self.0.local_id);
        self.0.closing = true;
        let mut out = frames_for_watch(&mut self.0, streams, false);
        out.extend(drain_bytes_frames(&self.0));
        if streams.events {
            let exited = frame_exited(&sid_tag(self.0.local_id));
            out.push(Frame::text(tag, crate::metrics::now_us(), exited));
        }
        out
    }
}

#[must_use]
fn prune_closed(
    store: &Store,
    watches: &mut Vec<Watch>,
    cursors: &mut PushCursors,
) -> Vec<Closing> {
    let g = store.read().unwrap_or_else(|p| p.into_inner());
    let high = g.roster_seq();
    let since = cursors.pruned_at.replace(high);
    let suspect = match since {
        Some(seen) if seen == high => return Vec::new(),
        Some(seen)
            if g.roster_low_seq()
                .is_some_and(|low| seen.saturating_add(1) >= low) =>
        {
            g.roster_since(seen).any(|r| {
                r.change == crate::session_store::RosterChange::Exited
                    && watches.iter().any(|w| w.local_id == r.local_id)
            })
        }
        _ => true,
    };
    if !suspect {
        return Vec::new();
    }
    let (dead, live): (Vec<Watch>, Vec<Watch>) =
        std::mem::take(watches).into_iter().partition(|w| {
            #[cfg(test)]
            crate::work_counts::watch_probed();
            g.by_local(w.local_id)
                .is_none_or(|h| !Arc::ptr_eq(&h.ctx.watchers, &w.src.watchers))
        });
    *watches = live;
    if !dead.is_empty()
        && let Some(dirty) = &cursors.instance_dirty
    {
        dirty.store(true, Ordering::Release);
    }
    dead.into_iter().map(Closing).collect()
}

/// Test fixture for the `events` digest: K watched targets whose ledgers are filled to a requested depth, woken through the shipping [`frames_for_watch`].
#[cfg(test)]
pub(crate) mod digest_fixture {
    use super::{
        PushOptions, ResolvedTarget, TargetStreams, Watch, WatchSource, frames_for_watch, new_watch,
    };
    use crate::session_timeline::SessionTimeline;
    use crate::turn_ledger::{TurnLedger, TurnRecord};
    use aterm_core::terminal::Terminal;
    use std::sync::{Arc, Mutex};

    const CYCLE: &[u8] =
        b"\x1b]133;A\x07$ \x1b]133;B\x07cmd\r\n\x1b]133;C\x07out\r\n\x1b]133;D;0\x07";

    pub(crate) struct DigestFixture {
        watches: Vec<Watch>,
        streams: TargetStreams,
        terms: Vec<Arc<Mutex<Terminal>>>,
        turns: Vec<Arc<Mutex<TurnLedger>>>,
        timelines: Vec<Arc<Mutex<SessionTimeline>>>,
        next_turn_id: u64,
    }

    impl DigestFixture {
        pub(crate) fn build(targets: usize, blocks: usize, turns: usize, timeline: usize) -> Self {
            let mut f = DigestFixture {
                watches: Vec::with_capacity(targets),
                streams: TargetStreams {
                    events: true,
                    ..Default::default()
                },
                terms: Vec::with_capacity(targets),
                turns: Vec::with_capacity(targets),
                timelines: Vec::with_capacity(targets),
                next_turn_id: 0,
            };
            for i in 0..targets {
                let fixture_term = Arc::new(Mutex::new(Terminal::new(24, 80)));
                {
                    let mut t = fixture_term.lock().expect("fresh engine");
                    for _ in 0..blocks {
                        t.process(CYCLE);
                    }
                    t.process(b"\x1b]2;fixture\x07");
                }
                let fixture_ctx =
                    crate::session_store::test_handle_with(i as u64, fixture_term.clone()).ctx;
                let ledger = fixture_ctx.turns.clone();
                {
                    let mut l = ledger.lock().expect("fresh ledger");
                    for _ in 0..turns {
                        f.next_turn_id += 1;
                        l.push(turn_record(f.next_turn_id));
                    }
                }
                let tl = fixture_ctx.timeline.clone();
                {
                    let mut t = tl.lock().expect("fresh timeline");
                    for j in 0..timeline {
                        // would never reach the emit path.
                        if j % 8 == 0 {
                            t.record_raw_meta_change("field=role value=lead".to_string());
                        } else {
                            t.record("state-change", "state=alive".to_string());
                        }
                    }
                }
                let target = ResolvedTarget {
                    local_id: i as u64,
                    term: fixture_term.clone(),
                    src: WatchSource::of(&fixture_ctx),
                };
                f.watches
                    .push(new_watch(&target, f.streams, &PushOptions::default()));
                f.terms.push(fixture_term);
                f.turns.push(ledger);
                f.timelines.push(tl);
            }
            f
        }

        pub(crate) fn wake(&mut self, woke: bool) -> usize {
            let mut bytes = 0usize;
            for w in &mut self.watches {
                for f in frames_for_watch(w, self.streams, woke) {
                    bytes += f.segs.iter().map(|seg| seg.bytes().len()).sum::<usize>();
                }
            }
            bytes
        }

        pub(crate) fn retained(&self) -> (usize, usize, usize) {
            let fixture_term = &self.terms[0];
            let fixture_turns = &self.turns[0];
            let fixture_timeline = &self.timelines[0];
            (
                fixture_term
                    .lock()
                    .expect("fixture engine")
                    .all_blocks()
                    .count(),
                fixture_turns.lock().expect("fixture ledger").len(),
                fixture_timeline.lock().expect("fixture timeline").len(),
            )
        }

        pub(crate) fn land_turn(&mut self) {
            let mut id = self.next_turn_id;
            for fixture_ledger in &self.turns {
                id += 1;
                fixture_ledger
                    .lock()
                    .expect("fixture ledger")
                    .push(turn_record(id));
            }
            self.next_turn_id = id;
        }
    }

    pub(crate) fn turn_record(id: u64) -> TurnRecord {
        TurnRecord {
            id,
            started_ms: id,
            dur_ms: 12,
            submitted: true,
            status: "settled",
            text: "run the suite".into(),
            screen_hash: id.wrapping_mul(0x9E37_79B9_7F4A_7C15),
            seq: id,
            arch: crate::turn_ledger::ArchMark::default(),
            by: crate::turn_ledger::TurnBy::Ctl,
            carried: false,
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn a_resumed_block_anchor_never_rises_above_the_engine() {
        use super::resume_block_watermark as seed;
        assert_eq!(seed(Some(12), Some(13)), Some(12), "block 13 is replayed");
        assert_eq!(
            seed(Some(40), Some(13)),
            Some(13),
            "a foreign anchor is the live high"
        );
        assert_eq!(
            seed(Some(40), None),
            None,
            "nothing completed: every block pushes"
        );
        assert_eq!(seed(None, Some(13)), Some(13), "no anchor: the live high");
        assert_eq!(seed(None, None), None);
    }

    use super::*;
    use std::time::Duration;

    #[test]
    fn only_owner_is_granted_the_instance_sessions_stream() {
        let edge = || Scope::Edge(aterm_session::EdgeToken::from_bytes([9u8; 32]));
        assert!(AdoptScope::authorize(true, Scope::Owner).on());
        assert!(
            !AdoptScope::authorize(true, edge()).on(),
            "an EDGE asking for @*"
        );
        assert!(
            !AdoptScope::authorize(false, Scope::Owner).on(),
            "not asked for"
        );
        assert!(!AdoptScope::none().on(), "the refusing default refuses");
        let asked = RequestedInstance { sessions: true };
        assert!(InstanceStreams::authorize(asked, Scope::Owner).sessions());
        assert!(
            !InstanceStreams::authorize(asked, edge()).sessions(),
            "an EDGE asking for the roster"
        );
        assert!(
            !InstanceStreams::authorize(RequestedInstance::default(), Scope::Owner).sessions(),
            "authorize grants, it does not add"
        );
    }

    #[test]
    fn adoption_picks_up_a_session_created_after_subscribe() {
        let store = crate::session_store::new_store();
        let mut sub = Subscription::new();
        let mut watches: Vec<Watch> = Vec::new();
        let mut cursors = PushCursors {
            roster: None,
            adopt_seq: 0,
            pruned_at: None,
            instance_dirty: None,
            reader: None,
        };
        let streams = TargetStreams {
            events: true,
            ..Default::default()
        };
        let opts = PushOptions::default();
        let all = AdoptScope::authorize(true, Scope::Owner);
        let adopt = |w: &mut Vec<Watch>, sub: &mut Subscription, c: &mut PushCursors| {
            text(&drain_membership(&store, w, sub, streams, &opts, all, c))
        };

        assert!(
            adopt(&mut watches, &mut sub, &mut cursors).is_empty(),
            "an instance with no sessions adopts nothing"
        );

        let h = crate::session_store::test_handle(5);
        let sid = h.sid.as_str().to_string();
        let watchers = h.ctx.watchers.clone();
        store.write().unwrap_or_else(|p| p.into_inner()).register(h);

        assert_eq!(
            adopt(&mut watches, &mut sub, &mut cursors),
            format!("sub 5 {sid}\n"),
            "the adopted channel is acked exactly like a handshake channel"
        );
        assert_eq!(watches.len(), 1, "and it joined the watch set");
        assert_eq!(
            watchers.count(),
            1,
            "and it is registered for producer wakes"
        );
        watchers.ring_watchers();
        assert!(sub.wait(Duration::ZERO), "which reach the subscription");

        assert!(
            adopt(&mut watches, &mut sub, &mut cursors).is_empty(),
            "a second pass adopts nothing — the watermark caught up"
        );
        assert!(
            frames_for_watch(&mut watches[0], streams, true).is_empty(),
            "an adopted watch is seeded at the LIVE highs, so it replays no backlog"
        );
    }

    #[test]
    fn an_adopted_new_sessions_log_stream_starts_with_its_opening() {
        let store = crate::session_store::new_store();
        let mut sub = Subscription::new();
        let mut watches: Vec<Watch> = Vec::new();
        let mut cursors = PushCursors {
            roster: None,
            adopt_seq: 0,
            pruned_at: None,
            instance_dirty: None,
            reader: None,
        };
        let streams = TargetStreams {
            log: true,
            ..Default::default()
        };
        let before = crate::session_store::test_handle(4);
        let old_sid = before.sid.as_str().to_string();
        store
            .write()
            .unwrap_or_else(|p| p.into_inner())
            .register(before);
        let resolved_at = store.read().unwrap_or_else(|p| p.into_inner()).roster_seq();
        let opts = PushOptions {
            adopt_after: Some(resolved_at),
            ..PushOptions::default()
        };
        let h = crate::session_store::test_handle(5);
        let sid = h.sid.as_str().to_string();
        store.write().unwrap_or_else(|p| p.into_inner()).register(h);
        let all = AdoptScope::authorize(true, Scope::Owner);
        let acks = text(&drain_membership(
            &store,
            &mut watches,
            &mut sub,
            streams,
            &opts,
            all,
            &mut cursors,
        ));
        assert!(
            acks.contains(&format!("sub 5 {sid} log_seq=0 log_ep=1\n")),
            "{acks}"
        );
        assert!(
            acks.contains(&format!("sub 4 {old_sid} log_seq=1 log_ep=1\n")),
            "{acks}"
        );
        let mut take = |local: u64| {
            let w = watches
                .iter_mut()
                .find(|w| w.local_id == local)
                .expect("adopted");
            text(&frames_for_watch(w, streams, true))
        };
        let new = take(5);
        assert!(
            new.lines()
                .next()
                .is_some_and(|l| l.starts_with("LOG 5 1 open - spawn ")),
            "the new session's opening comes first: {new:?}"
        );
        assert!(!new.contains("GAP"), "{new:?}");
        assert_eq!(
            take(4),
            "",
            "NEGATIVE CONTROL: a session from before replays nothing"
        );
    }

    fn watch_on(local_id: u64, term: &Arc<Mutex<Terminal>>) -> Watch {
        Watch {
            local_id,
            last_mail_id: 0,
            mail: crate::fabric::MailFilter::default(),
            log: None,
            closing: false,
            timeline_held: false,
            term: term.clone(),
            src: WatchSource::of(&crate::session_store::test_handle(local_id).ctx),
            last_sent_seq: 0,
            last_alt: false,
            last_cursor: (u16::MAX, u16::MAX, false, ""),
            last_text: TextKey::default(),
            last_rows_hash: 0,
            last_cells_etag: 0,
            last_cells_body: None,
            _cells_hold: None,
            last_block_id: None,
            last_timeline_id: None,
            last_resize_id: 0,
            last_turn_id: None,
            last_title: None,
            last_bell: 0,
            byte_sub: None,
            non_coalesced: false,
            pace: Paces::default(),
            records_read: 0,
            dirty: Arc::default(),
        }
    }

    fn ledgered_resize(w: &Watch, rows: u16, at_ms: u64) {
        let copy = {
            let mut t = crate::term_lock(&w.term);
            t.resize(rows, 20);
            crate::resize_ledger::JournalCopy::take(&t)
        };
        w.src.timeline.lock().unwrap().ingest_resizes(
            &copy,
            Some(crate::resize_ledger::Attribution::CTL_CROSS),
            at_ms,
        );
    }

    #[test]
    fn events_digest_coalesces_resizes_and_pushes_render_once() {
        let term = Arc::new(Mutex::new(Terminal::new(8, 20)));
        crate::term_lock(&term).process(
            b"\x1b[?1049h\x1b[1;1HL0\x1b[2;1HL1\x1b[3;1HL2\x1b[4;1HL3\x1b[5;1HL4\
              \x1b[6;1HL5\x1b[7;1HL6\x1b[8;1HF0\x1b[6;3H",
        );
        let mut w = watch_on(7, &term);
        w.last_title = Some(crate::term_lock(&term).title().to_string());
        let streams = TargetStreams {
            events: true,
            ..Default::default()
        };
        assert_eq!(wake_text(&mut w, streams, false), "", "nothing yet");

        let far = crate::turn_ledger::now_ms() + 1_000_000_000;
        ledgered_resize(&w, 7, far);
        crate::term_lock(&term).process(b"\x1b[?1000h");
        ledgered_resize(&w, 8, far);
        crate::term_lock(&term).process(b"\x1b[8;1HF1\x1b[K\x1b[6;3H");
        let first = wake_text(&mut w, streams, true);
        assert_eq!(first, "EVENT 7 resize n=2 id=2 to=20x8 run=1\n");
        assert_eq!(
            wake_text(&mut w, streams, false),
            "",
            "coalesced, not repeated"
        );

        let probe = crate::resize_ledger::EngineProbe::sample(&crate::term_lock(&term));
        assert!(
            w.src
                .timeline
                .lock()
                .unwrap()
                .evaluate_resizes(probe, far + 1_000)
        );
        let risk = wake_text(&mut w, streams, false);
        assert_eq!(
            risk,
            format!("EVENT 7 render desync-risk run=1 displaced=-1 net=zero at={far}\n")
        );
        assert!(
            !w.src
                .timeline
                .lock()
                .unwrap()
                .evaluate_resizes(probe, far + 2_000)
        );
        assert_eq!(wake_text(&mut w, streams, false), "", "pushed once");

        crate::term_lock(&term).process(b"\x1b[2J");
        let probe = crate::resize_ledger::EngineProbe::sample(&crate::term_lock(&term));
        assert!(
            w.src
                .timeline
                .lock()
                .unwrap()
                .evaluate_resizes(probe, far + 2_500)
        );
        assert_eq!(
            wake_text(&mut w, streams, false),
            "EVENT 7 render healed run=1 after_ms=2500\n"
        );

        for i in 0..70u64 {
            ledgered_resize(&w, 7, far + 3_000 + 1_000 * i);
            ledgered_resize(&w, 8, far + 3_001 + 1_000 * i);
        }
        let behind = wake_text(&mut w, streams, false);
        assert_eq!(
            behind, "EVENT 7 resize n=128 id=142 to=20x8 run=71 dropped=12\n",
            "142 booked, rows 15..=142 kept, the watch had seen 2: 3..=14 are gone"
        );
        assert!(!behind.contains("GAP"), "no GAP frame: {behind}");
    }

    fn frame_bytes(frames: &[Frame]) -> Vec<u8> {
        frames
            .iter()
            .flat_map(|f| &f.segs)
            .flat_map(|seg| seg.bytes().iter().copied())
            .collect()
    }

    fn text(frames: &[Frame]) -> String {
        String::from_utf8(frame_bytes(frames)).expect("text frames are UTF-8")
    }

    fn wake_text(w: &mut Watch, streams: TargetStreams, woke: bool) -> String {
        text(&frames_for_watch(w, streams, woke))
    }

    fn wire(timestamps: bool, frames: Vec<Frame>) -> Vec<u8> {
        let mut sink: Vec<u8> = Vec::new();
        let mut egress = Egress::new(&mut sink, timestamps);
        egress.begin_wake();
        egress.send(frames).expect("a Vec sink never hangs up");
        sink
    }

    fn targets_of(screen: bool, cursor: bool, events: bool) -> TargetStreams {
        TargetStreams {
            screen,
            cursor,
            events,
            ..Default::default()
        }
    }

    #[test]
    fn the_mail_streams_filter_parses_and_fails_closed() {
        let mail = |kinds: &[&str], from: Option<&str>| crate::fabric::MailFilter {
            kinds: kinds.iter().map(|k| (*k).to_string()).collect(),
            from: from.map(str::to_string),
            topic: None,
        };
        let topic = |t: &str| crate::fabric::MailFilter {
            topic: Some(t.to_string()),
            ..crate::fabric::MailFilter::default()
        };
        let got = |s: &str| Requested::parse(s).expect(s);

        assert!(got("mail").targets.mail);
        assert_eq!(got("mail").mail, mail(&[], None));
        assert_eq!(
            got("mail:kinds=task,ask").mail,
            mail(&["task", "ask"], None)
        );
        assert_eq!(got("mail:from=h-andrew").mail, mail(&[], Some("h-andrew")));
        assert_eq!(got("mail:from=human").mail, mail(&[], Some("human")));
        assert_eq!(
            got("mail:kinds=task,ask:from=agent").mail,
            mail(&["task", "ask"], Some("agent"))
        );

        let both = got("screen,mail:kinds=task,ask,cursor");
        assert_eq!(both.mail, mail(&["task", "ask"], None));
        assert!(both.targets.screen && both.targets.cursor && both.targets.mail);

        assert_eq!(got("mail:topic=build.failed").mail, topic("build.failed"));
        assert_eq!(
            got("mail:topic=build.failed:kinds=note").mail,
            crate::fabric::MailFilter {
                kinds: vec!["note".to_string()],
                from: None,
                topic: Some("build.failed".to_string()),
            }
        );

        assert!(Requested::parse("mail").is_some());

        for bad in [
            "mail:kinds=nope",
            "mail:kinds=",
            "mail:from=",
            "mail:bogus=1",
            "mail:kinds=task:kinds=ask",
            "mail:from=a:from=b",
            "mail:topic=",
            "mail:topic=Build",
            "mail:topic=a/b",
            "mail:topic=a:topic=b",
            "mailish",
            "mail:from=h-andrew,h-bob",
            "mail:from=human,agent",
            "mail:from=andrew",
            "mail:from=h-",
            "mail:from=h-a b",
        ] {
            assert_eq!(Requested::parse(bad), None, "{bad} must fail closed");
        }
        for good in ["service", "other", "s-w1@n-lab", "a-svc"] {
            assert_eq!(
                got(&format!("mail:from={good}")).mail,
                mail(&[], Some(good)),
                "{good}"
            );
        }
    }

    #[test]
    fn the_gap_frame_names_what_the_ring_dropped() {
        let store = crate::session_store::new_store();
        let h = crate::session_store::test_handle(7);
        store
            .write()
            .unwrap_or_else(|p| p.into_inner())
            .register(h.clone());
        let deliver = |off: u64, from: &str| {
            let reply = crate::fabric::cmd_deliver(
                &store,
                &format!(
                    "{} off={off} from={from} kind=note trust=agent text=x",
                    h.sid.as_str()
                ),
            );
            assert!(reply.starts_with("OK"), "{reply}");
        };
        let over = crate::fabric::RING_CAP + 40;
        for n in 1..=over {
            deliver(1000 + n as u64, &format!("s-p{n}"));
        }
        let mut w = new_watch(
            &target_of(&h),
            TargetStreams {
                mail: true,
                ..TargetStreams::default()
            },
            &PushOptions::default(),
        );
        w.last_mail_id = 0;
        let mut out = String::new();
        drain_mail_events(&mut w, "7", &mut out);
        let first = out.lines().next().expect("a frame");
        assert_eq!(
            first,
            &format!("GAP 7 mail-dropped={}", over - crate::fabric::RING_CAP),
            "the literal gap frame: {out}"
        );
        assert!(
            out.lines()
                .nth(1)
                .is_some_and(|l| l.starts_with("MAIL 7 id=")),
            "the MAIL lines follow it: {out}"
        );
    }

    #[test]
    fn streams_parse_subset_and_fail_closed() {
        let req = |targets: TargetStreams, timestamps: bool| {
            Some(Requested {
                targets,
                timestamps,
                ..Default::default()
            })
        };
        let trimmed = TargetStreams {
            screen: true,
            trim: true,
            ..Default::default()
        };
        let cells_bytes = |screen| TargetStreams {
            cells: true,
            bytes: true,
            screen,
            ..Default::default()
        };
        for (input, want) in [
            ("screen", req(targets_of(true, false, false), false)),
            (
                "screen,cursor,events",
                req(targets_of(true, true, true), false),
            ),
            ("cursor screen", req(targets_of(true, true, false), false)),
            ("cells,bytes,screen", req(cells_bytes(true), false)),
            (
                "screen,timestamps",
                req(targets_of(true, false, false), true),
            ),
            ("cursor ts", req(targets_of(false, true, false), true)),
            ("ts,screen", req(targets_of(true, false, false), true)),
            ("screen,trim", req(trimmed, false)),
            ("trim screen ts", req(trimmed, true)),
            (
                "sessions",
                Some(Requested {
                    instance: RequestedInstance { sessions: true },
                    ..Default::default()
                }),
            ),
            ("", None),
            ("bogus", None),
            ("screen,bogus", None),
            ("trim", None),
            ("trim,ts", None),
            ("timestamps", None),
            ("ts", None),
            ("ts,timestamps", None),
        ] {
            assert_eq!(Requested::parse(input), want, "{input:?}");
        }
        for single in ["cells", "bytes"] {
            assert!(Requested::parse(single).is_some(), "{single:?}");
        }
    }

    #[test]
    fn screen_delta_on_content_change_none_on_viewport_scroll() {
        let term = Arc::new(Mutex::new(Terminal::new(24, 80)));
        let streams = TargetStreams {
            screen: true,
            ..Default::default()
        };
        let mut w = watch_on(4, &term);

        crate::term_lock(&term).process(b"hello");
        let f1 = wake_text(&mut w, streams, true);
        assert!(
            f1.starts_with("DELTA 4 seq="),
            "sid-tagged screen delta: {f1:?}"
        );
        assert!(f1.contains("hello"), "delta carries the live screen text");
        let seq_after_write = w.last_sent_seq;
        assert!(seq_after_write > 0);

        crate::term_lock(&term).scroll_display(1);
        let f2 = wake_text(&mut w, streams, true);
        assert!(f2.is_empty(), "viewport scroll produces no delta: {f2:?}");
        assert_eq!(
            w.last_sent_seq, seq_after_write,
            "seq unchanged by a scroll"
        );

        crate::term_lock(&term).process(b" world");
        let f3 = wake_text(&mut w, streams, true);
        assert!(
            f3.starts_with("DELTA 4 seq="),
            "content change re-emits: {f3:?}"
        );
        assert!(
            w.last_sent_seq > seq_after_write,
            "seq advanced on real content"
        );
    }

    #[test]
    fn trim_modifier_shortens_screen_delta_to_last_nonblank_row() {
        let term = Arc::new(Mutex::new(Terminal::new(24, 80)));
        crate::term_lock(&term).process(b"one\r\n\r\nthree");
        let full = wake_text(
            &mut watch_on(1, &term),
            TargetStreams {
                screen: true,
                ..Default::default()
            },
            true,
        );
        assert!(
            full.starts_with("DELTA 1 seq=")
                && full.lines().next().unwrap_or("").ends_with(" screen 24"),
            "untrimmed frame carries the whole grid: {full:?}"
        );
        let trimmed = wake_text(
            &mut watch_on(2, &term),
            TargetStreams {
                screen: true,
                trim: true,
                ..Default::default()
            },
            true,
        );
        let header = trimmed.lines().next().unwrap_or("");
        assert!(
            header.starts_with("DELTA 2 seq=") && header.ends_with(" screen 3"),
            "trimmed header counts the rows sent: {header:?}"
        );
        let rows: Vec<&str> = trimmed.lines().skip(1).collect();
        assert_eq!(
            rows,
            ["one", "", "three"],
            "interior blank kept, tail dropped"
        );
    }

    #[test]
    fn a_fresh_watch_starts_at_every_live_high() {
        let store = crate::session_store::new_store();
        let h = crate::session_store::test_handle(5);
        store
            .write()
            .unwrap_or_else(|p| p.into_inner())
            .register(h.clone());
        let unseeded = || Watch {
            src: WatchSource::of(&h.ctx),
            ..watch_on(5, &h.term)
        };
        ledgered_resize(
            &unseeded(),
            20,
            crate::turn_ledger::now_ms() + 1_000_000_000,
        );
        crate::term_lock(&h.term).process(
            b"\x1b]133;A\x07$ \x1b]133;B\x07cmd\r\n\x1b]133;C\x07out\r\n\x1b]133;D;0\x07\
              \x1b]2;busy\x07\x07\x1b[?1049h",
        );
        h.ctx
            .turns
            .lock()
            .unwrap()
            .push(super::digest_fixture::turn_record(1));
        h.ctx
            .timeline
            .lock()
            .unwrap()
            .record_raw_meta_change("field=role value=lead".to_string());
        let reply = crate::fabric::cmd_deliver(
            &store,
            &format!(
                "{} off=1 from=s-peer kind=note trust=agent text=x",
                h.sid.as_str()
            ),
        );
        assert!(reply.starts_with("OK"), "{reply}");
        let events = TargetStreams {
            events: true,
            mail: true,
            ..Default::default()
        };
        let screen = TargetStreams {
            screen: true,
            ..Default::default()
        };

        let history = wake_text(&mut unseeded(), events, true);
        for kind in [
            " block-complete ",
            " title ",
            " bell ",
            " turn ",
            " meta ",
            " resize ",
            "MAIL ",
        ] {
            assert!(
                history.contains(kind),
                "the unseeded watch pushes the{kind}history: {history}"
            );
        }
        assert!(
            wake_text(&mut unseeded(), screen, true).starts_with("GAP "),
            "the unseeded watch resyncs onto the alternate screen"
        );

        let seeded = |streams| new_watch(&target_of(&h), streams, &PushOptions::default());
        assert_eq!(
            wake_text(&mut seeded(events), events, true),
            "",
            "a seeded watch replays no history"
        );
        let catch_up = wake_text(&mut seeded(screen), screen, true);
        assert!(
            catch_up.starts_with("DELTA 5 "),
            "a seeded watch's catch-up is a plain DELTA: {catch_up}"
        );
    }

    #[test]
    fn since_seeds_catch_up_only_when_content_advanced() {
        let h =
            crate::session_store::test_handle_with(1, Arc::new(Mutex::new(Terminal::new(24, 80))));
        crate::term_lock(&h.term).process(b"state");
        let cur = crate::term_lock(&h.term).content_seq();
        let streams = TargetStreams {
            screen: true,
            cells: true,
            ..Default::default()
        };
        let seeded = |since| {
            let opts = PushOptions {
                since: Some(since),
                ..PushOptions::default()
            };
            new_watch(&target_of(&h), streams, &opts)
        };

        assert!(
            wake_text(&mut seeded(cur), streams, true).is_empty(),
            "no frame when caught up"
        );
        let behind = wake_text(&mut seeded(cur - 1), streams, true);
        assert!(
            behind.starts_with("DELTA 1 ")
                && behind.contains(" screen ")
                && behind.contains(" cells "),
            "catch-up delta when content advanced past since: {behind:?}"
        );
    }

    #[test]
    fn cursor_stream_emits_moves_and_visibility_on_unchanged_seq() {
        let term = Arc::new(Mutex::new(Terminal::new(24, 80)));
        crate::term_lock(&term).process(b"abc");
        let streams = TargetStreams {
            cursor: true,
            ..Default::default()
        };
        let mut w = watch_on(5, &term);
        assert!(wake_text(&mut w, streams, true).contains("cursor 0 3 1 "));
        let seq = crate::term_lock(&term).content_seq();
        for (bytes, want) in [
            (&b"\x1b[10;5H"[..], "cursor 9 4 1 "),
            (&b"\x1b[?25l"[..], "cursor 9 4 0 "),
        ] {
            crate::term_lock(&term).process(bytes);
            assert_eq!(
                crate::term_lock(&term).content_seq(),
                seq,
                "{want}: the premise: no cell write"
            );
            let f = wake_text(&mut w, streams, true);
            assert!(f.contains(want), "{want}: on an unchanged seq: {f:?}");
        }
    }

    #[test]
    fn render_only_changes_reemit_cells_on_unchanged_seq() {
        use aterm_core::selection::{SelectionSide, SelectionType};
        let term = Arc::new(Mutex::new(Terminal::new(12, 16)));
        {
            let mut t = crate::term_lock(&term);
            t.process(b"x");
            let sel = t.text_selection_mut();
            sel.start_selection(0, 0, SelectionSide::Left, SelectionType::Simple);
            sel.update_selection(0, 7, SelectionSide::Right);
            sel.complete_selection();
        }
        let streams = TargetStreams {
            cells: true,
            ..Default::default()
        };
        let mut w = watch_on(1, &term);
        assert!(wake_text(&mut w, streams, true).contains(" cells "));
        let seq = crate::term_lock(&term).content_seq();
        for (bytes, want) in [
            (&b"\x1b]11;rgb:00/00/80\x07"[..], " cells "),
            (&b"\x1b[?5h"[..], " cells "),
            (
                &b"\x1b[10;5H"[..],
                "\"cursor\":{\"row\":9,\"col\":4,\"visible\":true",
            ),
            (&b"\x1b[6 q"[..], "\"style\":\"steady_bar\""),
            (&b"\x1b]12;rgb:ab/cd/ef\x07"[..], "\"color\":\"abcdef\""),
            (
                &b"\x1b]17;rgb:12/34/56\x07"[..],
                "\"selection_bg\":\"123456\"",
            ),
            (
                &b"\x1b]19;rgb:fe/dc/ba\x07"[..],
                "\"selection_fg\":\"fedcba\"",
            ),
        ] {
            let name = String::from_utf8_lossy(bytes).escape_debug().to_string();
            crate::term_lock(&term).process(bytes);
            assert_eq!(
                crate::term_lock(&term).content_seq(),
                seq,
                "{name}: the premise: a render-only change"
            );
            let f = wake_text(&mut w, streams, true);
            assert!(
                f.contains(" cells ") && f.contains(&format!("seq={seq} ")) && f.contains(want),
                "{name}: re-emits a cells DELTA at the unchanged seq: {f:?}"
            );
            assert!(!f.contains("GAP"), "{name}: no GAP resync: {f:?}");
        }
        assert!(
            frames_for_watch(&mut w, streams, true).is_empty(),
            "an unchanged render stays coalesced"
        );
    }

    #[test]
    fn style_policy_reload_reemits_cells_on_its_wake() {
        use aterm_core::config::TerminalConfig;
        type Reload = (&'static str, fn(&mut TerminalConfig));
        let reloads: [Reload; 2] = [
            ("bold_is_bright", |c| c.bold_is_bright = !c.bold_is_bright),
            ("faint_opacity", |c| c.faint_opacity = 0.9),
        ];
        let term = Arc::new(Mutex::new(Terminal::new(4, 8)));
        let watchers = Arc::<Watchers>::default();
        watchers.bind_frame(&term);
        let mut config = TerminalConfig::default();
        crate::term_lock(&term).apply_config(&config);
        crate::term_lock(&term).process(b"\x1b[1;33mB\x1b[0m \x1b[2mD\x1b[0m");
        let streams = TargetStreams {
            cells: true,
            ..Default::default()
        };
        let mut sub = Subscription::new();
        let mut w = watch_on(1, &term);
        w.dirty = sub.watch_frame(&watchers);
        let pass = |w: &mut Watch, at: Instant| {
            let dirty = w.dirty.swap(false, Ordering::AcqRel);
            pass_streams(streams, &mut w.pace, dirty, false, at)
                .map(|visit| wake_text(w, visit, true))
                .unwrap_or_default()
        };
        let t0 = Instant::now();
        assert!(pass(&mut w, t0).contains(" cells "), "the catch-up");
        let mut at = t0;
        for (name, change) in reloads {
            let seq = crate::term_lock(&term).content_seq();
            crate::term_lock(&term).apply_config(&config);
            assert!(
                !sub.wait(Duration::ZERO),
                "{name}: the config it already has rings nobody"
            );
            at += CONTENT_PACE;
            assert_eq!(pass(&mut w, at), "", "{name}: and its pass reads nothing");
            change(&mut config);
            crate::term_lock(&term).apply_config(&config);
            assert_eq!(
                crate::term_lock(&term).content_seq(),
                seq,
                "{name}: the premise: a reload bumps no content_seq"
            );
            assert!(sub.wait(Duration::ZERO), "{name}: the reload's hold rang");
            at += CONTENT_PACE;
            let f = pass(&mut w, at);
            assert!(
                f.contains("cells ") && f.contains(&format!("seq={seq} ")),
                "{name}: the reload's ring re-emits a cells DELTA at the unchanged seq: {f:?}"
            );
        }
    }

    #[test]
    fn selection_only_change_reemits_cells_on_unchanged_seq() {
        use aterm_core::selection::{SelectionSide, SelectionType};

        let term = Arc::new(Mutex::new(Terminal::new(2, 8)));
        crate::term_lock(&term).process(b"x");
        let streams = TargetStreams {
            cells: true,
            ..Default::default()
        };
        let mut watch = watch_on(1, &term);
        assert!(
            wake_text(&mut watch, streams, true).contains(" cells "),
            "first wake establishes the cells/render watermarks"
        );
        let seq = crate::term_lock(&term).content_seq();

        {
            let mut t = crate::term_lock(&term);
            let sel = t.text_selection_mut();
            sel.start_selection(0, 6, SelectionSide::Left, SelectionType::Block);
            sel.update_selection(1, 7, SelectionSide::Right);
            sel.complete_selection();
        }
        assert_eq!(
            crate::term_lock(&term).content_seq(),
            seq,
            "selection-only mutation must remain a render-only negative control"
        );
        let selected = wake_text(&mut watch, streams, true);
        assert!(
            selected.contains(" cells ")
                && selected.contains(
                    "\"selection\":{\"start_row\":0,\"start_col\":6,\"end_row\":1,\
                     \"end_col\":7,\"kind\":\"block\",\"is_block\":true}"
                ),
            "selection-only sparse-tail change must push typed CELLS: {selected:?}"
        );
        assert!(
            frames_for_watch(&mut watch, streams, true).is_empty(),
            "unchanged selection must not defeat coalescing"
        );

        crate::term_lock(&term).text_selection_mut().clear();
        assert_eq!(crate::term_lock(&term).content_seq(), seq);
        let cleared = wake_text(&mut watch, streams, true);
        assert!(
            cleared.contains(" cells ") && cleared.contains("\"selection\":null"),
            "selection clear must remove the stale rectangle at the same seq: {cleared:?}"
        );
    }

    #[test]
    fn alt_screen_flip_forces_resync_even_at_equal_seq() {
        let term = Arc::new(Mutex::new(Terminal::new(24, 80)));
        crate::term_lock(&term).process(b"\x1b[?1049h");
        let (seq, alt) = {
            let t = crate::term_lock(&term);
            (t.content_seq(), t.is_alternate_screen())
        };
        assert!(alt, "1049h enters the alternate screen");
        let streams = TargetStreams {
            screen: true,
            ..Default::default()
        };
        let mut w = Watch {
            last_sent_seq: seq,
            ..watch_on(1, &term)
        };
        let f = wake_text(&mut w, streams, true);
        assert!(
            f.contains("GAP 1 ") && f.contains("DELTA 1 "),
            "an alt-screen flip at equal seq resyncs (GAP + full frame): {f:?}"
        );
        assert!(w.last_alt, "the alt watermark is updated after the resync");
    }

    fn gate_inputs_before_the_frame_key(t: &Terminal) -> (u64, bool, u64) {
        (
            t.content_seq(),
            t.is_alternate_screen(),
            crate::frame_key::render_sig(t),
        )
    }

    fn content_changed_before_the_frame_key(then: (u64, bool, u64), t: &Terminal) -> bool {
        gate_inputs_before_the_frame_key(t) != then
    }

    #[test]
    fn a_kitty_placement_at_an_unchanged_seq_pushes_cells() {
        let term = Arc::new(Mutex::new(Terminal::new(6, 20)));
        {
            let mut t = crate::term_lock(&term);
            t.set_cell_pixel_size(10, 20);
            t.process(b"x\x1b[3;5H");
        }
        let streams = TargetStreams {
            cells: true,
            ..Default::default()
        };
        let mut w = watch_on(1, &term);
        assert!(wake_text(&mut w, streams, true).contains(" cells "));
        assert!(
            wake_text(&mut w, streams, true).is_empty(),
            "the premise: an unchanged frame re-emits nothing"
        );
        let then = gate_inputs_before_the_frame_key(&crate::term_lock(&term));
        let b64 = aterm_codec::base64::encode(&[0u8; 8]).expect("encode");
        crate::term_lock(&term)
            .process(format!("\x1b_Ga=T,f=32,s=2,v=1,c=2,r=1,C=1,q=2,i=7;{b64}\x1b\\").as_bytes());
        {
            let t = crate::term_lock(&term);
            assert_eq!(
                t.content_seq(),
                then.0,
                "the premise: a C=1 placement moves no content_seq"
            );
            assert!(
                !content_changed_before_the_frame_key(then, &t),
                "the negative control: the gate before the frame key saw no change"
            );
        }
        let f = wake_text(&mut w, streams, true);
        assert!(
            f.starts_with(&format!("DELTA 1 seq={} cells ", then.0)) && f.contains("\"b64\":"),
            "the placed image is pushed at the unchanged seq: {f:?}"
        );
        assert!(!f.contains("GAP"), "no resync for an unchanged seq: {f:?}");
        assert!(
            wake_text(&mut w, streams, true).is_empty(),
            "the placement is pushed once"
        );
    }

    #[test]
    fn a_title_only_batch_pushes_no_cells() {
        let term = Arc::new(Mutex::new(Terminal::new(4, 20)));
        crate::term_lock(&term).process(b"x");
        let streams = TargetStreams {
            cells: true,
            ..Default::default()
        };
        let mut w = watch_on(1, &term);
        assert!(wake_text(&mut w, streams, true).contains(" cells "));
        let batches: [&[u8]; 4] = [b"\x1b]2;t\x07", b"\x07", b"\x1b[6n", b"\x1b[?2004h"];
        for batch in batches {
            let (seq, etag) = {
                let t = crate::term_lock(&term);
                (t.content_seq(), FrameKey::read(&t).etag())
            };
            crate::term_lock(&term).process(batch);
            {
                let t = crate::term_lock(&term);
                assert_eq!(
                    t.content_seq(),
                    seq,
                    "{batch:?}: the premise: no content write"
                );
                assert_ne!(
                    FrameKey::read(&t).etag(),
                    etag,
                    "{batch:?}: the negative control: the key moved, so the etag-only rule pushed"
                );
            }
            let f = wake_text(&mut w, streams, true);
            assert!(
                f.is_empty(),
                "{batch:?}: an unchanged frame is not re-sent: {f:?}"
            );
        }
    }

    #[test]
    fn one_change_is_gathered_once_for_every_reader() {
        use crate::work_counts;
        let streams = TargetStreams {
            cells: true,
            ..Default::default()
        };
        let readers = |held: bool| {
            let h = crate::session_store::test_handle(1);
            let target = target_of(&h);
            let watch = || {
                let w = new_watch(&target, streams, &PushOptions::default());
                if held {
                    w
                } else {
                    Watch {
                        _cells_hold: None,
                        ..w
                    }
                }
            };
            let (mut a, mut b) = (watch(), watch());
            crate::term_lock(&h.term).process(b"x");
            assert!(wake_text(&mut a, streams, true).contains(" cells "));
            assert!(wake_text(&mut b, streams, true).contains(" cells "));
            work_counts::take_styled_frames_gathered();
            crate::term_lock(&h.term).process(b"y");
            let fa = wake_text(&mut a, streams, true);
            let fb = wake_text(&mut b, streams, true);
            let screen = crate::control::cmd_screen_styled_json(&h.term, &h.ctx.frame, "");
            let gathered = work_counts::take_styled_frames_gathered();
            assert!(fa.contains(" cells ") && fa == fb, "{fa:?} / {fb:?}");
            let body = fa
                .split_once('\n')
                .map(|(_, rest)| rest)
                .unwrap_or_default();
            let body = body.strip_suffix("}\n").expect("the frame ends the DELTA");
            assert!(
                screen.starts_with(&format!("OK 1\n{body},\"etag\":")),
                "`screen` serves the frame the watches pushed: {screen:.80}"
            );
            gathered
        };
        assert_eq!(readers(true), 1, "one gather, shared by three readers");
        assert_eq!(
            readers(false),
            3,
            "the negative control: each reader gathered"
        );
    }

    #[test]
    fn a_screen_read_pins_no_frame_and_the_last_cells_watch_frees_it() {
        use crate::work_counts;
        let h = crate::session_store::test_handle(1);
        crate::term_lock(&h.term).process(b"x");
        let read = || crate::control::cmd_screen_styled_json(&h.term, &h.ctx.frame, "");
        work_counts::take_styled_frames_gathered();
        let first = read();
        assert_eq!(read(), first);
        assert_eq!(
            work_counts::take_styled_frames_gathered(),
            2,
            "nothing kept the first read's frame"
        );

        let streams = TargetStreams {
            cells: true,
            ..Default::default()
        };
        let mut w = new_watch(&target_of(&h), streams, &PushOptions::default());
        assert!(wake_text(&mut w, streams, true).contains(" cells "));
        assert_eq!(read(), first);
        assert_eq!(
            work_counts::take_styled_frames_gathered(),
            1,
            "the watch's frame served the read"
        );
        drop(w);
        assert_eq!(read(), first);
        assert_eq!(
            work_counts::take_styled_frames_gathered(),
            1,
            "the last watch took the frame with it"
        );
    }

    #[test]
    fn a_caret_only_move_resends_no_screen_rows() {
        let term = Arc::new(Mutex::new(Terminal::new(4, 20)));
        crate::term_lock(&term).process(b"abc");
        let streams = TargetStreams {
            screen: true,
            cursor: true,
            ..Default::default()
        };
        let mut w = watch_on(1, &term);
        let first = wake_text(&mut w, streams, true);
        assert!(
            first.contains(" screen ") && first.contains(" cursor 0 3 "),
            "{first:?}"
        );
        let (then, text_then) = {
            let t = crate::term_lock(&term);
            (gate_inputs_before_the_frame_key(&t), TextKey::read(&t))
        };
        crate::term_lock(&term).process(b"\x08");
        {
            let t = crate::term_lock(&term);
            assert_eq!(
                t.content_seq(),
                then.0,
                "the premise: BS moves no content_seq"
            );
            assert_ne!(
                TextKey::read(&t),
                text_then,
                "the premise: the batch moved the text key, so the rows are compared"
            );
            assert!(
                content_changed_before_the_frame_key(then, &t),
                "the negative control: the gate before the frame key re-sent the screen"
            );
        }
        let f = wake_text(&mut w, streams, true);
        assert_eq!(f.lines().count(), 1, "one frame: {f:?}");
        assert!(
            f.starts_with(&format!("DELTA 1 seq={} cursor 0 2 ", then.0)),
            "the caret move is one cursor DELTA and no screen DELTA: {f:?}"
        );
    }

    #[test]
    fn an_alt_re_entry_at_an_equal_seq_pushes_the_new_screen_without_a_gap() {
        let term = Arc::new(Mutex::new(Terminal::new(6, 30)));
        crate::term_lock(&term).process(b"\x1b[?1049h\x1b[H ls -la /tmp/work\r\n [y/n]\r\n");
        let streams = TargetStreams {
            screen: true,
            ..Default::default()
        };
        let mut w = watch_on(1, &term);
        let first = wake_text(&mut w, streams, true);
        assert!(first.contains("ls -la"), "{first:?}");
        let (then, epoch_then) = {
            let t = crate::term_lock(&term);
            (
                gate_inputs_before_the_frame_key(&t),
                crate::control::screen_gen(&t).epoch,
            )
        };
        crate::term_lock(&term)
            .process(b"\x1b[?1049l\x1b[?1049h\x1b[H rm -rf /tmp/work\r\n [y/n]\r\n");
        {
            let t = crate::term_lock(&term);
            assert_eq!(
                (t.content_seq(), t.is_alternate_screen()),
                (then.0, true),
                "the premise: the second dialog reads the first one's seq, on the alt buffer"
            );
            assert!(
                !content_changed_before_the_frame_key(then, &t),
                "the negative control: the gate before the frame key saw no change"
            );
        }
        let f = wake_text(&mut w, streams, true);
        assert!(
            f.starts_with(&format!("DELTA 1 seq={} screen ", then.0)) && f.contains("rm -rf"),
            "the second dialog is pushed: {f:?}"
        );
        assert!(!f.contains("GAP"), "an epoch step is no resync: {f:?}");

        let epoch = crate::control::screen_gen(&crate::term_lock(&term)).epoch;
        assert_ne!(
            epoch, epoch_then,
            "the premise: the re-entry stepped the epoch"
        );
        crate::term_lock(&term).process(b"\x1b[1;3r\x1b[1;1H\x1bM");
        assert_ne!(
            crate::control::screen_gen(&crate::term_lock(&term)).epoch,
            epoch,
            "the premise: RI at the region's top steps the epoch"
        );
        let scrolled = wake_text(&mut w, streams, true);
        assert!(
            scrolled.starts_with("DELTA 1 seq=") && scrolled.contains(" screen "),
            "the region scroll is pushed: {scrolled:?}"
        );
        assert!(
            !scrolled.contains("GAP"),
            "a region scroll is no resync: {scrolled:?}"
        );
    }

    #[test]
    fn bytes_only_subscription_gets_its_bursts_stamped() {
        let fan = Arc::new(ByteFanout::new());
        let term = Arc::new(Mutex::new(Terminal::new(2, 4)));
        let bs = fan.subscribe();
        fan.tee(&Arc::from(&b"out"[..]), 9_000_000_000_000);
        let w = Watch {
            byte_sub: Some(bs),
            ..watch_on(7, &term)
        };
        let stamped = String::from_utf8(wire(true, drain_bytes_frames(&w))).unwrap();
        assert!(
            stamped.starts_with("T 7 9000000000000\n"),
            "the bytes burst is stamped with its arrival: {stamped:?}"
        );
        assert!(
            stamped.contains("\nBYTES 7 3\nout\n"),
            "the burst follows its stamp, byte-exact: {stamped:?}"
        );

        fan.tee(&Arc::from(&b"a"[..]), 0);
        let unstamped = String::from_utf8(wire(false, drain_bytes_frames(&w))).unwrap();
        assert_eq!(
            unstamped, "BYTES 7 1\na\n",
            "without the modifier the stream is byte-identical to the untimed one"
        );

        let tagged = String::from_utf8(wire(
            true,
            vec![
                Frame::text(Tag::Channel(7), 2, "EVENT 7 bell total=1\n".to_string()),
                Frame::text(Tag::Channel(7), 1, "BYTES 7 1\nz\n".to_string()),
                Frame::text(Tag::Instance, 3, "EVENT * session-exited s-a\n".to_string()),
            ],
        ))
        .unwrap();
        let stamps = |tag: &str| {
            tagged
                .lines()
                .filter(|l| l.starts_with(&format!("T {tag} ")))
                .count()
        };
        assert_eq!(stamps("7"), 1, "one stamp per tag per wake: {tagged:?}");
        assert_eq!(
            stamps("*"),
            1,
            "the instance channel is stamped too, and separately: {tagged:?}"
        );
    }

    #[test]
    fn the_stamp_is_the_sample_instant() {
        let term = Arc::new(Mutex::new(Terminal::new(4, 20)));
        crate::term_lock(&term).process(b"hello");
        let streams = TargetStreams {
            screen: true,
            ..Default::default()
        };
        let mut w = watch_on(7, &term);
        let before = crate::metrics::now_us();
        let frames = frames_for_watch(&mut w, streams, true);
        let after = crate::metrics::now_us();
        let read_at = frames[0].t_us;
        assert!(
            (before..=after).contains(&read_at),
            "the frame is dated inside its read: {before} <= {read_at} <= {after}"
        );
        let stamped = String::from_utf8(wire(true, frames)).unwrap();
        assert!(
            stamped.starts_with(&format!("T 7 {read_at}\nDELTA 7 ")),
            "the T line says the read's instant: {stamped:?}"
        );

        const DAY_US: u64 = 86_400_000_000;
        let event = |t_us: u64, body: &str| Frame::text(Tag::Channel(7), t_us, body.to_string());
        let mut sink: Vec<u8> = Vec::new();
        let mut egress = Egress::new(&mut sink, true);
        egress.begin_wake();
        egress
            .send(vec![
                event(DAY_US, ""),
                event(50 * DAY_US, "EVENT 7 a\n"),
                event(30 * DAY_US, "EVENT 7 b\n"),
            ])
            .unwrap();
        egress
            .send(vec![event(10 * DAY_US, "EVENT 7 c\n")])
            .unwrap();
        egress.begin_wake();
        egress
            .send(vec![event(90 * DAY_US, "EVENT 7 d\n")])
            .unwrap();
        assert_eq!(
            String::from_utf8(sink).unwrap(),
            format!(
                "T 7 {}\nEVENT 7 a\nEVENT 7 b\nEVENT 7 c\nT 7 {}\nEVENT 7 d\n",
                30 * DAY_US,
                90 * DAY_US
            )
        );
    }

    #[test]
    fn a_burst_teed_after_a_screen_read_is_stamped_before_it() {
        const DAY_US: u64 = 86_400_000_000;
        let term = Arc::new(Mutex::new(Terminal::new(2, 8)));
        let fan = Arc::new(ByteFanout::new());
        let w = Watch {
            byte_sub: Some(fan.subscribe()),
            ..watch_on(7, &term)
        };
        let mut sink: Vec<u8> = Vec::new();
        let mut egress = Egress::new(&mut sink, true);
        egress.begin_wake();
        let screen = Frame::text(
            Tag::Channel(7),
            2 * DAY_US,
            "DELTA 7 seq=1 screen 0\n".into(),
        );
        let mut frames = vec![screen];
        frames.extend(drain_bytes_frames(&w));
        egress.send(frames).unwrap();
        fan.tee(&Arc::from(&b"hi"[..]), DAY_US);
        egress.begin_wake();
        egress.send(drain_bytes_frames(&w)).unwrap();
        assert_eq!(
            String::from_utf8(sink).unwrap(),
            format!(
                "T 7 {}\nDELTA 7 seq=1 screen 0\nT 7 {DAY_US}\nBYTES 7 2\nhi\n",
                2 * DAY_US
            ),
            "the later stamp is the burst's earlier arrival"
        );
    }

    struct Counted {
        wire: Vec<u8>,
        calls: usize,
        cap: usize,
    }

    impl Counted {
        fn new(cap: usize) -> Self {
            Counted {
                wire: Vec::new(),
                calls: 0,
                cap,
            }
        }
    }

    impl Write for Counted {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.write_vectored(&[IoSlice::new(buf)])
        }

        fn write_vectored(&mut self, bufs: &[IoSlice<'_>]) -> std::io::Result<usize> {
            self.calls += 1;
            let mut took = 0;
            for buf in bufs {
                let n = buf.len().min(self.cap - took);
                self.wire.extend_from_slice(&buf[..n]);
                took += n;
                if took == self.cap {
                    break;
                }
            }
            Ok(took)
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn a_watch_is_one_vectored_write() {
        let streams = TargetStreams {
            screen: true,
            cursor: true,
            events: true,
            bytes: true,
            ..Default::default()
        };
        let rang = |cap: usize| {
            let term = Arc::new(Mutex::new(Terminal::new(2, 8)));
            crate::term_lock(&term).process(b"\x1b]2;job\x07hi");
            let fan = Arc::new(ByteFanout::new());
            let bs = fan.subscribe();
            fan.tee(&Arc::from(&b"hi"[..]), 0);
            fan.tee(&Arc::from(&[0x80u8, 0x00][..]), 0);
            let mut watches = [Watch {
                byte_sub: Some(bs),
                ..watch_on(7, &term)
            }];
            watches[0].dirty.store(true, Ordering::Release);
            let mut sink = Counted::new(cap);
            let mut egress = Egress::new(&mut sink, true);
            egress.begin_wake();
            visit_watches(&mut watches, streams, &mut egress).unwrap();
            (sink, watches, fan)
        };
        let (one, ..) = rang(usize::MAX);
        assert_eq!(one.calls, 1, "one watch, one write");
        let wire = String::from_utf8_lossy(&one.wire).into_owned();
        for part in [
            "DELTA 7 seq=",
            " screen 2\nhi\n",
            " cursor 0 2 1 ",
            "EVENT 7 title job\n",
            "BYTES 7 2\nhi\nBYTES 7 2\n\u{fffd}\0\n",
        ] {
            assert!(wire.contains(part), "{part:?} in {wire:?}");
        }
        let (short, ..) = rang(5);
        assert_eq!(
            short.wire, one.wire,
            "a short write loses and repeats nothing"
        );
        assert!(short.calls > 1, "the five-byte writer was written to again");

        let (_, mut watches, fan) = rang(usize::MAX);
        crate::term_lock(&watches[0].term).process(b"\x1b]2;job2\x07!");
        fan.tee(&Arc::from(&b"!"[..]), 0);
        let mut frames = frames_for_watch(&mut watches[0], streams, true);
        frames.extend(drain_bytes_frames(&watches[0]));
        let k = frames.len();
        assert_eq!(k, 3, "content, events and bytes frames: {k}");
        let mut sink = Counted::new(usize::MAX);
        let mut egress = Egress::new(&mut sink, true);
        egress.begin_wake();
        for f in frames {
            egress.send(vec![f]).unwrap();
        }
        assert_eq!(sink.calls, k, "the negative control: a write per frame");
    }

    #[test]
    fn a_cells_frame_writes_the_published_body_by_reference() {
        let streams = TargetStreams {
            cells: true,
            ..Default::default()
        };
        let h = crate::session_store::test_handle(1);
        let target = target_of(&h);
        let mut a = new_watch(&target, streams, &PushOptions::default());
        let mut b = new_watch(&target, streams, &PushOptions::default());
        crate::term_lock(&h.term).process(b"\x1b[1mhi");
        let shared = |frames: &[Frame]| {
            frames
                .iter()
                .flat_map(|f| &f.segs)
                .find_map(|seg| match seg {
                    Seg::Shared(frame) => Some(Arc::clone(frame)),
                    _ => None,
                })
                .expect("a cells frame carries the published body")
        };
        let from_a = frames_for_watch(&mut a, streams, true);
        let from_b = frames_for_watch(&mut b, streams, true);
        let published = {
            let look = h.ctx.frame.look_frame();
            let t = crate::term_lock(&h.term);
            match look.styled(&t, &FrameKey::read(&t)) {
                Styled::Hit(frame) => frame,
                Styled::Miss(_) => panic!("the first watch published its frame"),
            }
        };
        assert!(
            Arc::ptr_eq(&shared(&from_a), &published) && Arc::ptr_eq(&shared(&from_b), &published),
            "each watch writes the slot's one frame"
        );
        let wire = text(&from_b);
        assert!(
            wire.ends_with(&format!(
                " cells {}\n{}\n",
                published.body().len(),
                published.body()
            )),
            "the wire is the header, the published body and its terminator: {wire:.80}"
        );
    }

    #[test]
    fn cells_delta_carries_styled_payload() {
        let term = Arc::new(Mutex::new(Terminal::new(2, 4)));
        crate::term_lock(&term).process(b"\x1b[1mhi");
        let streams = TargetStreams {
            cells: true,
            ..Default::default()
        };
        let mut w = watch_on(5, &term);
        let f = wake_text(&mut w, streams, true);
        assert!(f.starts_with("DELTA 5 seq="), "sid-tagged cells delta: {f}");
        assert!(f.contains(" cells "), "is a cells frame: {f}");
        assert!(
            f.contains("\"rows\":[["),
            "carries the styled frame payload: {f}"
        );
        assert!(f.contains("\"bold\""), "carries resolved decorations: {f}");
        let header = f.lines().next().unwrap();
        let nbytes: usize = header.rsplit(' ').next().unwrap().parse().unwrap();
        let body = &f[header.len() + 1..f.len() - 1];
        assert_eq!(body.len(), nbytes, "length prefix matches body: {f}");
    }

    #[test]
    fn meta_change_drains_as_a_meta_event_and_watermark_advances() {
        let timeline = std::sync::Arc::new(std::sync::Mutex::new(
            crate::session_timeline::SessionTimeline::for_test(),
        ));
        timeline
            .lock()
            .unwrap()
            .record("spawned", "state=alive".to_string());
        timeline
            .lock()
            .unwrap()
            .record_raw_meta_change("field=title value=old".to_string());
        let (seeded, _) = initial_timeline_watermarks(
            &timeline,
            TargetStreams {
                events: true,
                ..Default::default()
            },
        );
        assert_eq!(seeded, Some(2), "seeded to the live high");

        let mut out = String::new();
        let wm = drain_timeline_events(&timeline.lock().unwrap(), "7", seeded, &mut out);
        assert!(
            out.is_empty(),
            "no replay of pre-subscription history: {out}"
        );
        assert_eq!(wm, Some(2));

        timeline
            .lock()
            .unwrap()
            .record_raw_meta_change("field=title value=build%20agent".to_string());
        timeline
            .lock()
            .unwrap()
            .record("state-change", "state=exited".to_string());
        let mut out = String::new();
        let wm = drain_timeline_events(&timeline.lock().unwrap(), "7", wm, &mut out);
        assert_eq!(
            out, "EVENT 7 meta field=title value=build%20agent\n",
            "exactly the meta record, verbatim payload"
        );
        assert_eq!(wm, Some(4), "watermark passes the non-meta record too");
        let mut out = String::new();
        let wm2 = drain_timeline_events(&timeline.lock().unwrap(), "7", wm, &mut out);
        assert!(out.is_empty());
        assert_eq!(wm2, Some(4));
        timeline.lock().unwrap().record("human", String::new());
        let mut out = String::new();
        drain_timeline_events(&timeline.lock().unwrap(), "7", wm2, &mut out);
        assert_eq!(out, "EVENT 7 human\n");
    }

    #[test]
    fn a_task_change_pushes_its_rev_and_state() {
        use crate::task_record::{TaskMark, TaskState};
        let timeline = std::sync::Arc::new(std::sync::Mutex::new(
            crate::session_timeline::SessionTimeline::for_test(),
        ));
        let (seeded, _) = initial_timeline_watermarks(
            &timeline,
            TargetStreams {
                events: true,
                ..Default::default()
            },
        );
        let now = std::time::Instant::now();
        let mut exited = TaskMark::new(TaskState::Exited);
        exited.why = Some("exit");
        exited.exit = Some(3);
        exited.result_path = Some(std::path::PathBuf::from("/s/tasks/r"));
        {
            let mut t = timeline.lock().unwrap();
            let _ = t.mark_task(&TaskMark::new(TaskState::Working), false, now, 1);
            let _ = t.mark_task(&exited, false, now, 2);
            let _ = t.mark_task(&exited, false, now, 3);
        }
        let mut out = String::new();
        let wm = drain_timeline_events(&timeline.lock().unwrap(), "7", seeded, &mut out);
        assert_eq!(
            out,
            "EVENT 7 task rev=1 state=working attention=0\n\
             EVENT 7 task rev=2 state=exited attention=0 why=exit exit=3\n"
        );
        assert_eq!(wm, Some(2));
    }

    #[test]
    fn an_evicted_timeline_record_is_reported_as_a_gap() {
        use crate::session_timeline::TIMELINE_CAP;
        let timeline = std::sync::Arc::new(std::sync::Mutex::new(
            crate::session_timeline::SessionTimeline::for_test(),
        ));
        timeline
            .lock()
            .unwrap()
            .record("spawned", "state=alive".to_string());
        let (seeded, _) = initial_timeline_watermarks(
            &timeline,
            TargetStreams {
                events: true,
                ..Default::default()
            },
        );
        assert_eq!(seeded, Some(1));
        timeline
            .lock()
            .unwrap()
            .record("topic", "add t since=head".to_string());
        let mut out = String::new();
        let wm = drain_timeline_events(&timeline.lock().unwrap(), "7", seeded, &mut out);
        assert_eq!(out, "EVENT 7 topic add t since=head\n");
        assert_eq!(wm, Some(2));
        for i in 0..(TIMELINE_CAP + 5) {
            timeline
                .lock()
                .unwrap()
                .record("topic", format!("drop t{i}"));
        }
        let (low, high) = {
            let tl = timeline.lock().unwrap();
            (tl.since(None).next().unwrap().id, tl.high_id().unwrap())
        };
        assert_eq!(high, 2 + TIMELINE_CAP as u64 + 5);
        assert_eq!(low, high - TIMELINE_CAP as u64 + 1);
        let mut out = String::new();
        let wm = drain_timeline_events(&timeline.lock().unwrap(), "7", wm, &mut out);
        let missed = low - 2 - 1;
        assert!(
            out.starts_with(&format!("GAP 7 events-dropped={missed}\n")),
            "{}",
            &out[..out.len().min(120)]
        );
        assert_eq!(
            out.matches("EVENT 7 topic drop").count(),
            TIMELINE_CAP,
            "every retained row follows the GAP"
        );
        assert_eq!(wm, Some(high));
        let mut out = String::new();
        drain_timeline_events(&timeline.lock().unwrap(), "7", wm, &mut out);
        assert!(out.is_empty());
    }

    #[test]
    fn push_loop_delivers_closing_before_exited_for_a_ctl_close() {
        let store = crate::session_store::new_store();
        let h = crate::session_store::test_handle(9);
        let target = target_of(&h);
        store.write().unwrap_or_else(|p| p.into_inner()).register(h);
        let streams = TargetStreams {
            events: true,
            ..Default::default()
        };
        let mut sink: Vec<u8> = Vec::new();
        let mut closed = false;
        let close_once = || {
            if !closed {
                closed = true;
                store
                    .write()
                    .unwrap_or_else(|p| p.into_inner())
                    .deregister_local_as(
                        9,
                        crate::session_store::ExitReason::CtlClose,
                        crate::session_store::ExitActor::Sid("s-boss".into()),
                    );
            }
            false
        };
        push_loop_with_peer_probe(
            &store,
            &[target],
            PushScopes {
                streams,
                instance: InstanceStreams::default(),
                adopt: AdoptScope::none(),
            },
            PushOptions::default(),
            "",
            &mut sink,
            close_once,
        );
        assert!(
            closed,
            "the probe ran: the loop reached its first real wake"
        );
        let out = String::from_utf8_lossy(&sink).into_owned();
        assert_eq!(
            out, "EVENT 9 closing reason=ctl-close by=s-boss\nEVENT 9 exited\n",
            "the whole stream after subscription is the ledger row, then the end marker"
        );
    }

    struct HeldHere;

    impl HeldHere {
        fn on(steps: &'static [&'static str]) -> Self {
            HELD_HERE.with(|held| held.set(steps));
            HeldHere
        }
    }

    impl Drop for HeldHere {
        fn drop(&mut self) {
            HELD_HERE.with(|held| held.set(&[]));
        }
    }

    #[derive(Clone, Default)]
    struct Shared(std::rc::Rc<std::cell::RefCell<Vec<u8>>>);

    impl Shared {
        fn text(&self) -> String {
            String::from_utf8_lossy(&self.0.borrow()).into_owned()
        }
    }

    impl Write for Shared {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.borrow_mut().extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn the_timeline_hold_withholds_live_drains_and_never_a_closing_tail() {
        let store = crate::session_store::new_store();
        let h = crate::session_store::test_handle(9);
        let target = target_of(&h);
        let timeline = h.ctx.timeline.clone();
        store.write().unwrap_or_else(|p| p.into_inner()).register(h);
        let wire = Shared::default();
        let _held = HeldHere::on(&["timeline"]);
        let mut wakes = 0;
        let mut hung = hang_detector("the closing pass after the deregistration");
        push_loop_with_peer_probe(
            &store,
            &[target],
            PushScopes {
                streams: TargetStreams {
                    events: true,
                    ..Default::default()
                },
                instance: InstanceStreams::default(),
                adopt: AdoptScope::none(),
            },
            PushOptions::default(),
            "",
            &mut wire.clone(),
            || {
                wakes += 1;
                if wakes == 1 {
                    timeline
                        .lock()
                        .unwrap_or_else(|p| p.into_inner())
                        .record_raw_meta_change("field=title value=held".to_string());
                } else if wakes == 2 {
                    assert_eq!(wire.text(), "", "the first wake's live drain was held");
                    store
                        .write()
                        .unwrap_or_else(|p| p.into_inner())
                        .deregister_local_as(
                            9,
                            crate::session_store::ExitReason::CtlClose,
                            crate::session_store::ExitActor::Sid("s-boss".into()),
                        );
                }
                hung()
            },
        );
        assert_eq!(
            wire.text(),
            "EVENT 9 meta field=title value=held\n\
             EVENT 9 closing reason=ctl-close by=s-boss\n\
             EVENT 9 exited\n",
            "the closing pass drains past the hold: the held record, the ledger row, the end marker"
        );
    }

    #[test]
    fn a_lifted_timeline_hold_is_drained_on_the_next_pass_with_no_ring() {
        use crate::session_timeline::TIMELINE_CAP;
        let streams = TargetStreams {
            events: true,
            ..Default::default()
        };
        let pass = |watches: &mut [Watch]| {
            for w in watches.iter_mut() {
                w.pace = Paces::default();
            }
            let mut wire = Vec::new();
            let mut egress = Egress::new(&mut wire, false);
            egress.begin_wake();
            visit_watches(watches, streams, &mut egress).expect("a Vec takes every write");
            String::from_utf8(wire).expect("text frames")
        };
        let record = |w: &Watch, payload: String| {
            w.src
                .timeline
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .record_raw_meta_change(payload);
        };
        let held_past_the_ring = || {
            let term = Arc::new(Mutex::new(Terminal::new(2, 8)));
            let mut watch = watch_on(9, &term);
            let mut sub = Subscription::new();
            watch.dirty = sub.watch(&watch.src.watchers);
            let mut watches = [watch];
            record(&watches[0], "x=seed".to_string());
            let seed = pass(&mut watches);
            assert!(
                seed.contains("EVENT 9 meta x=seed\n"),
                "the seed record is read with no hold: {seed:?}"
            );
            let held = HeldHere::on(&["timeline"]);
            record(&watches[0], "x=lost".to_string());
            for i in 0..TIMELINE_CAP {
                record(&watches[0], format!("x={i}"));
            }
            assert_eq!(
                pass(&mut watches),
                "",
                "the hold withholds the drain the records rang"
            );
            assert!(timeline_owed(&watches), "and the loop keeps a timer for it");
            drop(held);
            assert!(
                !watches[0].dirty.load(Ordering::Acquire),
                "nothing rang since the held pass"
            );
            (sub, watches)
        };

        let (_sub, mut watches) = held_past_the_ring();
        let mut want = String::from("GAP 9 events-dropped=1\n");
        for i in 0..TIMELINE_CAP {
            want.push_str(&format!("EVENT 9 meta x={i}\n"));
        }
        let got = pass(&mut watches);
        assert!(
            got == want,
            "the first pass after the hold: the GAP, then every retained record; got {} \
             lines, the first {:?}",
            got.lines().count(),
            got.lines().next()
        );
        assert!(!timeline_owed(&watches), "an unheld drain clears the mark");
        assert_eq!(pass(&mut watches), "", "and nothing is read twice");

        let (_sub, mut watches) = held_past_the_ring();
        watches[0].timeline_held = false;
        assert_eq!(
            pass(&mut watches),
            "",
            "the negative control: a watch left clean is not read"
        );
        assert!(
            !timeline_owed(&watches),
            "the negative control: and keeps no timer"
        );
    }

    struct CheckedPeer<F: FnMut(usize)> {
        wakes: Arc<Watchers>,
        checks: usize,
        check: F,
    }

    impl<F: FnMut(usize)> PeerLiveness for CheckedPeer<F> {
        fn gone(&mut self) -> bool {
            self.checks += 1;
            (self.check)(self.checks);
            false
        }

        fn rings(&self) -> Option<&Arc<Watchers>> {
            Some(&self.wakes)
        }
    }

    #[test]
    fn a_held_timeline_drain_keeps_a_timer_and_is_read_once_the_hold_goes() {
        let store = crate::session_store::new_store();
        let h = crate::session_store::test_handle(4);
        let target = target_of(&h);
        let timeline = h.ctx.timeline.clone();
        store.write().unwrap_or_else(|p| p.into_inner()).register(h);
        let pushed = std::thread::spawn(move || {
            let _held = HeldHere::on(&["timeline"]);
            let peer = CheckedPeer {
                wakes: Arc::default(),
                checks: 0,
                check: move |n| match n {
                    1 => {
                        timeline
                            .lock()
                            .unwrap_or_else(|p| p.into_inner())
                            .record_raw_meta_change("x=held".to_string());
                    }
                    2 => HELD_HERE.with(|held| held.set(&[])),
                    _ => {}
                },
            };
            let mut sink = Scripted::new(|line: &str| line != "EVENT 4 meta x=held");
            push_loop_with_peer_probe(
                &store,
                &[target],
                PushScopes {
                    streams: TargetStreams {
                        events: true,
                        ..Default::default()
                    },
                    instance: InstanceStreams::default(),
                    adopt: AdoptScope::none(),
                },
                PushOptions::default(),
                "",
                &mut sink,
                peer,
            );
            sink.wire
        });
        let wire =
            crate::test_socket::join_after_hang_up(pushed, "the held record to reach the wire");
        assert_eq!(
            wire, "EVENT 4 meta x=held\n",
            "the record made under the hold, once it went"
        );
    }

    struct Stalling<B: FnOnce(), S: FnOnce()> {
        wire: Vec<u8>,
        seeded: Option<B>,
        at: usize,
        stall: Option<S>,
        last: String,
        wakes: Arc<Watchers>,
        gone: Arc<AtomicBool>,
    }

    impl<B: FnOnce(), S: FnOnce()> Write for Stalling<B, S> {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.flush()?;
            if let Some(seeded) = self.seeded.take() {
                seeded();
            }
            let n = if self.wire.len() < self.at {
                buf.len().min(self.at - self.wire.len())
            } else if let Some(stall) = self.stall.take() {
                stall();
                return Err(std::io::ErrorKind::Interrupted.into());
            } else {
                buf.len().min(3)
            };
            self.wire.extend_from_slice(&buf[..n]);
            if self.wire.ends_with(self.last.as_bytes()) {
                self.gone.store(true, Ordering::Release);
                self.wakes.ring_watchers();
            }
            Ok(n)
        }

        fn flush(&mut self) -> std::io::Result<()> {
            if self.wire.ends_with(self.last.as_bytes()) {
                Err(std::io::ErrorKind::BrokenPipe.into())
            } else {
                Ok(())
            }
        }
    }

    fn stalled_wire(n: usize) -> String {
        const HANDSHAKE: &str = "OK subscribe 1\n";
        let pushed = std::thread::spawn(move || {
            let store = crate::session_store::new_store();
            let h = crate::session_store::test_handle(9);
            let target = target_of(&h);
            let timeline = h.ctx.timeline.clone();
            store.write().unwrap_or_else(|p| p.into_inner()).register(h);
            let record = |payload: String| {
                timeline
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .record_raw_meta_change(payload);
            };
            let peer = RingingPeer {
                wakes: Arc::default(),
                gone: Arc::default(),
                asked: Arc::default(),
            };
            let mut sink = Stalling {
                wire: Vec::new(),
                seeded: Some(|| record("x=before".to_string())),
                at: HANDSHAKE.len() + 10,
                stall: Some(|| (0..n).for_each(|i| record(format!("x=s{i}")))),
                last: format!("EVENT 9 meta x=s{}\n", n - 1),
                wakes: Arc::clone(&peer.wakes),
                gone: Arc::clone(&peer.gone),
            };
            push_loop_with_peer_probe(
                &store,
                &[target],
                PushScopes {
                    streams: TargetStreams {
                        events: true,
                        ..Default::default()
                    },
                    instance: InstanceStreams::default(),
                    adopt: AdoptScope::none(),
                },
                PushOptions::default(),
                HANDSHAKE,
                &mut sink,
                peer,
            );
            String::from_utf8(sink.wire).expect("text frames are UTF-8")
        });
        let wire = crate::test_socket::join_after_hang_up(
            pushed,
            "the stalled reader to take its last record and hang up",
        );
        wire.strip_prefix(HANDSHAKE)
            .expect("the handshake comes first")
            .to_string()
    }

    #[test]
    fn a_stalled_readers_records_come_after_the_stall_or_in_its_gap() {
        use crate::session_timeline::TIMELINE_CAP;
        let stalled = |range: std::ops::Range<usize>| -> String {
            range.map(|i| format!("EVENT 9 meta x=s{i}\n")).collect()
        };
        let before = "EVENT 9 meta x=before\n";
        let within = stalled_wire(3);
        assert!(
            within.starts_with(before),
            "x=before is recorded inside the handshake write, so the push loop must seed its \
             watch before it writes the handshake: {within}"
        );
        assert_eq!(
            within,
            format!("{before}{}", stalled(0..3)),
            "within the ring: every record made in the stall, after it"
        );
        let n = TIMELINE_CAP + 5;
        assert_eq!(
            stalled_wire(n),
            format!("{before}GAP 9 events-dropped=5\n{}", stalled(5..n)),
            "past the ring: the five it evicted counted, the rest pushed"
        );
    }

    #[test]
    fn bytes_drain_is_byte_lossless_and_every_frame() {
        let fan = Arc::new(ByteFanout::new());
        let term = Arc::new(Mutex::new(Terminal::new(2, 4)));
        let bs = fan.subscribe();
        fan.tee(&Arc::from(&b"\x1b[31m"[..]), 0);
        fan.tee(&Arc::from(&[0x80u8, 0x00][..]), 0);
        let w = Watch {
            byte_sub: Some(bs),
            ..watch_on(7, &term)
        };
        let out = frame_bytes(&drain_bytes_frames(&w));
        let mut expected: Vec<u8> = Vec::new();
        expected.extend_from_slice(b"BYTES 7 5\n\x1b[31m\n");
        expected.extend_from_slice(b"BYTES 7 2\n\x80\x00\n");
        assert_eq!(out, expected, "byte-exact, every-frame, length-prefixed");
    }

    #[test]
    fn bytes_gap_emitted_when_queue_overflowed() {
        let fan = Arc::new(ByteFanout::with_budget(4));
        let term = Arc::new(Mutex::new(Terminal::new(2, 4)));
        let bs = fan.subscribe();
        for i in 0..10 {
            fan.tee(&Arc::from(&b"abcd"[..]), i);
        }
        let w = Watch {
            byte_sub: Some(bs),
            ..watch_on(7, &term)
        };
        let out = String::from_utf8_lossy(&frame_bytes(&drain_bytes_frames(&w))).into_owned();
        assert!(
            out.starts_with("GAP 7 bytes-dropped="),
            "gap precedes bursts: {out}"
        );
        assert!(out.contains("BYTES 7 4\n"), "newest burst survives: {out}");
    }

    #[test]
    fn drain_title_event_emits_once_per_change_pct_encoded() {
        let term = Arc::new(Mutex::new(Terminal::new(24, 80)));
        crate::term_lock(&term).process(b"\x1b]2;my dir\x07");
        let mut out = String::new();
        let title = |last: Option<&str>| sample_engine_events(&term, None, last).title;
        let sampled = title(None).expect("no watermark must sample a title");
        let wm = drain_title_event("3", Some(sampled), None, &mut out);
        assert!(
            out.contains("EVENT 3 title my%20dir\n"),
            "title emitted + pct-encoded: {out:?}"
        );
        out.clear();
        let sampled = title(wm.as_deref());
        assert!(sampled.is_none(), "an idle sample is no change");
        let wm = drain_title_event("3", sampled, wm, &mut out);
        assert!(out.is_empty(), "unchanged title emits nothing: {out:?}");
        assert_eq!(wm.as_deref(), Some("my dir"), "keep the watermark");
        crate::term_lock(&term).process(b"\x1b]2;other\x07");
        out.clear();
        let sampled = title(wm.as_deref());
        let wm = drain_title_event("3", sampled, wm, &mut out);
        assert!(
            out.contains("EVENT 3 title other\n"),
            "change re-emits: {out:?}"
        );
        assert_eq!(wm.as_deref(), Some("other"));

        crate::term_lock(&term).process(b"\x1b]2;\x07");
        out.clear();
        let sampled = title(wm.as_deref());
        assert_eq!(sampled.as_deref(), Some(""));
        let wm = drain_title_event("3", sampled, wm, &mut out);
        assert_eq!(out, "EVENT 3 title \n");
        assert_eq!(wm.as_deref(), Some(""));
        assert!(title(wm.as_deref()).is_none());
        assert_eq!(
            title(None).as_deref(),
            Some(""),
            "initial empty title still emits"
        );
    }

    #[test]
    fn sample_engine_events_blocks_match_the_forward_filter() {
        let term = Arc::new(Mutex::new(Terminal::new(24, 80)));
        let run = |bytes: &[u8]| {
            crate::term_lock(&term).process(bytes);
        };
        run(b"\x1b]133;A\x07$ \x1b]133;B\x07one\n\x1b]133;C\x07o\n\x1b]133;D;0\x07");
        run(b"\x1b]133;A\x07$ \x1b]133;B\x07two\n\x1b]133;C\x07t\n\x1b]133;D;1\x07");
        run(b"\x1b]133;A\x07$ \x1b]133;B\x07abandoned");
        run(b"\x1b]133;A\x07$ \x1b]133;B\x07three\n\x1b]133;C\x07x\n\x1b]133;D;0\x07");

        let reference = |wm: Option<u64>| -> Vec<(u64, Option<i32>)> {
            let t = crate::term_lock(&term);
            t.all_blocks()
                .filter(|b| b.is_complete() && wm.is_none_or(|h| b.id > h))
                .map(|b| (b.id, b.exit_code))
                .collect()
        };
        let all = reference(None);
        assert_eq!(
            all.len(),
            3,
            "REACH: the fixture must produce three COMPLETED blocks around one \
             abandoned prompt, not {all:?} — otherwise the walk is priced against \
             a shape that cannot exercise the break"
        );
        assert!(
            all.iter().any(|&(_, exit)| exit == Some(1)),
            "REACH: a non-zero exit must be present so the payload is not degenerate"
        );

        let ceiling = all.last().expect("three blocks").0;
        let mut probes: Vec<Option<u64>> = vec![None];
        for id in 0..=(ceiling + 2) {
            probes.push(Some(id));
        }
        for wm in probes {
            assert_eq!(
                sample_engine_events(&term, wm, None).blocks,
                reference(wm),
                "watermark {wm:?} diverged from the forward filter"
            );
        }

        assert!(
            sample_engine_events(&term, Some(ceiling), None)
                .blocks
                .is_empty(),
            "at the high-water an idle wake must produce nothing"
        );
        assert_eq!(
            sample_engine_events(&term, None, None).blocks,
            all,
            "no watermark = every completed block, oldest-first"
        );
    }

    fn fresh_cursors() -> PushCursors {
        PushCursors {
            roster: None,
            adopt_seq: 0,
            pruned_at: None,
            instance_dirty: None,
            reader: None,
        }
    }

    fn roster_cursor(store: &Store) -> RosterCursor {
        let g = store.read().unwrap_or_else(|p| p.into_inner());
        RosterCursor {
            seq: g.roster_seq(),
            known: g.live_sids(),
        }
    }

    #[test]
    fn a_bye_rides_the_instance_stream_and_moves_no_membership() {
        use aterm_types::fabric_grammar::Bye;
        let store = crate::session_store::new_store();
        let mut cursor = roster_cursor(&store);
        let known = cursor.known.clone();
        for bye in [Bye::Handoff, Bye::Quit] {
            store
                .write()
                .unwrap_or_else(|p| p.into_inner())
                .say_bye(bye);
        }
        assert_eq!(
            text(&drain_session_events(&store, &mut cursor)),
            "EVENT * bye handoff\nEVENT * bye quit\n"
        );
        assert_eq!(cursor.known, known);
    }

    #[test]
    fn a_quits_bye_is_on_the_wire_before_its_wait_answers() {
        use aterm_types::fabric_grammar::Bye;
        struct Shared(Arc<Mutex<String>>);
        impl Write for Shared {
            fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
                self.0
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .push_str(&String::from_utf8_lossy(buf));
                Ok(buf.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        let readers_of = |store: &Store| {
            let key = store_key(store);
            ROSTER_READERS
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .written
                .values()
                .filter(|(of, _)| *of == key)
                .count()
        };
        let say_quit = |store: &Store| {
            let mut g = store.write().unwrap_or_else(|p| p.into_inner());
            g.say_bye(Bye::Quit);
            g.roster_seq()
        };
        let store = crate::session_store::new_store();
        let wire = Arc::new(Mutex::new(String::new()));
        let ended = Arc::new(AtomicBool::new(false));
        let lane = {
            let (store, wire, ended) = (store.clone(), Arc::clone(&wire), Arc::clone(&ended));
            std::thread::spawn(move || {
                let mut hang = hang_detector("the test to end the loop");
                push_loop_with_peer_probe(
                    &store,
                    &[],
                    bridge_scopes(),
                    PushOptions::default(),
                    "",
                    &mut Shared(wire),
                    move || ended.load(Ordering::Acquire) || hang(),
                );
            })
        };
        let deadline = std::time::Instant::now() + aterm_spec::wait::MUST_HAPPEN;
        while readers_of(&store) == 0 {
            assert!(
                std::time::Instant::now() < deadline,
                "the loop never joined"
            );
            std::thread::yield_now();
        }
        let other = crate::session_store::new_store();
        let stuck = RosterReader::join(&other, 0);
        let other_seq = say_quit(&other);

        let seq = say_quit(&store);
        assert!(
            await_roster_written(&store, seq, aterm_spec::wait::MUST_HAPPEN),
            "the loop wrote past the bye"
        );
        assert!(
            wire.lock()
                .unwrap_or_else(|p| p.into_inner())
                .contains("EVENT * bye quit\n"),
            "{:?}",
            wire.lock().unwrap_or_else(|p| p.into_inner())
        );
        assert!(!await_roster_written(
            &other,
            other_seq,
            Duration::from_millis(50)
        ));
        drop(stuck);
        assert!(await_roster_written(
            &other,
            other_seq,
            aterm_spec::wait::MUST_HAPPEN
        ));
        ended.store(true, Ordering::Release);
        lane.join().expect("the push loop ends");
        assert_eq!(readers_of(&store), 0, "the ended loop left the readers");
    }

    #[test]
    fn a_sub_tick_spawn_and_exit_both_surface() {
        let store = crate::session_store::new_store();
        let mut cursor = roster_cursor(&store);
        let before = cursor.known.clone();

        let h = crate::session_store::test_handle(7);
        let sid = h.sid.as_str().to_string();
        {
            let mut g = store.write().unwrap_or_else(|p| p.into_inner());
            g.register(h);
            g.deregister_local_as(
                7,
                crate::session_store::ExitReason::CtlClose,
                crate::session_store::ExitActor::Sid("s-boss".into()),
            );
        }

        let after = store.read().unwrap_or_else(|p| p.into_inner()).live_sids();
        let mut old_way = String::new();
        diff_session_events(&after, &before, &|_| "", &mut old_way);
        assert!(
            old_way.is_empty(),
            "the snapshot diff cannot see a cancelled-out pair — if it can, this \
             fixture is not reproducing the sub-tick window: {old_way:?}"
        );

        let got = text(&drain_session_events(&store, &mut cursor));
        assert!(
            got.contains(&format!("EVENT * session-created {sid}\n")),
            "the spawn must surface: {got:?}"
        );
        assert!(
            got.contains(&format!("EVENT * session-exited {sid} reason=ctl-close\n")),
            "the exit must surface, WITH the journalled reason as the trailing token: {got:?}"
        );
        assert!(
            got.find("session-created") < got.find("session-exited"),
            "in the order they happened: {got:?}"
        );
        assert!(
            drain_session_events(&store, &mut cursor).is_empty(),
            "a caught-up cursor emits nothing"
        );
    }

    #[test]
    fn a_cursor_past_the_journal_low_water_rebuilds_and_diffs() {
        use crate::session_store::ROSTER_JOURNAL_CAP;
        let store = crate::session_store::new_store();
        let mut cursor = RosterCursor {
            seq: 0,
            known: std::collections::HashSet::new(),
        };
        let survivor;
        {
            let mut g = store.write().unwrap_or_else(|p| p.into_inner());
            let h0 = crate::session_store::test_handle(0);
            survivor = h0.sid.as_str().to_string();
            g.register(h0);
            for i in 1..=(ROSTER_JOURNAL_CAP as u64) {
                g.register(crate::session_store::test_handle(i));
                g.deregister_local(i);
            }
            assert!(
                g.roster_low_seq().expect("journal is non-empty") > cursor.seq + 1,
                "REACH: the fixture must push the cursor PAST the retained window, \
                 or this test silently exercises the fast path instead"
            );
        }

        let got = text(&drain_session_events(&store, &mut cursor));
        assert_eq!(
            got,
            format!("EVENT * session-created {survivor}\n"),
            "recovery reports the NET set change, not the evicted history"
        );
        assert!(
            drain_session_events(&store, &mut cursor).is_empty(),
            "recovery leaves the cursor caught up"
        );
    }

    #[test]
    fn drain_bell_event_emits_on_new_bells() {
        let term = Arc::new(Mutex::new(Terminal::new(24, 80)));
        let mut out = String::new();
        let bell = |t: &Arc<Mutex<Terminal>>| sample_engine_events(t, None, None).bell;
        let wm = drain_bell_event("4", bell(&term), 0, &mut out);
        assert!(out.is_empty() && wm == 0, "no bell yet: {out:?}");
        crate::term_lock(&term).process(b"\x07");
        out.clear();
        let wm = drain_bell_event("4", bell(&term), wm, &mut out);
        assert!(
            out.contains("EVENT 4 bell total=1\n"),
            "bell emitted: {out:?}"
        );
        out.clear();
        let _ = drain_bell_event("4", bell(&term), wm, &mut out);
        assert!(
            out.is_empty(),
            "unchanged bell total emits nothing: {out:?}"
        );
    }

    #[test]
    fn drain_turn_events_emits_once_per_new_record() {
        use crate::turn_ledger::{TurnBy, TurnLedger, TurnRecord};
        let ledger = Arc::new(Mutex::new(TurnLedger::default()));
        let push_by = |id: u64, submitted: bool, status: &'static str, by: TurnBy| {
            ledger.lock().unwrap().push(TurnRecord {
                id,
                started_ms: id,
                dur_ms: 7,
                submitted,
                status,
                text: format!("m{id}").into(),
                screen_hash: id,
                seq: id,
                arch: crate::turn_ledger::ArchMark::default(),
                by,
                carried: false,
            });
        };
        let push = |id: u64, submitted: bool, status: &'static str| {
            push_by(id, submitted, status, TurnBy::Ctl);
        };

        push(1, true, "settled");
        let mut wm = ledger.lock().unwrap().high_id();
        let mut out = String::new();
        wm = drain_turn_events(&ledger, "3", wm, &mut out);
        assert!(
            out.is_empty(),
            "backlog before subscription is not replayed"
        );

        push(2, false, "timeout");
        out.clear();
        wm = drain_turn_events(&ledger, "3", wm, &mut out);
        assert_eq!(
            out,
            "EVENT 3 turn 2 submitted=0 status=timeout dur_ms=7 by=ctl\n"
        );
        assert_eq!(wm, Some(2));

        out.clear();
        let wm2 = drain_turn_events(&ledger, "3", wm, &mut out);
        assert!(out.is_empty() && wm2 == Some(2), "no double-report");

        push_by(3, true, "settled", TurnBy::Harness);
        out.clear();
        let _ = drain_turn_events(&ledger, "3", wm2, &mut out);
        assert_eq!(
            out,
            "EVENT 3 turn 3 submitted=1 status=settled dur_ms=7 by=harness\n"
        );
    }

    #[test]
    fn every_frame_sends_an_unchanged_body_once() {
        let term = Arc::new(Mutex::new(Terminal::new(2, 4)));
        crate::term_lock(&term).process(b"x");
        let streams = TargetStreams {
            cells: true,
            ..Default::default()
        };
        let mut w = Watch {
            non_coalesced: true,
            ..watch_on(1, &term)
        };
        let first = wake_text(&mut w, streams, true);
        assert!(first.starts_with("DELTA 1 "), "{first:?}");
        let reread = crate::control::serialize_styled_frame(&crate::control::gather_styled_frame(
            &crate::term_lock(&term),
        ));
        assert!(
            first.ends_with(&format!(" cells {}\n{reread}\n", reread.len())),
            "the negative control: the rule before the body id re-sent this body: {first:?}"
        );
        for wake in 1..=2 {
            assert!(
                wake_text(&mut w, streams, true).is_empty(),
                "genuine wake {wake} on an unchanged frame sends nothing"
            );
        }
        w.last_cells_body = Some((0, 0));
        assert!(
            wake_text(&mut w, streams, true).ends_with(&format!("\n{reread}\n")),
            "every-frame sends a body that differs from the last one sent"
        );
        w.last_cells_body = Some((0, 0));
        assert!(
            wake_text(&mut w, streams, false).is_empty(),
            "every-frame must not re-emit on a read no wake drove",
        );
    }

    fn paced_reads(pace: Duration, wakes: u32, every: Duration) -> (u32, Duration, bool) {
        let t0 = Instant::now();
        let last_wake = t0 + every * (wakes - 1);
        let mut p = ReadPace::default();
        let (mut reads, mut last_read, mut next) = (0, t0, 0);
        loop {
            let wake = (next < wakes).then(|| t0 + every * next);
            let (now, woke) = match (wake, p.due(pace)) {
                (Some(w), Some(due)) if due < w => (due, false),
                (Some(w), _) => {
                    next += 1;
                    (w, true)
                }
                (None, Some(due)) => (due, false),
                (None, None) => break,
            };
            if p.visit(now, woke, pace) == Some(true) {
                reads += 1;
                last_read = now;
            }
        }
        (reads, last_read - t0, last_read >= last_wake)
    }

    #[test]
    fn content_reads_are_paced_and_the_last_wake_is_read() {
        let (reads, span, last_landed) = paced_reads(CONTENT_PACE, 200, Duration::from_millis(1));
        let bound = span.as_nanos().div_ceil(CONTENT_PACE.as_nanos()) + 1;
        assert!(
            u128::from(reads) <= bound,
            "{reads} reads over {span:?} exceed ceil(T/PACE)+1 = {bound}"
        );
        assert!(last_landed, "the read owed by the last wake was made");
        assert!(reads < 200, "the pace bounded the reads: {reads}");
        let (unpaced, _, _) = paced_reads(Duration::ZERO, 200, Duration::from_millis(1));
        assert_eq!(
            unpaced, 200,
            "the negative control: unpaced, every wake reads"
        );
    }

    #[derive(Debug, PartialEq, Eq)]
    struct EventsReads {
        at_once: u32,
        records: u32,
        owed: u32,
        last_landed: bool,
        span: Duration,
    }

    fn events_rings(rings: u32, every: Duration, record: impl Fn(u32) -> bool) -> EventsReads {
        let streams = TargetStreams {
            events: true,
            ..Default::default()
        };
        let t0 = Instant::now();
        let last_ring = t0 + every * (rings - 1);
        let mut pace = Paces::default();
        let mut seen = EventsReads {
            at_once: 0,
            records: 0,
            owed: 0,
            last_landed: false,
            span: Duration::ZERO,
        };
        let mut next = 0;
        loop {
            let ring = (next < rings).then(|| t0 + every * next);
            let (now, rang) = match (ring, pace.due()) {
                (Some(r), Some(due)) if due < r => (due, None),
                (Some(r), _) => {
                    next += 1;
                    (r, Some(next - 1))
                }
                (None, Some(due)) => (due, None),
                (None, None) => break,
            };
            let recorded = rang.is_some_and(&record);
            let read = pass_streams(streams, &mut pace, rang.is_some(), recorded, now)
                .is_some_and(|visit| visit.events);
            if !read {
                continue;
            }
            if rang.is_some() {
                seen.at_once += 1;
                seen.records += u32::from(recorded);
            } else {
                seen.owed += 1;
            }
            seen.span = now - t0;
            seen.last_landed = now >= last_ring;
        }
        seen
    }

    #[test]
    fn a_trickle_of_records_is_read_once_per_record_with_no_owed_wait() {
        let every = Duration::from_millis(1);
        let trickle = events_rings(50, every, |_| true);
        assert_eq!(
            (trickle.at_once, trickle.records, trickle.owed),
            (50, 50, 0),
            "every record read at once, none owed: {trickle:?}"
        );
        let paced = events_rings(50, every, |_| false);
        assert!(
            paced.at_once < 50 && paced.owed > 0,
            "the negative control: paced alike, the trickle waits: {paced:?}"
        );
    }

    #[test]
    fn an_output_flood_with_records_stays_paced_and_reads_each_record_at_once() {
        const RINGS: u32 = 2000;
        const EVERY_RECORD: u32 = 50;
        let every = Duration::from_micros(100);
        let records = RINGS / EVERY_RECORD;
        let flood = events_rings(RINGS, every, |i| i % EVERY_RECORD == 0);
        let reads = u128::from(flood.at_once + flood.owed);
        let bound =
            flood.span.as_nanos().div_ceil(EVENTS_PACE.as_nanos()) + 1 + u128::from(records);
        assert!(
            reads <= bound,
            "{reads} reads over {:?} exceed ceil(T/PACE)+1+R = {bound}: {flood:?}",
            flood.span
        );
        assert_eq!(flood.records, records, "every record read at once");
        assert!(flood.last_landed, "the read owed by the last ring was made");
        let unpaced = events_rings(RINGS, every, |_| true);
        assert_eq!(
            unpaced.at_once, RINGS,
            "the negative control: every ring a record's, every ring reads"
        );
    }

    #[test]
    fn a_flooded_screen_watch_is_paced_and_ends_on_the_final_screen() {
        const N: usize = 2000;
        let store = crate::session_store::new_store();
        let term = Arc::new(Mutex::new(Terminal::new(2, 16)));
        crate::term_lock(&term).process(b"start");
        let h = crate::session_store::test_handle_with(5, term.clone());
        let target = target_of(&h);
        let watchers = h.ctx.watchers.clone();
        store.write().unwrap_or_else(|p| p.into_inner()).register(h);
        let (caught_up, flood) = std::sync::mpsc::channel::<()>();
        let producer = {
            let term = term.clone();
            std::thread::spawn(move || {
                if flood.recv().is_err() {
                    return;
                }
                for i in 0..N {
                    crate::term_lock(&term).process(format!("\r\x1b[2Kline {i}").as_bytes());
                    watchers.ring_watchers();
                }
            })
        };
        let final_row = format!("line {}", N - 1);
        let (mut deltas, mut rows_left, mut landed) = (0u128, 0usize, false);
        let mut sink = Scripted::new(|line: &str| {
            if line.starts_with("DELTA 5 seq=") && line.ends_with(" screen 2") {
                deltas += 1;
                if deltas == 1 {
                    let _ = caught_up.send(());
                }
                rows_left = 2;
                return true;
            }
            if rows_left == 2 && line.trim_end() == final_row {
                landed = true;
            }
            rows_left = rows_left.saturating_sub(1);
            !landed
        });
        let started = Instant::now();
        push_loop_with_peer_probe(
            &store,
            &[target],
            PushScopes {
                streams: TargetStreams {
                    screen: true,
                    ..Default::default()
                },
                instance: InstanceStreams::default(),
                adopt: AdoptScope::none(),
            },
            PushOptions::default(),
            "",
            &mut sink,
            hang_detector("the final screen"),
        );
        let run = started.elapsed();
        drop(sink);
        producer.join().expect("the producer finished");
        assert!(landed, "the final screen was pushed");
        let bound = run.as_nanos().div_ceil(CONTENT_PACE.as_nanos()) + 1;
        assert!(
            deltas <= bound,
            "{deltas} screen DELTAs over {run:?} exceed ceil(T/PACE)+1 = {bound}"
        );
    }

    #[test]
    fn a_flooded_events_watch_is_paced_and_ends_on_the_final_title() {
        use crate::work_counts;
        const N: usize = 2000;
        let store = crate::session_store::new_store();
        let term = Arc::new(Mutex::new(Terminal::new(2, 16)));
        let h = crate::session_store::test_handle_with(5, term.clone());
        let target = target_of(&h);
        let watchers = h.ctx.watchers.clone();
        store.write().unwrap_or_else(|p| p.into_inner()).register(h);
        let (seeded, flood) = std::sync::mpsc::channel::<()>();
        let producer = {
            let term = term.clone();
            std::thread::spawn(move || {
                if flood.recv().is_err() {
                    return;
                }
                for i in 0..N {
                    crate::term_lock(&term).process(format!("line {i}\r\n").as_bytes());
                    watchers.ring_watchers();
                }
                crate::term_lock(&term).process(b"\x1b]2;done\x07");
                watchers.ring_watchers();
            })
        };
        let mut landed = false;
        let mut sink = Scripted::new(|line: &str| {
            if line == "sub 5 s-flood" {
                let _ = seeded.send(());
            }
            landed |= line == "EVENT 5 title done";
            !landed
        });
        work_counts::take_engine_samples();
        let started = Instant::now();
        push_loop_with_peer_probe(
            &store,
            &[target],
            PushScopes {
                streams: TargetStreams {
                    events: true,
                    ..Default::default()
                },
                instance: InstanceStreams::default(),
                adopt: AdoptScope::none(),
            },
            PushOptions::default(),
            "sub 5 s-flood\n",
            &mut sink,
            hang_detector("the final title"),
        );
        let run = started.elapsed();
        let samples = work_counts::take_engine_samples();
        drop(sink);
        producer.join().expect("the producer finished");
        assert!(landed, "the final title was pushed");
        let bound = run.as_nanos().div_ceil(EVENTS_PACE.as_nanos()) + 1;
        assert!(
            u128::try_from(samples).expect("a count") <= bound,
            "{samples} engine samples over {run:?} exceed ceil(T/PACE)+1 = {bound}"
        );
    }

    #[test]
    fn a_record_trickle_through_the_loop_waits_out_no_pace() {
        use crate::work_counts;
        const N: usize = 50;
        let store = crate::session_store::new_store();
        let term = Arc::new(Mutex::new(Terminal::new(2, 16)));
        let h = crate::session_store::test_handle_with(5, term);
        let target = target_of(&h);
        let timeline = h.ctx.timeline.clone();
        store.write().unwrap_or_else(|p| p.into_inner()).register(h);
        let record = |n: usize| {
            timeline
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .record_raw_meta_change(format!("n={n}"));
        };
        let mut written = 0;
        let mut sink = Scripted::new(|line: &str| {
            let read = line == format!("EVENT 5 meta n={}", written.max(1) - 1);
            if line == "sub 5 s-trickle" || (read && written < N) {
                record(written);
                written += 1;
                return true;
            }
            !(read && written == N)
        });
        work_counts::take_owed_events_reads();
        work_counts::take_engine_samples();
        push_loop_with_peer_probe(
            &store,
            &[target],
            PushScopes {
                streams: TargetStreams {
                    events: true,
                    ..Default::default()
                },
                instance: InstanceStreams::default(),
                adopt: AdoptScope::none(),
            },
            PushOptions::default(),
            "sub 5 s-trickle\n",
            &mut sink,
            hang_detector("the last record"),
        );
        drop(sink);
        assert_eq!(
            written, N,
            "every record was written after the one before it was read"
        );
        assert_eq!(
            work_counts::take_owed_events_reads(),
            0,
            "no record waited out the pace"
        );
        assert_eq!(
            work_counts::take_engine_samples(),
            N,
            "one read per record, the first the catch-up's"
        );
    }

    #[test]
    fn a_flooded_events_watch_reads_its_records_and_stays_paced() {
        use crate::work_counts;
        const N: usize = 2000;
        const EVERY_RECORD: usize = 100;
        let store = crate::session_store::new_store();
        let term = Arc::new(Mutex::new(Terminal::new(2, 16)));
        let h = crate::session_store::test_handle_with(5, term.clone());
        let target = target_of(&h);
        let watchers = h.ctx.watchers.clone();
        let timeline = h.ctx.timeline.clone();
        store.write().unwrap_or_else(|p| p.into_inner()).register(h);
        let (seeded, flood) = std::sync::mpsc::channel::<()>();
        let producer = std::thread::spawn(move || {
            if flood.recv().is_err() {
                return;
            }
            for i in 0..N {
                crate::term_lock(&term).process(format!("line {i}\r\n").as_bytes());
                if i % EVERY_RECORD == 0 {
                    timeline
                        .lock()
                        .unwrap_or_else(|p| p.into_inner())
                        .record_raw_meta_change(format!("n={i}"));
                }
                watchers.ring_watchers();
            }
            crate::term_lock(&term).process(b"\x1b]2;done\x07");
            watchers.ring_watchers();
        });
        let (mut pushed, mut landed) = (0, false);
        let mut sink = Scripted::new(|line: &str| {
            if line == "sub 5 s-mixed" {
                let _ = seeded.send(());
            }
            pushed += usize::from(line.starts_with("EVENT 5 meta n="));
            landed |= line == "EVENT 5 title done";
            !landed
        });
        work_counts::take_engine_samples();
        let started = Instant::now();
        push_loop_with_peer_probe(
            &store,
            &[target],
            PushScopes {
                streams: TargetStreams {
                    events: true,
                    ..Default::default()
                },
                instance: InstanceStreams::default(),
                adopt: AdoptScope::none(),
            },
            PushOptions::default(),
            "sub 5 s-mixed\n",
            &mut sink,
            hang_detector("the final title"),
        );
        let run = started.elapsed();
        let samples = work_counts::take_engine_samples();
        drop(sink);
        producer.join().expect("the producer finished");
        assert!(landed, "the final title was pushed");
        let records = N.div_ceil(EVERY_RECORD);
        assert_eq!(pushed, records, "every record was pushed");
        let bound = run.as_nanos().div_ceil(EVENTS_PACE.as_nanos()) + 1 + records as u128;
        assert!(
            u128::try_from(samples).expect("a count") <= bound,
            "{samples} engine samples over {run:?} exceed ceil(T/PACE)+1+R = {bound}"
        );
    }

    fn dead_target(local_id: u64) -> (Store, Vec<ResolvedTarget>, Arc<ByteFanout>) {
        let h = crate::session_store::test_handle_with(
            local_id,
            Arc::new(Mutex::new(Terminal::new(4, 8))),
        );
        let fan = h.ctx.byte_fanout.clone();
        (crate::session_store::new_store(), vec![target_of(&h)], fan)
    }

    #[test]
    fn a_closed_session_delivers_its_byte_tail_before_exited() {
        for events in [true, false] {
            let (store, targets, fan) = dead_target(11);
            let streams = TargetStreams {
                bytes: true,
                events,
                ..Default::default()
            };
            let mut sink: Vec<u8> = Vec::new();

            let mut teed = false;
            push_loop_with_peer_probe(
                &store,
                &targets,
                PushScopes {
                    streams,
                    instance: InstanceStreams::default(),
                    adopt: AdoptScope::none(),
                },
                PushOptions::default(),
                "",
                &mut sink,
                || {
                    if !std::mem::replace(&mut teed, true) {
                        assert!(fan.subscriber_count() > 0, "push_loop subscribed first");
                        fan.tee(&Arc::from(&b"tail-bytes"[..]), 0);
                    }
                    false
                },
            );
            assert!(teed, "REACH: the loop woke once and the burst was teed");

            let out = String::from_utf8_lossy(&sink).into_owned();
            let bytes_at = out.find("BYTES 11 10\ntail-bytes\n");
            let exited_at = out.find("EVENT 11 exited\n");
            assert!(
                bytes_at.is_some(),
                "events={events}: the queued tail survived the close: {out:?}"
            );
            if events {
                assert!(exited_at.is_some(), "the close was reported: {out:?}");
                assert!(
                    bytes_at < exited_at,
                    "the tail precedes the end-of-stream marker: {out:?}"
                );
            } else {
                assert_eq!(
                    exited_at, None,
                    "`exited` belongs to the events stream, which was not requested: {out:?}"
                );
            }
        }
    }

    #[test]
    fn prune_closes_from_the_journal_and_survives_a_reused_local_id() {
        use crate::session_store::test_handle;
        let store = crate::session_store::new_store();
        let x = test_handle(5);
        let target = target_of(&x);
        store.write().unwrap_or_else(|p| p.into_inner()).register(x);
        let streams = TargetStreams {
            events: true,
            ..Default::default()
        };
        let mut watches = vec![new_watch(&target, streams, &PushOptions::default())];
        let mut cursors = fresh_cursors();
        let _ = crate::work_counts::take_watches_probed();
        let mut check = |watches: &mut Vec<Watch>| {
            let closed = prune_closed(&store, watches, &mut cursors).len();
            (closed, crate::work_counts::take_watches_probed())
        };
        assert_eq!(check(&mut watches), (0, 1), "the first check probes");
        assert_eq!(check(&mut watches), (0, 0), "an unchanged roster: no probe");
        store
            .write()
            .unwrap_or_else(|p| p.into_inner())
            .register(test_handle(6));
        assert_eq!(check(&mut watches), (0, 0), "a spawn is no death");
        store
            .write()
            .unwrap_or_else(|p| p.into_inner())
            .deregister_local(6);
        assert_eq!(check(&mut watches), (0, 0), "an unwatched exit");
        {
            let mut g = store.write().unwrap_or_else(|p| p.into_inner());
            g.deregister_local(5);
            g.register(test_handle(5));
            assert!(
                g.by_local(5).is_some(),
                "the negative control: by local id alone, the reused id reads live"
            );
        }
        assert_eq!(
            check(&mut watches),
            (1, 1),
            "the journal's exit names the watch, and the probe finds another session"
        );
        assert!(watches.is_empty(), "the old watch left the set");
    }

    #[test]
    fn a_wake_visits_only_the_dirty_watch() {
        const K: usize = 4;
        let store = crate::session_store::new_store();
        let handles: Vec<crate::session_store::SessionHandle> = (1..=K as u64)
            .map(crate::session_store::test_handle)
            .collect();
        {
            let mut g = store.write().unwrap_or_else(|p| p.into_inner());
            for h in &handles {
                g.register(h.clone());
            }
        }
        let targets: Vec<ResolvedTarget> = handles.iter().map(target_of).collect();
        let mut sink = Scripted::new(|line: &str| {
            if line == "OK" {
                handles[0].ctx.watchers.ring_watchers();
            }
            true
        });
        let _ = crate::work_counts::take_watches_visited();
        let mut visits = Vec::new();
        push_loop_with_peer_probe(
            &store,
            &targets,
            PushScopes {
                streams: TargetStreams {
                    events: true,
                    mail: true,
                    ..Default::default()
                },
                instance: InstanceStreams::default(),
                adopt: AdoptScope::none(),
            },
            PushOptions::default(),
            "OK\n",
            &mut sink,
            || {
                visits.push(crate::work_counts::take_watches_visited());
                match visits.len() {
                    2 => handles[2].ctx.watchers.ring_watchers(),
                    3 => handles.iter().for_each(|h| h.ctx.watchers.ring_watchers()),
                    _ => {}
                }
                visits.len() > 3
            },
        );
        assert_eq!(
            visits,
            [K, 0, 1, K],
            "catch-up, a wake already read, one ring, every ring"
        );
    }

    #[test]
    fn a_hosts_selection_rings_its_session_however_busy_another_is() {
        use aterm_core::selection::{SelectionSide, SelectionType};
        let store = crate::session_store::new_store();
        let (a, b) = (
            crate::session_store::test_handle(1),
            crate::session_store::test_handle(2),
        );
        crate::term_lock(&b.term).process(b"hello");
        {
            let mut g = store.write().unwrap_or_else(|p| p.into_inner());
            g.register(a.clone());
            g.register(b.clone());
        }
        let targets = [target_of(&a), target_of(&b)];
        let mut b_cells = false;
        let mut sink = Scripted::new(|line: &str| {
            if line == "OK" {
                a.ctx.watchers.ring_watchers();
                return true;
            }
            if let Some(header) = line.strip_prefix("DELTA ") {
                b_cells = header.starts_with("2 ") && header.contains(" cells ");
                return true;
            }
            !(std::mem::take(&mut b_cells) && line.contains("\"selection\":{"))
        });
        let mut seqs = None;
        let mut hang = hang_detector("B's selection on the wire");
        push_loop_with_peer_probe(
            &store,
            &targets,
            PushScopes {
                streams: TargetStreams {
                    cells: true,
                    ..Default::default()
                },
                instance: InstanceStreams::default(),
                adopt: AdoptScope::none(),
            },
            PushOptions::default(),
            "OK\n",
            &mut sink,
            || {
                a.ctx.watchers.ring_watchers();
                if seqs.is_none() {
                    let mut t = crate::term_lock(&b.term);
                    let before = t.content_seq();
                    let sel = t.text_selection_mut();
                    sel.start_selection(0, 0, SelectionSide::Left, SelectionType::Simple);
                    sel.update_selection(0, 3, SelectionSide::Right);
                    sel.complete_selection();
                    seqs = Some((before, t.content_seq()));
                }
                hang()
            },
        );
        let (before, after) = seqs.expect("REACH: the loop woke and B was selected");
        assert_eq!(before, after, "the selection moves no content_seq");
        assert!(
            sink.hung_up,
            "B's selection reached the wire though A rang before every pass: {:?}",
            sink.wire
        );
    }

    #[test]
    fn only_a_content_stream_arms_its_sessions_holds() {
        use aterm_core::selection::{SelectionSide, SelectionType};
        use aterm_core::terminal::FrameWatchers as _;
        let one = |set: fn(&mut TargetStreams)| {
            let mut streams = TargetStreams::default();
            set(&mut streams);
            streams
        };
        let cases = [
            ("events", one(|s| s.events = true), false),
            ("bytes", one(|s| s.bytes = true), false),
            ("mail", one(|s| s.mail = true), false),
            ("screen", one(|s| s.screen = true), true),
            ("cursor", one(|s| s.cursor = true), true),
            ("cells", one(|s| s.cells = true), true),
        ];
        for (name, streams, reads) in cases {
            let store = crate::session_store::new_store();
            let h = crate::session_store::test_handle(1);
            crate::term_lock(&h.term).process(b"hello");
            store
                .write()
                .unwrap_or_else(|p| p.into_inner())
                .register(h.clone());
            let mut armed = None;
            let mut sink = Scripted::new(|line: &str| {
                if line == "OK" {
                    armed = Some(h.ctx.watchers.watched());
                }
                false
            });
            push_loop_with_peer_probe(
                &store,
                &[target_of(&h)],
                PushScopes {
                    streams,
                    instance: InstanceStreams::default(),
                    adopt: AdoptScope::none(),
                },
                PushOptions::default(),
                "OK\n",
                &mut sink,
                hang_detector("the hang-up on the handshake"),
            );
            assert_eq!(armed, Some(reads), "{name}: the loop's registration");
            assert!(!h.ctx.watchers.watched(), "{name}: gone with the loop");

            let mut sub = Subscription::new();
            let flag = watch_session(&mut sub, &h.ctx.watchers, streams);
            flag.store(false, Ordering::Release);
            {
                let mut t = crate::term_lock(&h.term);
                let sel = t.text_selection_mut();
                sel.start_selection(0, 0, SelectionSide::Left, SelectionType::Simple);
                sel.update_selection(0, 3, SelectionSide::Right);
                sel.complete_selection();
                assert_eq!(
                    matches!(t.frame, crate::FrameHold::Watched(..)),
                    reads,
                    "{name}: the hold reads the frame key"
                );
            }
            assert_eq!(
                flag.load(Ordering::Acquire),
                reads,
                "{name}: the selection rings the registration"
            );
            assert_eq!(sub.wait(Duration::ZERO), reads, "{name}: and wakes it");
        }
    }

    #[test]
    fn a_pass_reads_what_rang_and_what_is_owed() {
        let t0 = Instant::now();
        let events = TargetStreams {
            events: true,
            ..Default::default()
        };
        let both = TargetStreams {
            screen: true,
            events: true,
            ..Default::default()
        };
        let mailed = TargetStreams { mail: true, ..both };
        let read_at_t0 = || {
            let mut pace = Paces::default();
            let _ = pass_streams(both, &mut pace, true, false, t0);
            pace
        };
        let t1 = t0 + CONTENT_PACE;
        let read = |streams, dirty, at| pass_streams(streams, &mut read_at_t0(), dirty, false, at);
        assert_eq!(read(both, true, t1), Some(both), "rang: all");
        assert_eq!(read(events, true, t1), Some(events));
        assert_eq!(read(both, false, t1), None, "it did not ring");
        assert_eq!(read(events, false, t1), None);
        assert_eq!(
            read(both, false, t0 + LIVENESS * 40),
            None,
            "however long since its last read"
        );
        assert_eq!(
            pass_streams(both, &mut Paces::default(), false, false, t0),
            None,
            "nor before its first"
        );
        let mut pace = Paces::default();
        assert_eq!(
            pass_streams(mailed, &mut pace, true, false, t0),
            Some(mailed)
        );
        assert_eq!(
            pass_streams(mailed, &mut pace, true, false, t0 + CONTENT_PACE / 2),
            Some(TargetStreams {
                mail: true,
                ..Default::default()
            }),
            "a paced ring still drains its mail"
        );
        assert_eq!(
            pace.due(),
            Some(t0 + CONTENT_PACE),
            "and owes its content and events reads"
        );
        assert_eq!(
            pass_streams(mailed, &mut pace, false, false, t0 + CONTENT_PACE),
            Some(both),
            "the owed reads come due on a pass it did not ring"
        );
        assert_eq!(
            pass_streams(mailed, &mut pace, false, false, t0 + CONTENT_PACE * 2),
            None,
            "and are read once"
        );

        let mut pace = Paces::default();
        assert_eq!(
            pass_streams(events, &mut pace, true, false, t0),
            Some(events)
        );
        assert_eq!(
            pass_streams(events, &mut pace, true, false, t0 + EVENTS_PACE / 2),
            None,
            "a ring inside the window reads nothing"
        );
        assert_eq!(pace.due(), Some(t0 + EVENTS_PACE), "its read is owed");
        assert_eq!(
            pass_streams(events, &mut pace, false, false, t0 + EVENTS_PACE),
            Some(events),
            "and made when it comes due"
        );

        let mut pace = Paces::default();
        assert_eq!(pass_streams(both, &mut pace, true, false, t0), Some(both));
        let half = t0 + EVENTS_PACE / 2;
        assert_eq!(pass_streams(events, &mut pace, true, false, half), None);
        assert_eq!(
            pass_streams(events, &mut pace, true, true, half),
            Some(events),
            "a record inside the window is read at once"
        );
        assert_eq!(pace.events.due(EVENTS_PACE), None, "and owes nothing");
        assert_eq!(
            pass_streams(both, &mut pace, true, true, half),
            Some(events),
            "its content is not read"
        );
        assert_eq!(
            pace.due(),
            Some(t0 + CONTENT_PACE),
            "but owed, at the content's pace"
        );
        assert_eq!(
            pass_streams(events, &mut pace, false, true, half),
            Some(events),
            "a moved count is read on a pass its ring did not start"
        );
        assert_eq!(
            pass_streams(mailed, &mut pace, false, true, half),
            Some(events),
            "and drains no mail: only a ring does"
        );

        let mut pace = Paces::default();
        let mail = TargetStreams {
            mail: true,
            ..Default::default()
        };
        assert_eq!(pass_streams(mail, &mut pace, true, false, t0), Some(mail));
        assert_eq!(
            pass_streams(mail, &mut pace, true, false, t0 + EVENTS_PACE / 2),
            Some(mail),
            "the negative control: a mail watch drains on that ring"
        );
    }

    #[test]
    fn an_evented_subscription_parks_without_a_timeout() {
        let t0 = Instant::now();
        let term = Arc::new(Mutex::new(Terminal::new(2, 8)));
        let screen = TargetStreams {
            screen: true,
            ..Default::default()
        };
        let events = [watch_on(1, &term)];
        assert_eq!(next_wait(&[], t0, false), None, "no watch, no timer");
        assert_eq!(next_wait(&events, t0, false), None, "an events watch");
        assert_eq!(
            next_wait(&events, t0, true),
            Some(LIVENESS),
            "a peer probed on a timer"
        );
        assert!(
            (|| false).rings().is_none(),
            "and a probe closure is one: it rings nothing"
        );

        let flag = Arc::new(AtomicBool::new(false));
        let cursors = PushCursors {
            roster: None,
            adopt_seq: 0,
            pruned_at: None,
            instance_dirty: Some(flag.clone()),
            reader: None,
        };
        assert!(!membership_owed(&cursors), "a consumed instance flag");
        flag.store(true, Ordering::Release);
        assert!(membership_owed(&cursors), "a held decision left it set");
        assert!(
            !membership_owed(&PushCursors {
                instance_dirty: None,
                ..cursors
            }),
            "no membership to drain"
        );

        let mut content = watch_on(2, &term);
        let _ = pass_streams(screen, &mut content.pace, true, false, t0);
        let content = [content];
        assert_eq!(next_wait(&content, t0, false), None, "a content watch");
        assert_eq!(
            next_wait(&content, t0 + LIVENESS * 40, false),
            None,
            "however long since its last read"
        );
        assert_eq!(
            next_wait(&content, t0, true),
            Some(LIVENESS),
            "a content watch with a peer probed on a timer"
        );
        let mut owed = watch_on(3, &term);
        let _ = pass_streams(screen, &mut owed.pace, true, false, t0);
        let half = CONTENT_PACE / 2;
        let _ = pass_streams(screen, &mut owed.pace, true, false, t0 + half);
        let owed = [owed];
        assert_eq!(
            next_wait(&owed, t0 + half, false),
            Some(CONTENT_PACE - half),
            "an owed read bounds the wait"
        );
        assert_eq!(
            next_wait(&owed, t0 + half, true),
            Some(CONTENT_PACE - half),
            "before the timer"
        );
    }

    struct RingingPeer {
        wakes: Arc<Watchers>,
        gone: Arc<AtomicBool>,
        asked: Arc<std::sync::atomic::AtomicUsize>,
    }

    impl PeerLiveness for RingingPeer {
        fn gone(&mut self) -> bool {
            self.gone.load(Ordering::Acquire)
        }

        fn rings(&self) -> Option<&Arc<Watchers>> {
            self.asked.fetch_add(1, Ordering::AcqRel);
            Some(&self.wakes)
        }
    }

    #[test]
    fn an_events_only_loop_parked_without_a_bound_wakes_on_its_peers_ring() {
        let store = crate::session_store::new_store();
        let h = crate::session_store::test_handle(4);
        let target = target_of(&h);
        store.write().unwrap_or_else(|p| p.into_inner()).register(h);
        let wakes = Arc::<Watchers>::default();
        let gone = Arc::new(AtomicBool::new(false));
        let asked = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let peer = RingingPeer {
            wakes: wakes.clone(),
            gone: gone.clone(),
            asked: asked.clone(),
        };
        let pushed = std::thread::spawn(move || {
            push_loop_with_peer_probe(
                &store,
                &[target],
                PushScopes {
                    streams: TargetStreams {
                        events: true,
                        ..Default::default()
                    },
                    instance: InstanceStreams::default(),
                    adopt: AdoptScope::none(),
                },
                PushOptions::default(),
                "",
                &mut std::io::sink(),
                peer,
            );
        });
        aterm_spec::wait::until_within(
            aterm_spec::wait::MUST_HAPPEN,
            "the loop to ask for its peer's list before its first wait",
            || (asked.load(Ordering::Acquire) >= 2).then_some(()),
        );
        gone.store(true, Ordering::Release);
        wakes.ring_watchers();
        crate::test_socket::join_after_hang_up(
            pushed,
            "the parked events loop to end on its peer's ring",
        );
    }

    const PROBE_TOPIC: &str = "probe";

    const HEARTBEAT: &str = "heartbeat";

    struct Scripted<F: FnMut(&str) -> bool> {
        wire: String,
        partial: String,
        on_line: F,
        hung_up: bool,
    }

    impl<F: FnMut(&str) -> bool> Scripted<F> {
        fn new(on_line: F) -> Self {
            Scripted {
                wire: String::new(),
                partial: String::new(),
                on_line,
                hung_up: false,
            }
        }
    }

    impl<F: FnMut(&str) -> bool> Write for Scripted<F> {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            if self.hung_up {
                return Err(std::io::ErrorKind::BrokenPipe.into());
            }
            let text = String::from_utf8_lossy(buf);
            self.wire.push_str(&text);
            self.partial.push_str(&text);
            while !self.hung_up
                && let Some(nl) = self.partial.find('\n')
            {
                let line: String = self.partial.drain(..=nl).collect();
                self.hung_up = !(self.on_line)(line.trim_end_matches('\n'));
            }
            if self.hung_up {
                Err(std::io::ErrorKind::BrokenPipe.into())
            } else {
                Ok(buf.len())
            }
        }

        fn flush(&mut self) -> std::io::Result<()> {
            if self.hung_up {
                Err(std::io::ErrorKind::BrokenPipe.into())
            } else {
                Ok(())
            }
        }
    }

    fn hang_detector(what: &'static str) -> impl FnMut() -> bool {
        let deadline = std::time::Instant::now() + aterm_spec::wait::MUST_HAPPEN;
        move || {
            assert!(
                std::time::Instant::now() <= deadline,
                "timed out after {:?} waiting for: {what}",
                aterm_spec::wait::MUST_HAPPEN
            );
            false
        }
    }

    pub(super) fn target_of(h: &crate::session_store::SessionHandle) -> ResolvedTarget {
        ResolvedTarget {
            local_id: h.local_id,
            term: h.term.clone(),
            src: WatchSource::of(&h.ctx),
        }
    }

    fn bridge_scopes() -> PushScopes {
        PushScopes {
            streams: TargetStreams {
                events: true,
                ..Default::default()
            },
            instance: InstanceStreams::authorize(
                RequestedInstance { sessions: true },
                Scope::Owner,
            ),
            adopt: AdoptScope::authorize(true, Scope::Owner),
        }
    }

    struct OptInProbe {
        local: u64,
        sid: String,
        ctx: Arc<crate::SessionCtx>,
        reads: Vec<Vec<String>>,
        acked: bool,
        beat: bool,
    }

    impl OptInProbe {
        fn new(h: &crate::session_store::SessionHandle) -> Self {
            OptInProbe {
                local: h.local_id,
                sid: h.sid.as_str().to_string(),
                ctx: h.ctx.clone(),
                reads: Vec::new(),
                acked: false,
                beat: false,
            }
        }

        fn ack_line(&self) -> String {
            format!("sub {} {}", self.local, self.sid)
        }

        fn created_line(&self) -> String {
            format!("EVENT * session-created {}", self.sid)
        }

        fn deferred_line(&self) -> String {
            format!("{} watch=deferred", self.created_line())
        }

        fn read(&mut self) {
            self.reads.push(
                self.ctx
                    .fabric
                    .topics()
                    .into_iter()
                    .map(|(t, _)| t)
                    .collect(),
            );
            if self.reads.len() == 1 {
                assert_eq!(
                    self.ctx
                        .fabric
                        .topic_add(PROBE_TOPIC, "head", &self.ctx.timeline),
                    Ok(true),
                    "a fresh session holds no opt-in yet"
                );
            }
        }

        fn on_line(&mut self, line: &str) -> bool {
            if line == self.ack_line() {
                self.acked = true;
                self.read();
            } else if self.reads.is_empty()
                && (line == self.created_line() || line == self.deferred_line())
            {
                self.read();
            }
            if self.acked && !self.beat {
                self.beat = true;
                self.ctx
                    .timeline
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .record_raw_meta_change(HEARTBEAT.to_string());
            }
            line != format!("EVENT {} meta {HEARTBEAT}", self.local)
        }

        fn learned(&self, wire: &str) -> bool {
            self.reads
                .iter()
                .any(|topics| topics.iter().any(|t| t == PROBE_TOPIC))
                || wire.contains(&format!("EVENT {} topic add {PROBE_TOPIC} ", self.local))
                || wire.contains(&format!("GAP {} events-dropped=", self.local))
        }

        fn order(&self, wire: &str) -> (Option<usize>, Option<usize>, Option<usize>) {
            (
                wire.find(&format!("{}\n", self.ack_line())),
                wire.find(&format!("{}\n", self.created_line())),
                wire.find(&format!("{}\n", self.deferred_line())),
            )
        }
    }

    #[test]
    fn a_spawn_between_adoption_and_the_roster_is_announced_with_its_watch() {
        const T: u64 = 1;
        let store = crate::session_store::new_store();
        store
            .write()
            .unwrap_or_else(|p| p.into_inner())
            .register(crate::session_store::test_handle(T));
        let s = crate::session_store::test_handle(2);
        let mut probe = OptInProbe::new(&s);
        let mut spawn = Some(s);
        let t_ack = format!("sub {T} ");
        let mut sink = Scripted::new(|line: &str| {
            if line.starts_with(&t_ack)
                && let Some(h) = spawn.take()
            {
                store.write().unwrap_or_else(|p| p.into_inner()).register(h);
            }
            probe.on_line(line)
        });
        push_loop_with_peer_probe(
            &store,
            &[],
            bridge_scopes(),
            PushOptions::default(),
            "",
            &mut sink,
            hang_detector("S's heartbeat"),
        );
        let wire = std::mem::take(&mut sink.wire);
        drop(sink);

        assert!(
            spawn.is_none(),
            "REACH: T was adopted, so S was registered mid-wake: {wire:?}"
        );
        assert!(
            !probe.reads.is_empty() && probe.beat,
            "REACH: S was announced, acked and its heartbeat came back: {wire:?}"
        );
        let (ack, announced, deferred) = probe.order(&wire);
        assert!(
            ack.is_some() && ack < announced,
            "S is announced only once its watch exists — its `sub` ack comes first: \
             {wire:?}"
        );
        assert_eq!(deferred, None, "S had a free slot: {wire:?}");
        assert!(
            probe.learned(&wire),
            "the `topic add` made after the bridge's first read reached it by no \
             road — not a later read, not S's watch, no GAP: {wire:?}"
        );
    }

    #[test]
    fn a_deferred_session_is_announced_at_once_and_acked_when_adopted() {
        use crate::control::MAX_SUBSCRIBE_TARGETS as CAP;
        use crate::session_store::{ROSTER_JOURNAL_CAP, test_handle};
        let store = crate::session_store::new_store();
        let mut sub = Subscription::new();
        let term = Arc::new(Mutex::new(Terminal::new(4, 8)));
        let mut watches: Vec<Watch> = (0..CAP as u64)
            .map(|i| watch_on(10_000 + i, &term))
            .collect();
        let mut cursors = PushCursors {
            roster: Some(roster_cursor(&store)),
            adopt_seq: 0,
            pruned_at: None,
            instance_dirty: None,
            reader: None,
        };
        let streams = TargetStreams {
            events: true,
            ..Default::default()
        };
        let opts = PushOptions::default();
        let all = AdoptScope::authorize(true, Scope::Owner);
        let pass = |w: &mut Vec<Watch>, sub: &mut Subscription, c: &mut PushCursors| {
            text(&drain_membership(&store, w, sub, streams, &opts, all, c))
        };
        let spawn = |local: u64| {
            let h = test_handle(local);
            let sid = h.sid.as_str().to_string();
            store.write().unwrap_or_else(|p| p.into_inner()).register(h);
            sid
        };
        let close = |local: u64| {
            store
                .write()
                .unwrap_or_else(|p| p.into_inner())
                .deregister_local(local);
        };

        let a = spawn(1);
        assert_eq!(
            pass(&mut watches, &mut sub, &mut cursors),
            format!("EVENT * session-created {a} watch=deferred\n"),
            "at the cap: no `sub`, and the announcement says so"
        );
        close(1);
        assert_eq!(
            pass(&mut watches, &mut sub, &mut cursors),
            format!("EVENT * session-exited {a} reason=unknown\n"),
            "a deferred session that leaves is announced exiting like any other"
        );

        let c = spawn(3);
        assert_eq!(
            pass(&mut watches, &mut sub, &mut cursors),
            format!("EVENT * session-created {c} watch=deferred\n")
        );
        let d = spawn(4);
        assert_eq!(
            pass(&mut watches, &mut sub, &mut cursors),
            format!("EVENT * session-created {d} watch=deferred\n")
        );
        let e = spawn(5);
        close(4);
        {
            let mut g = store.write().unwrap_or_else(|p| p.into_inner());
            for i in 0..ROSTER_JOURNAL_CAP as u64 {
                g.register(test_handle(100 + i));
                g.deregister_local(100 + i);
            }
            let seq = cursors.roster.as_ref().expect("sessions stream").seq;
            assert!(
                g.roster_low_seq().expect("journal is non-empty") > seq + 1,
                "REACH: the roster cursor is past the retained window"
            );
        }
        assert_eq!(
            pass(&mut watches, &mut sub, &mut cursors),
            format!(
                "EVENT * session-created {e} watch=deferred\n\
                 EVENT * session-exited {d} reason=unknown\n"
            ),
            "recovery marks the newcomer it cannot watch, reports the deferred \
             session that left, and nets the churn to nothing"
        );
        drop(watches.remove(0));
        drop(watches.remove(0));
        let adopted = pass(&mut watches, &mut sub, &mut cursors);
        let mut acks: Vec<&str> = adopted.lines().collect();
        acks.sort_unstable();
        assert_eq!(
            acks,
            [format!("sub 3 {c}"), format!("sub 5 {e}")],
            "the ones still waiting are acked by the pass that adopts them, and \
             not announced again"
        );
    }

    #[test]
    fn a_slot_freed_by_a_prune_runs_the_membership_pass_that_adopts() {
        use crate::control::MAX_SUBSCRIBE_TARGETS as CAP;
        use crate::session_store::test_handle;
        let store = crate::session_store::new_store();
        let handles: Vec<crate::session_store::SessionHandle> =
            (0..CAP as u64).map(test_handle).collect();
        {
            let mut g = store.write().unwrap_or_else(|p| p.into_inner());
            for h in &handles {
                g.register(h.clone());
            }
        }
        let streams = TargetStreams {
            events: true,
            ..Default::default()
        };
        let opts = PushOptions::default();
        let all = AdoptScope::authorize(true, Scope::Owner);
        let mut sub = Subscription::new();
        let mut watches: Vec<Watch> = handles
            .iter()
            .map(|h| {
                let mut w = new_watch(&target_of(h), streams, &opts);
                w.dirty = sub.watch(&h.ctx.watchers);
                w
            })
            .collect();
        let instance = store
            .read()
            .unwrap_or_else(|p| p.into_inner())
            .instance_watchers();
        let rang = Subscription::single(&instance);
        let flag = sub.watch(&instance);
        let mut cursors = PushCursors {
            roster: None,
            adopt_seq: 0,
            pruned_at: None,
            instance_dirty: Some(flag.clone()),
            reader: None,
        };
        let prune = |w: &mut Vec<Watch>, sub: &mut Subscription, c: &mut PushCursors| {
            let closed = prune_closed(&store, w, c);
            let n = closed.len();
            for closing in closed {
                sub.unwatch(closing.watchers());
                let _ = closing.drain(streams);
            }
            n
        };
        let membership = |w: &mut Vec<Watch>, sub: &mut Subscription, c: &mut PushCursors| {
            text(&drain_membership(&store, w, sub, streams, &opts, all, c))
        };
        let s = test_handle(CAP as u64);
        let s_sid = s.sid.as_str().to_string();
        store.write().unwrap_or_else(|p| p.into_inner()).register(s);

        assert_eq!(prune(&mut watches, &mut sub, &mut cursors), 0);
        assert_eq!(
            membership(&mut watches, &mut sub, &mut cursors),
            "",
            "at the cap S is deferred"
        );
        assert_eq!(prune(&mut watches, &mut sub, &mut cursors), 0);
        store
            .write()
            .unwrap_or_else(|p| p.into_inner())
            .deregister_local(0);
        assert_eq!(
            membership(&mut watches, &mut sub, &mut cursors),
            "",
            "X left after the prune: its watch holds the slot"
        );
        assert!(
            !flag.load(Ordering::Acquire),
            "and that pass took the ring of X's exit"
        );
        let _ = rang.wait(Duration::ZERO);
        assert_eq!(
            prune(&mut watches, &mut sub, &mut cursors),
            1,
            "the next pass's prune retires X"
        );
        assert!(
            !rang.wait(Duration::ZERO),
            "nothing rang the instance list since that pass"
        );
        assert_eq!(
            membership(&mut watches, &mut sub, &mut cursors),
            format!("sub {CAP} {s_sid}\n"),
            "the membership pass after the prune adopts S"
        );
    }

    #[test]
    fn a_handshake_ack_is_written_after_its_watch_is_seeded() {
        let store = crate::session_store::new_store();
        let s = crate::session_store::test_handle(1);
        let (local, sid, ctx) = (s.local_id, s.sid.as_str().to_string(), s.ctx.clone());
        store.write().unwrap_or_else(|p| p.into_inner()).register(s);
        let active: crate::control::ActiveHandle = Arc::new(Mutex::new(None));
        let ack = format!("sub {local} {sid}");
        let pushed = format!("EVENT {local} topic add {PROBE_TOPIC} ");
        let mut added = false;
        let mut sink = Scripted::new(|line: &str| {
            if line == ack && !added {
                added = true;
                assert_eq!(
                    ctx.fabric.topic_add(PROBE_TOPIC, "head", &ctx.timeline),
                    Ok(true),
                    "a fresh session holds no opt-in yet"
                );
            }
            !line.starts_with(&pushed)
        });
        crate::control::run_subscribe_with_peer_probe(
            aterm_types::control_request::head("subscribe @* events,sessions"),
            &active,
            &store,
            Scope::Owner,
            &mut sink,
            hang_detector("the add made on S's handshake ack"),
        );
        let wire = std::mem::take(&mut sink.wire);
        drop(sink);

        assert!(added, "REACH: the handshake acked S: {wire:?}");
        assert!(
            wire.contains(&pushed),
            "the add made on S's handshake ack was below its watch's seed, so the watch \
             never pushed it: {wire:?}"
        );
    }

    #[cfg(unix)]
    #[derive(Debug)]
    struct SeenAtClose {
        count: usize,
        master_open: bool,
        wire: String,
    }

    #[cfg(unix)]
    fn master_open_now(peer: &std::os::unix::net::UnixStream) -> bool {
        use std::io::Read as _;
        peer.set_nonblocking(true).expect("nonblocking peer");
        let open = matches!(
            (&*peer).read(&mut [0_u8; 1]),
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock
        );
        peer.set_nonblocking(false).expect("blocking peer");
        open
    }

    #[cfg(unix)]
    fn master_closes(peer: &std::os::unix::net::UnixStream) -> bool {
        use std::io::Read as _;
        matches!((&*peer).read(&mut [0_u8; 1]), Ok(0))
    }

    #[cfg(unix)]
    fn close_under_subscription(
        pin: bool,
    ) -> (
        SeenAtClose,
        Option<Arc<crate::SessionCtx>>,
        std::os::unix::net::UnixStream,
    ) {
        let store = crate::session_store::new_store();
        let (peer, master) = std::os::unix::net::UnixStream::pair().expect("socketpair");
        peer.set_read_timeout(Some(aterm_spec::wait::MUST_HAPPEN))
            .expect("read timeout");
        let h = crate::session_store::test_handle_owning(1, master.into());
        let (local, sid) = (h.local_id, h.sid.as_str().to_string());
        let mut ctx = Some(h.ctx.clone());
        let pinned = pin.then(|| h.ctx.clone());
        store.write().unwrap_or_else(|p| p.into_inner()).register(h);
        let active: crate::control::ActiveHandle = Arc::new(Mutex::new(None));
        let (ack, exited) = (
            format!("sub {local} {sid}"),
            format!("EVENT {local} exited"),
        );
        let mut seen: Option<(usize, bool)> = None;
        let mut sink = Scripted::new(|line: &str| {
            if line == ack {
                store
                    .write()
                    .unwrap_or_else(|p| p.into_inner())
                    .deregister_local_as(
                        local,
                        crate::session_store::ExitReason::CtlClose,
                        crate::session_store::ExitActor::Sid("s-boss".into()),
                    );
                let ctx = ctx.take().expect("one handshake ack");
                let count = Arc::strong_count(&ctx);
                drop(ctx);
                let open = if pin {
                    master_open_now(&peer)
                } else {
                    !master_closes(&peer)
                };
                seen = Some((count, open));
            }
            line != exited
        });
        crate::control::run_subscribe_with_peer_probe(
            aterm_types::control_request::head("subscribe @* screen,cells,events,bytes,sessions"),
            &active,
            &store,
            Scope::Owner,
            &mut sink,
            hang_detector("the closed tab's `exited`"),
        );
        let wire = std::mem::take(&mut sink.wire);
        drop(sink);
        let (count, master_open) = seen.expect("REACH: the handshake acked the tab");
        (
            SeenAtClose {
                count,
                master_open,
                wire,
            },
            pinned,
            peer,
        )
    }

    #[cfg(unix)]
    #[test]
    fn a_closed_tabs_master_closes_under_a_live_subscription() {
        let (seen, _, _peer) = close_under_subscription(false);
        assert_eq!(
            seen.count, 1,
            "the subscription holds the closed tab's context, and with it its master: {seen:?}"
        );
        assert!(
            !seen.master_open,
            "the closed tab's master is open under the subscription: {seen:?}"
        );
        assert!(
            seen.wire
                .contains("EVENT 1 closing reason=ctl-close by=s-boss\nEVENT 1 exited\n"),
            "the subscription delivers the closed tab's tail: {seen:?}"
        );

        let (seen, pinned, peer) = close_under_subscription(true);
        assert_eq!(seen.count, 2, "the pin holds the context: {seen:?}");
        assert!(
            seen.master_open,
            "a held context keeps the master open: {seen:?}"
        );
        drop(pinned);
        assert!(master_closes(&peer), "the pin was the master's last holder");
    }

    #[test]
    fn an_at_star_subscription_over_the_cap_watches_the_cap_and_defers_the_rest() {
        use crate::control::MAX_SUBSCRIBE_TARGETS as CAP;
        let store = crate::session_store::new_store();
        let mut sids = std::collections::BTreeMap::new();
        {
            let mut g = store.write().unwrap_or_else(|p| p.into_inner());
            for local in 0..=CAP as u64 {
                let h = crate::session_store::test_handle(local);
                sids.insert(local, h.sid.as_str().to_string());
                g.register(h);
            }
        }
        let active: crate::control::ActiveHandle = Arc::new(Mutex::new(None));
        let mut acked: Vec<u64> = Vec::new();
        let mut left_over: Option<u64> = None;
        let mut sink = Scripted::new(|line: &str| {
            let Some(local) = line
                .strip_prefix("sub ")
                .and_then(|rest| rest.split_once(' '))
                .and_then(|(l, _)| l.parse::<u64>().ok())
            else {
                return true;
            };
            if Some(local) == left_over {
                return false;
            }
            acked.push(local);
            if acked.len() == CAP {
                left_over = sids.keys().copied().find(|l| !acked.contains(l));
                store
                    .write()
                    .unwrap_or_else(|p| p.into_inner())
                    .deregister_local(acked[0]);
            }
            true
        });
        crate::control::run_subscribe_with_peer_probe(
            aterm_types::control_request::head("subscribe @* events,sessions"),
            &active,
            &store,
            Scope::Owner,
            &mut sink,
            hang_detector("the left-over session's ack"),
        );
        let wire = std::mem::take(&mut sink.wire);
        drop(sink);

        assert!(
            wire.starts_with(&format!("OK subscribe {CAP}\n")),
            "a `@*` over the cap watches the cap: {:?}",
            wire.lines().next()
        );
        assert_eq!(acked.len(), CAP, "every handshake target is acked");
        let left_over = left_over.expect("REACH: one session was left over at the cap");
        assert!(
            wire.contains(&format!("sub {left_over} {}\n", sids[&left_over])),
            "the session the cap left over is acked once a slot frees"
        );
    }

    #[test]
    fn membership_passes_refine_the_announcement_model() {
        use aterm_spec::interp::State;
        let model = aterm_models_session::subscribe_announcement_order_model();
        const LAW: &str = "NotAnnouncedBeforeItsWatchOrExit";

        let admits =
            |overrides: &[(&'static str, i64)], prev: &State, next: &State, action: &str| {
                aterm_spec::verify::validate_transition_tiered(
                    &model,
                    overrides,
                    prev,
                    next,
                    Some(action),
                    &format!("subscribe announcement {action}"),
                )
            };
        let step = |prev: &State, next: &State, action: &str| {
            let (ok, why) = admits(&[], prev, next, action);
            assert!(
                ok,
                "the real {action} is not the model's: {prev:?} -> {next:?}\n{why}"
            );
            assert!(model.check_invariant(LAW, next), "{LAW} fails at {next:?}");
        };

        #[derive(Clone, Copy, Debug)]
        enum Move {
            Register,
            Exit,
            FreeSlot,
            Pass,
            RegisterDuringPass,
        }
        let (reg, exit, free, pass, reg_mid) = (
            Move::Register,
            Move::Exit,
            Move::FreeSlot,
            Move::Pass,
            Move::RegisterDuringPass,
        );
        let scenarios: &[&[Move]] = &[
            &[free, reg, pass, pass],
            &[reg, pass, free, pass, pass],
            &[reg, pass, exit, pass],
            &[free, reg, exit, pass],
            &[reg, exit, pass],
            &[free, reg_mid, pass, exit, pass],
            &[reg_mid, pass, free, pass],
        ];
        let mut lines_seen = std::collections::BTreeSet::new();
        for (i, moves) in scenarios.iter().enumerate() {
            let mut w = AnnounceWorld::new();
            let mut state = w.project();
            assert_eq!(state, model.init_state(), "scenario {i} starts at Init");
            for mv in moves.iter().copied() {
                let prev = state.clone();
                match mv {
                    Move::Register => {
                        w.register();
                        state = w.project();
                        step(&prev, &state, "Register");
                    }
                    Move::Exit => {
                        w.exit();
                        state = w.project();
                        step(&prev, &state, "Exit");
                    }
                    Move::FreeSlot => {
                        w.free_slot();
                        state = w.project();
                        step(&prev, &state, "FreeSlot");
                    }
                    Move::Pass | Move::RegisterDuringPass => {
                        let (picked, line) = w.pass(|w| {
                            if matches!(mv, Move::RegisterDuringPass) {
                                w.register();
                            }
                        });
                        lines_seen.insert(line);
                        let after = w.project();
                        let mut s1 = prev.clone();
                        s1.insert("phase", 1);
                        s1.insert("snap_reg", prev["reg"]);
                        s1.insert("snap_live", prev["live"]);
                        s1.insert("picked", picked);
                        step(&prev, &s1, "Read");
                        let mut s2 = s1.clone();
                        s2.insert("phase", 2);
                        s2.insert("line", line);
                        s2.insert("drained", after["drained"]);
                        step(&s1, &s2, "Drain");
                        let mut s3 = s2.clone();
                        s3.insert("phase", 3);
                        s3.insert("watched", after["watched"]);
                        step(&s2, &s3, "Seed");
                        if matches!(mv, Move::RegisterDuringPass) {
                            let mut registered = s3.clone();
                            registered.insert("reg", 1);
                            registered.insert("live", 1);
                            step(&s3, &registered, "Register");
                            s3 = registered;
                        }
                        let mut s4 = s3.clone();
                        s4.insert("phase", 0);
                        s4.insert("line", 0);
                        s4.insert("snap_reg", 0);
                        s4.insert("snap_live", 0);
                        s4.insert("picked", 0);
                        if line != 0 {
                            s4.insert("told", line);
                        }
                        if picked == 1 {
                            s4.insert("ackwire", 1);
                        }
                        step(&s3, &s4, "Write");
                        assert_eq!(s4, after, "scenario {i}: the pass left what it decided");
                        state = after;
                    }
                }
            }
        }
        assert_eq!(
            lines_seen,
            std::collections::BTreeSet::from([0, 1, 2]),
            "the scenarios reach every line a pass can write for S"
        );

        for (label, room, register_between) in [
            ("between the reads", true, true),
            ("at the cap", false, false),
        ] {
            let mut w = AnnounceWorld::new();
            if room {
                w.free_slot();
            }
            if !register_between {
                w.register();
            }
            let s0 = w.project();
            let (picked, line) = w.two_read_pass(|w| {
                if register_between {
                    w.register();
                }
            });
            let after = w.project();
            assert_eq!(
                (picked, line, after["watched"]),
                (0, 1, 0),
                "{label}: the two-read pass announces S plainly and adopts nothing"
            );
            let mut s1 = s0.clone();
            s1.insert("phase", 1);
            s1.insert("snap_reg", s0["reg"]);
            s1.insert("snap_live", s0["live"]);
            s1.insert("picked", 0);
            if register_between {
                s1.insert("reg", 1);
                s1.insert("live", 1);
            }
            let mut s2 = s1.clone();
            s2.insert("phase", 2);
            s2.insert("line", line);
            s2.insert("drained", after["drained"]);
            let (healthy, _) = admits(&[], &s1, &s2, "Drain");
            assert!(
                !healthy,
                "{label}: the healthy machine admitted the two-read roster"
            );
            let (old, why) = admits(&[("Buggy", 1)], &s1, &s2, "Drain");
            assert!(
                old,
                "{label}: Buggy=1 must reproduce the two-read roster\n{why}"
            );
            let buggy = aterm_spec::interp::with_buggy(&model, 1);
            let mut end = s2.clone();
            assert!(buggy.fire("Seed", &mut end) && buggy.fire("Write", &mut end));
            assert_eq!(end, after, "{label}: Buggy=1 lands where the real pass did");
            assert!(
                !buggy.check_invariant(LAW, &end),
                "{label}: S told it is watched, with no watch and no exit: {end:?}"
            );
        }
    }

    struct AnnounceWorld {
        store: Store,
        sub: Subscription,
        watches: Vec<Watch>,
        cursors: PushCursors,
        handle: crate::session_store::SessionHandle,
        sid: String,
        created: Option<u64>,
        told: i64,
        ack_written: bool,
    }

    impl AnnounceWorld {
        const S: u64 = 7;

        fn new() -> Self {
            use crate::control::MAX_SUBSCRIBE_TARGETS as CAP;
            let store = crate::session_store::new_store();
            let sub = Subscription::new();
            let term = Arc::new(Mutex::new(Terminal::new(4, 8)));
            let watches = (0..CAP as u64)
                .map(|i| watch_on(10_000 + i, &term))
                .collect();
            let cursors = PushCursors {
                roster: Some(roster_cursor(&store)),
                adopt_seq: 0,
                pruned_at: None,
                instance_dirty: None,
                reader: None,
            };
            let handle = crate::session_store::test_handle(Self::S);
            AnnounceWorld {
                store,
                sub,
                watches,
                cursors,
                sid: handle.sid.as_str().to_string(),
                handle,
                created: None,
                told: 0,
                ack_written: false,
            }
        }

        fn register(&mut self) {
            let mut g = self.store.write().unwrap_or_else(|p| p.into_inner());
            g.register(self.handle.clone());
            self.created = Some(g.roster_seq());
        }

        fn exit(&mut self) {
            self.store
                .write()
                .unwrap_or_else(|p| p.into_inner())
                .deregister_local(Self::S);
        }

        fn free_slot(&mut self) {
            assert_ne!(self.watches[0].local_id, Self::S);
            drop(self.watches.remove(0));
        }

        fn watched(&self) -> bool {
            self.watches.iter().any(|w| w.local_id == Self::S)
        }

        fn project(&self) -> aterm_spec::interp::State {
            use crate::control::MAX_SUBSCRIBE_TARGETS as CAP;
            let g = self.store.read().unwrap_or_else(|p| p.into_inner());
            let live = g
                .by_sid(&aterm_session::SessionId::new(&self.sid))
                .is_some();
            let seq = self.cursors.roster.as_ref().expect("sessions stream").seq;
            let watched = self.watched();
            [
                ("reg", i64::from(self.created.is_some())),
                ("live", i64::from(live)),
                ("room", i64::from(watched || self.watches.len() < CAP)),
                ("phase", 0),
                ("snap_reg", 0),
                ("snap_live", 0),
                ("picked", 0),
                ("line", 0),
                ("drained", i64::from(self.created.is_some_and(|c| seq >= c))),
                ("watched", i64::from(watched)),
                ("told", self.told),
                ("ackwire", i64::from(self.ack_written)),
                ("added", 0),
                ("pushed", 0),
                ("listed", 0),
                ("acked", 0),
                ("asking", 0),
                ("seen", 0),
                ("known", 0),
                ("stopped", 0),
            ]
            .into()
        }

        fn decided(&mut self, frames: &str) -> (i64, i64) {
            let picked = frames.contains(&format!("sub {} {}\n", Self::S, self.sid));
            self.ack_written |= picked;
            let created = format!("EVENT * session-created {}", self.sid);
            let line = if frames.contains(&format!("{created} watch=deferred\n")) {
                2
            } else if frames.contains(&format!("{created}\n")) {
                1
            } else {
                0
            };
            if line != 0 {
                self.told = line;
            }
            (i64::from(picked), line)
        }

        fn pass(&mut self, before_write: impl FnOnce(&mut Self)) -> (i64, i64) {
            let frames = text(&drain_membership(
                &self.store,
                &mut self.watches,
                &mut self.sub,
                TargetStreams {
                    events: true,
                    ..Default::default()
                },
                &PushOptions::default(),
                AdoptScope::authorize(true, Scope::Owner),
                &mut self.cursors,
            ));
            before_write(self);
            self.decided(&frames)
        }

        fn two_read_pass(&mut self, between: impl FnOnce(&mut Self)) -> (i64, i64) {
            let picked = {
                let g = self.store.read().unwrap_or_else(|p| p.into_inner());
                pick_adoptions(&g, &self.watches, &mut self.cursors.adopt_seq)
            };
            between(self);
            let mut roster = String::new();
            {
                let g = self.store.read().unwrap_or_else(|p| p.into_inner());
                let cursor = self.cursors.roster.as_mut().expect("sessions stream");
                drain_roster(&g, cursor, None, &mut roster);
            }
            let mut frames = String::new();
            for (target, sid) in picked {
                self.sub.watch(&target.src.watchers);
                frames.push_str(&format!("sub {} {sid}\n", target.local_id));
                self.watches.push(new_watch(
                    &target,
                    TargetStreams {
                        events: true,
                        ..Default::default()
                    },
                    &PushOptions::default(),
                ));
            }
            frames.push_str(&roster);
            self.decided(&frames)
        }
    }
}

#[cfg(test)]
mod log_stream_tests {
    use super::*;

    fn log_watch(
        timeline: &Arc<Mutex<crate::session_timeline::SessionTimeline>>,
        cursor: Option<aterm_slog::Cursor>,
        filter: LogFilter,
    ) -> (Watch, TargetStreams) {
        let streams = TargetStreams {
            log: true,
            ..TargetStreams::default()
        };
        let term = Arc::new(Mutex::new(Terminal::new(24, 80)));
        let target = ResolvedTarget {
            local_id: 1,
            term,
            src: WatchSource {
                byte_fanout: Arc::new(ByteFanout::new()),
                turns: Arc::new(Mutex::new(TurnLedger::default())),
                timeline: timeline.clone(),
                fabric: Arc::default(),
                frame: Arc::default(),
                watchers: Arc::default(),
            },
        };
        let opts = PushOptions {
            log: filter,
            log_since: cursor,
            ..PushOptions::default()
        };
        (new_watch(&target, streams, &opts), streams)
    }

    fn text(frames: &[Frame]) -> String {
        frames
            .iter()
            .flat_map(|f| &f.segs)
            .map(|seg| String::from_utf8_lossy(seg.bytes()).into_owned())
            .collect()
    }

    fn timeline() -> Arc<Mutex<crate::session_timeline::SessionTimeline>> {
        let tl = Arc::new(Mutex::new(
            crate::session_timeline::SessionTimeline::for_test(),
        ));
        {
            let mut g = tl.lock().unwrap();
            let now = g.slog().now();
            g.slog_mut().open_session(None, None, now);
        }
        tl
    }

    fn mark(
        tl: &Mutex<crate::session_timeline::SessionTimeline>,
        kind: aterm_slog::Kind,
        src: aterm_slog::Src,
    ) {
        let mut g = tl.lock().unwrap();
        let now = g.slog().now();
        g.slog_mut()
            .log_mut_for_journal()
            .mark(
                aterm_slog::Mark::new(kind, src, aterm_slog::Conf::Strong),
                now,
            )
            .expect("a mark");
    }

    #[test]
    fn a_log_streams_sub_line_names_where_it_starts() {
        let tl = timeline();
        mark(&tl, aterm_slog::Kind::Bell, aterm_slog::Src::Engine);
        let (live, _) = log_watch(&tl, None, LogFilter::default());
        let ack = "OK subscribe 1\nsub 1 s-aaaa\n";
        assert_eq!(
            with_log_anchors(ack, std::slice::from_ref(&live)),
            "OK subscribe 1\nsub 1 s-aaaa log_seq=2 log_ep=1\n"
        );
        let (resumed, _) = log_watch(
            &tl,
            Some(aterm_slog::Cursor::new(1, 1)),
            LogFilter::default(),
        );
        assert_eq!(
            with_log_anchors(ack, std::slice::from_ref(&resumed)),
            "OK subscribe 1\nsub 1 s-aaaa log_seq=1 log_ep=1\n"
        );
        let mut quiet = live;
        quiet.log = None;
        assert_eq!(with_log_anchors(ack, std::slice::from_ref(&quiet)), ack);
    }

    #[test]
    fn the_log_stream_parses_and_fails_closed() {
        for (spec, lvl, class) in [
            ("log", None, false),
            ("log:lvl=turn", Some(aterm_slog::Lvl::Turn), false),
            ("log:class=turn", None, true),
            ("log:lvl=run:class=turn", Some(aterm_slog::Lvl::Run), true),
            ("log:class=turn:lvl=step", Some(aterm_slog::Lvl::Step), true),
        ] {
            let req = Requested::parse(spec).unwrap_or_else(|| panic!("{spec}"));
            assert!(req.targets.log, "{spec}");
            assert_eq!(
                req.log,
                LogFilter {
                    lvl,
                    class_turn: class
                },
                "{spec}"
            );
        }
        for bad in [
            "log:lvl=page",
            "log:class=step",
            "log:lvl=turn:lvl=run",
            "log:foo=1",
            "log,log",
            "logs",
            "log:",
        ] {
            assert!(Requested::parse(bad).is_none(), "{bad} must fail closed");
        }
        let both = Requested::parse("mail:kinds=task,log:class=turn").expect("two streams");
        assert!(both.targets.mail && both.targets.log);
        assert_eq!(both.mail.kinds, vec!["task".to_string()]);
        assert!(both.log.class_turn);
        assert!(aterm_types::control_verbs::SUBSCRIBE_STREAMS.contains(&"log"));
    }

    #[test]
    fn a_log_watch_is_live_from_the_head_and_replays_from_since_log() {
        let tl = timeline();
        mark(&tl, aterm_slog::Kind::Bell, aterm_slog::Src::Engine);
        let (mut live, streams) = log_watch(&tl, None, LogFilter::default());
        assert!(
            frames_for_watch(&mut live, streams, true).is_empty(),
            "no backlog"
        );
        mark(&tl, aterm_slog::Kind::Bell, aterm_slog::Src::Engine);
        mark(&tl, aterm_slog::Kind::Cwd, aterm_slog::Src::Engine);
        let got = text(&frames_for_watch(&mut live, streams, true));
        let lines: Vec<&str> = got.lines().collect();
        assert_eq!(lines.len(), 2, "{got}");
        assert!(
            lines[0].starts_with("LOG 1 3 mark - bell ")
                && lines[1].starts_with("LOG 1 4 mark - cwd "),
            "{got}"
        );
        assert!(
            frames_for_watch(&mut live, streams, false).is_empty(),
            "idle"
        );
        let ep = tl.lock().unwrap().slog().log().ep();
        let (mut resumed, streams) = log_watch(
            &tl,
            Some(aterm_slog::Cursor::new(ep, 1)),
            LogFilter::default(),
        );
        let got = text(&frames_for_watch(&mut resumed, streams, true));
        assert_eq!(got.lines().count(), 3, "seqs 2..=4: {got}");
        assert!(got.starts_with("LOG 1 2 mark - bell "), "{got}");
    }

    #[test]
    fn a_program_mark_streams_as_a_log_frame() {
        use aterm_core::terminal::program_state::{ProgramState, ProgramStateWord};
        let tl = timeline();
        let (mut all, streams) = log_watch(&tl, None, LogFilter::default());
        let (mut turns, turn_streams) = log_watch(
            &tl,
            None,
            LogFilter {
                lvl: None,
                class_turn: true,
            },
        );
        let _ = tl.lock().unwrap().note_program_state(Some(ProgramState {
            state: ProgramStateWord::Working,
            kind: None,
        }));
        let got = text(&frames_for_watch(&mut all, streams, true));
        assert!(
            got.lines().any(|l| l.starts_with("LOG 1 ")
                && l.contains(" mark - program state=working src=engine conf=claim ")),
            "{got}"
        );
        let quiet = text(&frames_for_watch(&mut turns, turn_streams, true));
        assert!(!quiet.contains(" program "), "{quiet}");
    }

    #[test]
    fn the_log_gap_is_the_reads_exact_count() {
        let tl = timeline();
        let ep = tl.lock().unwrap().slog().log().ep();
        for _ in 0..200 {
            mark(&tl, aterm_slog::Kind::Bell, aterm_slog::Src::Engine);
        }
        let (mut clean, streams) = log_watch(
            &tl,
            Some(aterm_slog::Cursor::new(ep, 1)),
            LogFilter::default(),
        );
        assert!(!text(&frames_for_watch(&mut clean, streams, true)).contains("GAP"));
        tl.lock().unwrap().slog_mut().set_log_byte_budget(2048);
        let lost = tl.lock().unwrap().slog().log().lost();
        assert!(lost > 0);
        let (mut behind, streams) = log_watch(
            &tl,
            Some(aterm_slog::Cursor::new(ep, 1)),
            LogFilter::default(),
        );
        let got = text(&frames_for_watch(&mut behind, streams, true));
        assert!(
            got.starts_with(&format!("GAP 1 log-lost={lost} why=evicted resync_to=")),
            "{got}"
        );
        let (mut reset, streams) = log_watch(
            &tl,
            Some(aterm_slog::Cursor::new(ep + 7, 3)),
            LogFilter::default(),
        );
        assert!(text(&frames_for_watch(&mut reset, streams, true)).starts_with("GAP 1 log-lost="));
        let (mut reset2, streams) = log_watch(
            &tl,
            Some(aterm_slog::Cursor::new(ep + 7, 3)),
            LogFilter::default(),
        );
        assert!(text(&frames_for_watch(&mut reset2, streams, true)).contains(" why=reset "));
        let head = tl.lock().unwrap().slog().log().last_seq();
        let (mut across, streams) = log_watch(
            &tl,
            Some(aterm_slog::Cursor::new(ep, head)),
            LogFilter::default(),
        );
        {
            let mut g = tl.lock().unwrap();
            g.slog_mut().log_mut_for_journal().skip_to(head + 50);
        }
        mark(&tl, aterm_slog::Kind::Bell, aterm_slog::Src::Engine);
        let got = text(&frames_for_watch(&mut across, streams, true));
        assert!(got.starts_with("GAP 1 log-lost=0 why=handoff "), "{got}");
        assert!(
            got.lines().nth(1).is_some_and(|l| l.starts_with("LOG 1 ")),
            "{got}"
        );
    }

    #[test]
    fn the_log_filter_is_the_log_verbs_and_the_tier_is_hosts() {
        let tl = timeline();
        let (mut runs, streams) = log_watch(
            &tl,
            None,
            LogFilter {
                lvl: Some(aterm_slog::Lvl::Run),
                class_turn: false,
            },
        );
        let (mut class, _) = log_watch(
            &tl,
            None,
            LogFilter {
                lvl: None,
                class_turn: true,
            },
        );
        let (mut all, _) = log_watch(&tl, None, LogFilter::default());
        {
            let mut g = tl.lock().unwrap();
            let now = g.slog().now();
            let log = g.slog_mut().log_mut_for_journal();
            let run = log
                .open(
                    aterm_slog::Open::new(
                        aterm_slog::Kind::Agent,
                        aterm_slog::Src::Status,
                        aterm_slog::Conf::Strong,
                    ),
                    now,
                )
                .expect("run");
            let turn = log
                .open(
                    aterm_slog::Open::new(
                        aterm_slog::Kind::Turn,
                        aterm_slog::Src::Screen,
                        aterm_slog::Conf::Strong,
                    )
                    .under(run),
                    now,
                )
                .expect("turn");
            log.mark(
                aterm_slog::Mark::new(
                    aterm_slog::Kind::Title,
                    aterm_slog::Src::Engine,
                    aterm_slog::Conf::Exact,
                )
                .at(turn)
                .fields(
                    aterm_slog::Fields::new()
                        .text(aterm_slog::Key::Title, "SECRET TITLE")
                        .expect("title"),
                ),
                now,
            )
            .expect("title");
        }
        let runs_got = text(&frames_for_watch(&mut runs, streams, true));
        assert!(
            runs_got.contains(" open r1 agent ") && !runs_got.contains(" open t1 turn "),
            "{runs_got}"
        );
        let class_got = text(&frames_for_watch(&mut class, streams, true));
        assert!(
            class_got.contains(" open t1 turn ") && !class_got.contains(" title"),
            "{class_got}"
        );
        let all_got = text(&frames_for_watch(&mut all, streams, true));
        assert!(all_got.contains(" mark t1 title "), "{all_got}");
        assert!(
            !all_got.contains("SECRET"),
            "never the content tier: {all_got}"
        );
        let log = tl.lock().unwrap().slog().log().clone();
        let q = crate::session_log::LogQuery::parse("lvl=run").expect("q");
        let verb = crate::session_log::render_log(&log, &q);
        assert_eq!(
            verb.lines()
                .skip(1)
                .filter(|l| l.contains(" open "))
                .count(),
            runs_got.lines().filter(|l| l.contains(" open ")).count() + 1,
            "the verb also shows the session's open the live stream started after"
        );
    }

    #[test]
    fn a_closed_sessions_close_record_precedes_exited() {
        let store = crate::session_store::new_store();
        let h = crate::session_store::test_handle(9);
        let target = super::tests::target_of(&h);
        store.write().unwrap_or_else(|p| p.into_inner()).register(h);
        let streams = TargetStreams {
            events: true,
            log: true,
            ..Default::default()
        };
        let mut sink: Vec<u8> = Vec::new();
        let mut closed = false;
        let close_once = || {
            if !closed {
                closed = true;
                store
                    .write()
                    .unwrap_or_else(|p| p.into_inner())
                    .deregister_local_as(
                        9,
                        crate::session_store::ExitReason::CtlClose,
                        crate::session_store::ExitActor::Ctl,
                    );
            }
            false
        };
        push_loop_with_peer_probe(
            &store,
            &[target],
            PushScopes {
                streams,
                instance: InstanceStreams::default(),
                adopt: AdoptScope::none(),
            },
            PushOptions::default(),
            "",
            &mut sink,
            close_once,
        );
        let out = String::from_utf8_lossy(&sink).into_owned();
        let close = out
            .find("LOG 9 2 close - spawn out=ctl-close")
            .unwrap_or_else(|| panic!("{out}"));
        let exited = out
            .find("EVENT 9 exited")
            .unwrap_or_else(|| panic!("{out}"));
        assert!(close < exited, "{out}");
    }
}

#[cfg(test)]
#[path = "subscribe_wire_goldens_tests.rs"]
mod wire_goldens;
