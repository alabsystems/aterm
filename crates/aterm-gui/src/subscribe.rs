// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Real-time server-push layer over the control socket.

use std::io::{IoSlice, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use aterm_core::terminal::Terminal;
use aterm_types::MutexExt as _;

use crate::cast::{ByteFanout, ByteSubscription};
use crate::control::Scope;
use crate::frame_key::{FrameKey, TextKey};
use crate::frame_memo::{
    CellsHold, FrameSlot, MAX_CELLS_WATCHES, STYLED_FRAME_CAP, ScreenText, Styled, StyledFrame,
};
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
    /// The lifecycle digest: `EVENT <sid> block-complete <id> exit=<code>` on OSC 133 D, `EVENT <sid> turn <id> submitted=<0|1> status=<..> dur_ms=<..>` per completed turn (the turn ledger), `EVENT <sid> title <pct>` on an OSC 0/2 title change, `EVENT <sid> bell total=<n>` on a BEL, the timeline kinds [`timeline_wire_kind`] carries (`meta field=<f> value=<pct|->`, `agent`, `status`, `human`, `render`, a program's own `program state=<w>[ kind=<k>] rev=<id>` and `act <act> rev=<id>`, the fabric digest), and at retirement `EVENT <sid> closing reason=<token> by=<sid|human|ctl|bridge|harness|->` then `EVENT <sid> exited`.
    pub events: bool,
    /// Emit `DELTA <sid> seq=<n> cells <nbytes>\n<styled-json>` when the styled frame changes — the LOSSLESS styled-screen frame (Item 1's payload) pushed live, so an outer agent sees the inner TUI's colour/attrs, not plaintext. A frame past [`STYLED_FRAME_CAP`] is never built or sent: `GAP <sid> cells-too-large=<bytes> cap=<cap>` says so once, until a frame fits again; a session already serving [`MAX_CELLS_WATCHES`] gives a further watch no cells, said once as `GAP <sid> cells-watches=<cap>`.
    pub cells: bool,
    /// Emit `BYTES <sid> <len>\n<raw bytes>` for every program-output burst: live, byte-lossless, never coalesced (the per-subscriber queue holds every burst).
    pub bytes: bool,
    /// The `trim` modifier: a `screen` DELTA stops after the last non-blank row (`DELTA <sid> seq=<n> screen <nrows>`, `<nrows>` the count sent), by the rule of `text trim` ([`crate::control::trimmed_len`]).
    pub trim: bool,
    /// Emit `MAIL <local> id=<n> off=<n> from=<p> kind=<k> [re=<n>]`, or `id=f<n> off=-` for direct fleet mail. Broker and direct cursors are independent. A `GAP` names broker rows evicted before this subscriber saw them.
    pub mail: bool,
    /// Emit `LOG <local> <record>`, one line per record appended to the watched session's SESSION LOG (phase 3A), at the host tier `log` shows by default (never the content tier: no OSC title), filtered as `log lvl= class=turn` filter ([`crate::session_log::shows`]), and `GAP <local> log-lost=<n> why=<w> resync_to=<seq> ep=<e> base=<b>` (`base=` the log's identity, for a resume's `log-base=`) when the records past the watch's cursor cross a loss or name another log, exactly as the read counts it (seqs have holes across a carried update, so no id arithmetic would do).
    pub log: bool,
}

impl TargetStreams {
    #[must_use]
    const fn wants_content(self) -> bool {
        self.screen || self.cursor || self.cells
    }

    #[must_use]
    const fn any_frame_source(self) -> bool {
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
    /// The instance lifecycle stream: `EVENT * session-created <sid>` when any session spawns and `EVENT * session-exited <sid> reason=<shell-exit|ctl-close|ui-close|window-close|reaper|unknown>` when one closes, so a supervisor learns of siblings without polling.
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
    pub(crate) const fn sessions(self) -> bool {
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
    /// `since-log=<seq> log-ep=<e> [log-base=<b>]`: resume the `log` stream after that record of that epoch (a `log`/`brief`/`status`/`await log` reply's), the read saying any loss in between as a `GAP`; `log-base=` (`status log_base=`) names the log the cursor is of, so another log's cursor reads as a reset ([`aterm_slog::Cursor::base`]).
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
    /// `None` on a `cells` watch the session refused ([`FrameSlot::hold_cells`]).
    cells_hold: Option<CellsHold>,
    /// Whether this watch's `cells` refusal `GAP` went out, since the last frame it sent.
    cells_refusal_said: bool,
    last_block_id: Option<u64>,
    last_turn_id: Option<u64>,
    last_timeline_id: Option<u64>,
    /// An `events` watch's `role= attention= user_title=`, as `meta` prints them, read in the
    /// same meta hold as `last_timeline_id` ([`watch_start`]): its ack says them
    /// ([`ack_tail`]), and every later change is an `EVENT <local> meta` past the watermark.
    acked_meta: Option<String>,
    last_resize_id: u64,
    last_title: Option<String>,
    last_bell: u64,
    byte_sub: Option<ByteSubscription>,
    non_coalesced: bool,
    pace: Paces,
    records_read: u64,
    last_mail_id: u64,
    last_direct_mail_id: u64,
    mail: crate::fabric::MailFilter,
    dirty: Arc<AtomicBool>,
    log: Option<LogWatch>,
    closing: bool,
    timeline_held: bool,
}

#[derive(Clone, Copy, Debug)]
struct LogWatch {
    cursor: aterm_slog::Cursor,
    /// The log the ack says the watch starts in (`log_base=`): the one `log-base=` named, else the watched log's own ([`aterm_slog::Log::base_ep`]).
    base: u32,
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

    const fn on(self) -> bool {
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
    const fn new(writer: &mut W, timestamps: bool) -> Egress<'_, W> {
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

/// Scan the target's event timeline past `last_id` and push each record kind [`timeline_wire_kind`] carries as `EVENT <sid> <kind>[ <payload>]`, the payload being the record's own pre-encoded tail (`meta`'s is `field=<f> value=<pct|->`), a program's claim's as [`timeline_wire_payload`] words it. It reads the session log's view: a row id past the watermark the log issued and no longer holds is said first as `GAP <sid> events-dropped=<n>`.
pub(crate) fn drain_timeline_events(
    tl: &crate::session_timeline::SessionTimeline,
    sid: &str,
    last_id: Option<u64>,
    out: &mut String,
) -> Option<u64> {
    let hi = match tl.view_head() {
        None => return last_id,
        Some(hi) if last_id.is_some_and(|a| hi <= a) => return Some(hi),
        Some(hi) => hi,
    };
    let view = tl.view_since(last_id);
    if view.lost > 0 {
        out.push_str(&format!("GAP {sid} events-dropped={}\n", view.lost));
    }
    for e in &view.rows {
        let Some(kind) = timeline_wire_kind(e.kind) else {
            continue;
        };
        let payload = timeline_wire_payload(e);
        if payload.is_empty() {
            out.push_str(&format!("EVENT {sid} {kind}\n"));
        } else {
            out.push_str(&format!("EVENT {sid} {kind} {payload}\n"));
        }
    }
    Some(hi)
}

/// A PROGRAM'S CLAIM IS PUSHED AS THE CLAIM AND ITS ROW, NEVER AS AN AGENT'S WORD: a `program-state` row (`state=<w> kind=<k|->`) is pushed as `program state=<w>[ kind=<k>] rev=<id>` (`kind=` only on a blocked state that names one) and a self-claim's `harness-act` row (`act=<act> holder=<h>`) as `act <act> rev=<id>`, `<id>` the row's own id (`timeline`'s `event <id>`), so a codex's or Grok Build's moves and a self-claim's acts reach an `events` watcher while `agent` stays the screen verdict's alone. Every other row's payload is its own, verbatim.
fn timeline_wire_payload(e: &crate::session_timeline::TimelineEvent) -> std::borrow::Cow<'_, str> {
    match e.kind {
        crate::session_timeline::PROGRAM_STATE => {
            let shown = e.payload.strip_suffix(" kind=-").unwrap_or(&e.payload);
            std::borrow::Cow::Owned(format!("{shown} rev={}", e.id))
        }
        crate::session_timeline::HARNESS_ACT => {
            let act = e
                .payload
                .strip_prefix("act=")
                .and_then(|tail| tail.split(' ').next())
                .unwrap_or("-");
            std::borrow::Cow::Owned(format!("{act} rev={}", e.id))
        }
        _ => std::borrow::Cow::Borrowed(&e.payload),
    }
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
        crate::restart_act::RESTART_EVENT => Some("restart"),
        crate::sys_host::JOB_CHANGE => Some("job"),
        crate::occupancy_wakes::WAKE_READY => Some("wake"),
        crate::session_timeline::PROGRAM_STATE => Some("program"),
        crate::session_timeline::HARNESS_ACT => Some("act"),
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
                    #[cfg(unix)]
                    crate::session_store::RosterChange::QuitCloses => {
                        out.push_str("EVENT * quit-closes\n");
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
    let (high, rows) = watch
        .src
        .fabric
        .direct_mail_since(watch.last_direct_mail_id, &watch.mail);
    for row in rows {
        out.push_str(&format!(
            "MAIL {sid} id=f{} off=- from={} kind={}\n",
            row.id, row.from, row.kind
        ));
    }
    watch.last_direct_mail_id = watch.last_direct_mail_id.max(high);
}

/// What a target's `sub <local> <sid>` ack says after its sid: a `log` watch's anchor
/// (` log_seq= log_ep= log_base=`), then an `events` watch's meta ([`Watch::acked_meta`]: ` role=
/// attention= user_title=`), each empty when the watch has none.
fn ack_tail(watch: &Watch) -> String {
    let mut tail = watch.log.as_ref().map_or_else(String::new, |lw| {
        format!(
            " log_seq={} log_ep={} log_base={}",
            lw.cursor.seq, lw.cursor.ep, lw.base
        )
    });
    if let Some(meta) = &watch.acked_meta {
        tail.push(' ');
        tail.push_str(meta);
    }
    tail
}

fn with_ack_tails<'h>(handshake: &'h str, watches: &[Watch]) -> std::borrow::Cow<'h, str> {
    if watches
        .iter()
        .all(|w| w.log.is_none() && w.acked_meta.is_none())
    {
        return std::borrow::Cow::Borrowed(handshake);
    }
    let mut out = String::with_capacity(handshake.len() + 64 * watches.len());
    for line in handshake.split_inclusive('\n') {
        let watch = line
            .strip_prefix("sub ")
            .and_then(|rest| rest.split_whitespace().next())
            .filter(|local| local.bytes().all(|b| b.is_ascii_digit()))
            .and_then(|local| local.parse::<u64>().ok())
            .and_then(|local| watches.iter().find(|w| w.local_id == local));
        match (watch.map(ack_tail), line.strip_suffix('\n')) {
            (Some(tail), Some(body)) if !tail.is_empty() => {
                out.push_str(body);
                out.push_str(&tail);
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
    let (gap, recs, next, base) = {
        let tl = watch.src.timeline.lock().unwrap_or_else(|p| p.into_inner());
        let log = tl.slog().log();
        if log.reader_last_seq() == lw.cursor.seq
            && log.ep() == lw.cursor.ep
            && !lw.cursor.names_another_log(log.base_ep())
        {
            return;
        }
        let read = log.read_shared(lw.cursor, aterm_slog::MAX_RECORDS);
        let recs: Vec<Arc<aterm_slog::Rec>> = read
            .recs
            .into_iter()
            .filter(|r| crate::session_log::shows(r, lw.filter.lvl, lw.filter.class_turn))
            .map(Arc::clone)
            .collect();
        (read.gap, recs, read.next, log.base_ep())
    };
    lw.cursor = next;
    lw.base = base;
    if let Some(g) = gap {
        out.push_str(&format!(
            "GAP {sid} log-lost={} why={} resync_to={} ep={} base={base}\n",
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

    'content: {
        if !streams.wants_content() {
            break 'content;
        }
        let cells_on = streams.cells && watch.cells_hold.is_some();
        let _turn = cells_on.then(|| watch.src.frame.build.lock_or_recover());
        let mut look = watch.src.frame.look_frame();
        let (t_us, seq, alt, gap, advance, cur, text, rows, cells_etag, styled) = {
            let t = crate::term_lock(&watch.term);
            if !watch.closing && crate::frame_unfinished_now(&t) {
                watch.pace.frame = t.frame_hang_deadline();
                watch.pace.content.settle(false);
                break 'content;
            }
            watch.pace.frame = None;
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
            let key = cells_on.then(|| FrameKey::read(&t));
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
                .map(|k| {
                    let floor = crate::control::styled_frame_floor(
                        usize::from(t.rows()),
                        usize::from(t.cols()),
                    );
                    let styled = if floor > STYLED_FRAME_CAP {
                        Err(floor)
                    } else if let Some(reached) = look.refused(&k) {
                        Err(reached)
                    } else {
                        Ok(look.styled(&t, &k))
                    };
                    (k, styled)
                });
            (
                t_us, seq, alt, gap, advance, cur, text, rows, cells_etag, styled,
            )
        };
        let mut cells_refused = None;
        let cells_payload = styled
            .and_then(|(key, styled)| {
                let built = match styled {
                    Ok(Styled::Hit(frame)) if frame.body().len() <= STYLED_FRAME_CAP => Ok(frame),
                    Ok(Styled::Hit(frame)) => Err(frame.body().len()),
                    Ok(Styled::Miss(snap)) => look.publish_styled_within(key, &snap),
                    Err(floor) => Err(floor),
                };
                built
                    .map_err(|bytes| {
                        cells_refused = Some(bytes);
                        watch.last_cells_body = None;
                    })
                    .ok()
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
        if cells_payload.is_some() {
            watch.cells_refusal_said = false;
        } else if !watch.cells_refusal_said {
            let refusal = match cells_refused {
                Some(bytes) => Some(format!(
                    "GAP {sid} cells-too-large={bytes} cap={STYLED_FRAME_CAP}\n"
                )),
                None if streams.cells && !cells_on => {
                    Some(format!("GAP {sid} cells-watches={MAX_CELLS_WATCHES}\n"))
                }
                None => None,
            };
            if let Some(refusal) = refusal {
                out.push_str(&refusal);
                watch.cells_refusal_said = true;
            }
        }
        watch
            .pace
            .content
            .settle(!out.is_empty() || cells_payload.is_some());
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

/// What a watch reads from its session besides the engine: the byte fan-out (the `bytes` stream), the turn ledger, the meta its ack seeds and the event timeline (the `events` digest), the inbox (`mail`), and the published frame (`screen`, `cells`); and the watcher list its subscription registers on, which the writes to all of these ring.
#[derive(Clone)]
pub(crate) struct WatchSource {
    byte_fanout: Arc<ByteFanout>,
    turns: Arc<Mutex<TurnLedger>>,
    meta: Arc<Mutex<crate::session_timeline::SessionMeta>>,
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
            meta: ctx.meta.clone(),
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
    let (last_timeline_id, last_resize_id, acked_meta) = watch_start(src, streams);
    Watch {
        local_id: *local_id,
        term: term.clone(),
        last_mail_id: if streams.mail {
            src.fabric.last_msg_id()
        } else {
            0
        },
        last_direct_mail_id: if streams.mail {
            src.fabric.last_direct_mail_id()
        } else {
            0
        },
        mail: opts.mail.clone(),
        log: streams.log.then(|| {
            let tl = src.timeline.lock().unwrap_or_else(|p| p.into_inner());
            let log = tl.slog().log();
            let cursor = opts.log_since.unwrap_or_else(|| {
                let from = if log_from_start {
                    0
                } else {
                    log.reader_last_seq()
                };
                aterm_slog::Cursor::new(log.ep(), from)
            });
            LogWatch {
                cursor,
                base: cursor.base.unwrap_or_else(|| log.base_ep()),
                filter: opts.log,
            }
        }),
        closing: false,
        timeline_held: false,
        last_turn_id: opts
            .since_turn
            .filter(|anchor| *anchor <= crate::control::turn_ids_minted())
            .or_else(|| initial_turn_watermark(&src.turns, streams)),
        last_timeline_id,
        acked_meta,
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
        cells_hold: streams
            .cells
            .then(|| FrameSlot::hold_cells(&src.frame))
            .flatten(),
        cells_refusal_said: false,
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

/// The least time after a content read (`screen`, `cursor`, `cells`) that pushed something before the next one a wake drives on the same watch; a read that pushed nothing opens no window ([`ReadPace::settle`]).
pub(crate) const CONTENT_PACE: Duration = Duration::from_millis(16);

/// The least time between two `events` reads that the PTY reader's wakes drive on one watch.
pub(crate) const EVENTS_PACE: Duration = CONTENT_PACE;

/// One watch's paced reads: its content streams' ([`CONTENT_PACE`]) and its `events` digest's ([`EVENTS_PACE`]), and a content read put off for an unfinished frame.
#[derive(Default)]
pub(crate) struct Paces {
    content: ReadPace,
    events: ReadPace,
    /// A content read put off for an unfinished DEC 2026 frame, owed with no ring at its per-run hang detector.
    frame: Option<Instant>,
}

impl Paces {
    fn due(&self) -> Option<Instant> {
        let content = self.content.due(CONTENT_PACE).or(self.frame);
        let events = self.events.due(EVENTS_PACE);
        content.into_iter().chain(events).min()
    }
}

/// One paced read of a watch: when a wake last drove it, and whether one is owed ([`CONTENT_PACE`], [`EVENTS_PACE`]).
#[derive(Default)]
pub(crate) struct ReadPace {
    read_at: Option<Instant>,
    owed: bool,
    /// The window the read [`Self::visit`] just made replaced, until [`Self::settle`] says whether that read found anything.
    replaced: Option<Option<Instant>>,
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
        self.replaced = Some(self.read_at.replace(now));
        Some(true)
    }

    /// Settles the read [`Self::visit`] just made: one that pushed something keeps the window it opened; one that pushed nothing hands it back, so a change a moment after an empty read (a keystroke's echo after a ring that moved nothing) is read at its own ring instead of at the end of a window nothing was pushed in. A read with nothing to settle is left as it is.
    pub(crate) fn settle(&mut self, pushed: bool) {
        if let Some(replaced) = self.replaced.take()
            && !pushed
        {
            self.read_at = replaced;
        }
    }

    /// The read at `now` that a record drove ([`EVENTS_PACE`]): made whatever the window, and covering any read owed, since it reads the latest state.
    pub(crate) const fn read_now(&mut self, now: Instant) {
        self.owed = false;
        self.replaced = None;
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
    fn gone(&mut self) -> bool;

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

    let handshake = with_ack_tails(handshake, &watches);
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
    let frame_due = pace.frame.is_some_and(|hang| now >= hang);
    let content = streams.wants_content()
        && pace.content.visit(now, dirty || frame_due, CONTENT_PACE) == Some(true);
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

/// Wait while a test holds `step`, on a thread that has nothing else to do meanwhile. A shipped
/// build holds nothing.
#[cfg(any(unix, test))]
pub(crate) fn wait_out_hold(step: &str) {
    while push_held(step) {
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
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

/// An `events` watch's timeline and resize watermarks ([`initial_timeline_watermarks`]) and the
/// meta its ack says (critic item E05), all read in ONE meta hold. Every meta write records its
/// `meta-change` row inside the session's meta hold (meta → timeline,
/// [`crate::session_timeline::SessionTimeline::record_meta_change`]), so each change is either
/// already in the fields the ack says or a row past the watermark, an `EVENT <local> meta` this
/// watch pushes: none falls between the two.
fn watch_start(src: &WatchSource, streams: TargetStreams) -> (Option<u64>, u64, Option<String>) {
    watch_start_with(src, streams, initial_timeline_watermarks)
}

/// [`watch_start`] with its watermark read passed in, so a test can ask what is held while the
/// watermark is read.
fn watch_start_with(
    src: &WatchSource,
    streams: TargetStreams,
    watermarks: impl FnOnce(
        &Arc<Mutex<crate::session_timeline::SessionTimeline>>,
        TargetStreams,
    ) -> (Option<u64>, u64),
) -> (Option<u64>, u64, Option<String>) {
    if !streams.events {
        return (None, 0, None);
    }
    let meta = src.meta.lock().unwrap_or_else(|p| p.into_inner());
    let (head, resizes) = watermarks(&src.timeline, streams);
    (
        head,
        resizes,
        Some(crate::control::presence_meta_tokens(&meta)),
    )
}

fn initial_timeline_watermarks(
    timeline: &Arc<Mutex<crate::session_timeline::SessionTimeline>>,
    streams: TargetStreams,
) -> (Option<u64>, u64) {
    if !streams.events {
        return (None, 0);
    }
    let tl = timeline.lock().unwrap_or_else(|p| p.into_inner());
    (tl.view_head(), tl.resizes().high_row())
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
        let tail = ack_tail(&watch);
        watches.push(watch);
        out.push(Frame::text(
            Tag::Instance,
            t_us,
            format!("sub {} {sid}{tail}\n", target.local_id),
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
    const fn watchers(&self) -> &Arc<Watchers> {
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
#[path = "subscribe_digest_fixture_tests.rs"]
pub(crate) mod digest_fixture;

#[cfg(test)]
#[path = "subscribe_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "subscribe_log_stream_tests.rs"]
mod log_stream_tests;

#[cfg(test)]
#[path = "subscribe_wire_goldens_tests.rs"]
mod wire_goldens;
