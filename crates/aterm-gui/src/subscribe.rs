// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The real-time PUSH face (design P1.3) — the additive layer that turns the
//! poll-only control socket into a server-PUSH face, so one agent watches its
//! OWN and OTHER sessions LIVE without busy-polling.
//!
//! A `subscribe` connection (its own thread, see [`crate::control`]) registers a
//! [`SubscriberHandle`] under the process-local `u64` id of each session it watches,
//! the id `Wake::Output { session }` routes with. The handle is a single-slot notify
//! (`sync_channel(1)`): the producer's one `Wake::Output` hook calls
//! [`SubscriberSet::notify`], which `try_send`s to every subscriber of that session and
//! ignores a full slot or a hung-up receiver. The subscriber thread blocks on `recv`,
//! then reads the session's current state and emits deltas.
//!
//! A `subscribe` connection (already its own thread, see [`crate::control`])
//! registers one [`Subscription`] on the [`crate::session_watch::Watchers`] of
//! each session it watches (and on the store's instance list for `sessions` and
//! `@*`). The subscription is a SINGLE-SLOT non-blocking notify: an
//! `mpsc::sync_channel(1)` whose `try_send` NEVER blocks. The producers ring the
//! session's list at the write that changes what a stream reads: the PTY reader
//! after each burst, a timeline or turn record, a roster journal append
//! ([`crate::session_watch`] names them). The subscriber thread blocks on
//! `recv`, then reads the CURRENT state of each session whose list rang (its
//! registration's dirty flag) and emits deltas; a session that did not ring is
//! not read. A write a host makes into a session's engine itself (a selection,
//! a config reload's colours, a Kitty frame) rings at the end of the hold that
//! made it ([`crate::TermGuard`]), for the subscriptions that read the screen.
//!
//! The registry lock is a leaf, never held across a `Terminal` lock or a socket write.
//! Targets resolve through [`crate::session_store::Store`] with the control path's
//! clone-then-release discipline.
//!
//! The notify slot has capacity ONE and `try_send` drops on a full slot, so a
//! pending-but-unread notify simply stays pending: N producer wakes between two
//! subscriber reads collapse into AT MOST ONE pending notify. When the subscriber
//! finally wakes it reads the LATEST state (the current `content_seq` and grid),
//! not a queue of every intermediate frame. A slow subscriber
//! therefore gets COARSER / fewer deltas and CANNOT block, backpressure, or even
//! slow the producing session's reader thread — the producer's `try_send` is O(1)
//! and infallible by design (it discards rather than waits). If the subscriber's
//! own socket write fails, its thread drops the connection (and deregisters) —
//! the producer is never involved in that path. A write the subscriber is slow
//! to read (a consumer that paused) waits for it: its push worker stays its own
//! for the whole subscription, reading or not, and the stream goes on when it
//! reads again: an end of stream still means its sessions ended or it hung up.
//!
//! ## Discipline
//!
//! A watcher list's lock is a strict leaf: a ring takes it, `try_send`s, and
//! releases — nothing is locked under it, and it is never held across a socket
//! write. The subscriber thread takes it only to (de)register itself; it reads
//! target terminals through handles cloned out of the
//! [`crate::session_store::Store`] with the same clone-then-release discipline
//! the rest of the control path uses.
//!
//! ## Push-only
//!
//! Once a connection issues `subscribe` and the verb authorizes, [`crate::control`]
//! FLIPS that connection to push mode: it stops reading requests and enters
//! [`push_loop_with_peer_probe`]. A subscribed connection is PUSH-ONLY for the rest of its life —
//! the client reads `DELTA`/`EVENT`/`GAP` frames and never sends another verb.

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

/// The per-target streams a subscription may watch: exactly the frames the per-target
/// `ReadScreen` gate in [`crate::control`] authorizes, one resolved selector at a time.
/// No authority is attached, which is safe only because every target handed to
/// [`push_loop_with_peer_probe`] has passed the gate individually. Instance-wide
/// streams live in [`InstanceStreams`]: the gate speaks only for sessions the client
/// named, and an instance stream reports sessions it never named.
///
/// `screen`/`cursor`/`cells` ride the `content_seq` delta path; `events` the
/// block-complete (OSC 133 D) signal; `bytes` the raw output fan-out.
#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
pub(crate) struct TargetStreams {
    /// Emit `DELTA <sid> seq=<n> screen <changed rows>` when content advances.
    pub screen: bool,
    /// Emit `DELTA <sid> seq=<n> cursor <row> <col> <visible> <style>` on any caret
    /// change (position, DECSCUSR style, or DECTCEM visibility) — even at an
    /// unchanged seq. `<visible>` is `0|1`, matching the poll `cursor` verb.
    pub cursor: bool,
    /// The lifecycle digest: `EVENT <sid> block-complete <id> exit=<code>` on OSC 133 D,
    /// `EVENT <sid> turn <id> submitted=<0|1> status=<..> dur_ms=<..>` per completed
    /// turn (the turn ledger), `EVENT <sid> title <pct>` on an OSC 0/2 title change,
    /// `EVENT <sid> bell total=<n>` on a BEL, the timeline kinds [`timeline_wire_kind`]
    /// carries (`meta field=<f> value=<pct|->`, `agent`, `status`, `human`, `render`,
    /// the fabric digest), and at retirement `EVENT <sid> closing reason=<token>
    /// by=<sid|human|ctl|bridge|->` then `EVENT <sid> exited`. One low-rate stream for
    /// N sessions on one fd; pull `screen`/`image` when an event says to.
    pub events: bool,
    /// Emit `DELTA <sid> seq=<n> cells <nbytes>\n<styled-json>` when the styled
    /// frame changes — the LOSSLESS styled-screen frame (Item 1's payload) pushed live,
    /// so an outer agent sees the inner TUI's colour/attrs, not plaintext.
    pub cells: bool,
    /// Emit `BYTES <sid> <len>\n<raw bytes>` for every program-output burst: live,
    /// byte-lossless, never coalesced (the per-subscriber queue holds every burst).
    pub bytes: bool,
    /// The `trim` modifier: a `screen` DELTA stops after the last non-blank row
    /// (`DELTA <sid> seq=<n> screen <nrows>`, `<nrows>` the count sent), by the rule of
    /// `text trim` ([`crate::control::trimmed_len`]). It lives here, not in
    /// [`PushOptions`], because it shapes one per-target frame; it carries no authority.
    /// Inert without `screen`; alone it names no source, so a `trim`-only list fails.
    pub trim: bool,
    /// Emit `MAIL <local> id=<n> off=<n> from=<p> kind=<k> [re=<n>]`, one line per row
    /// delivered into the watched session's inbox, and a `GAP` when the ring evicted
    /// rows this subscriber had not been shown.
    ///
    /// Metadata only, by the shape of [`crate::fabric::MailLine`]: no body, no `text=`.
    /// A manager sees who wrote and what kind, and reads the words with `inbox get`.
    /// No authority beyond the subscribe gate: `inbox` listing and `subscribe` both
    /// require `Op::ReadScreen` (`control::required_op`), so this pushes a read the
    /// subscriber can already make.
    pub mail: bool,
    /// Emit `LOG <local> <record>`, one line per record appended to the watched
    /// session's SESSION LOG (phase 3A), at the host tier `log` shows by default
    /// (never the content tier: no OSC title), filtered as `log lvl= class=turn`
    /// filter ([`crate::session_log::shows`]), and `GAP <local> log-lost=<n>
    /// why=<w> resync_to=<seq> ep=<e>` when the records past the watch's cursor
    /// cross a loss, exactly as the read counts it (seqs have holes across a
    /// carried update, so no id arithmetic would do). Live from the head unless
    /// resumed with `since-log=<seq> log-ep=<e>`, or (an `@*` adoption of a
    /// session created after the subscription) from its log's start. The
    /// target's `sub <local> <sid>` line names where the stream starts,
    /// ` log_seq=<seq> log_ep=<e>`: a reader that resumes from the last record
    /// it read (or from there, when it read none) misses nothing and is shown
    /// nothing twice.
    ///
    /// AUTHORITY: none beyond the subscribe gate this target already passed —
    /// the push face of `log`, which a `ReadScreen` edge may already read.
    pub log: bool,
}

impl TargetStreams {
    /// Whether any `content_seq`-driven stream (screen/cursor/cells) is requested.
    #[must_use]
    fn wants_content(self) -> bool {
        self.screen || self.cursor || self.cells
    }

    /// Whether any per-target frame source is requested. `trim` is a modifier and does
    /// not count, so `trim` alone fails the "names a source" check.
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

/// What the client asked for at instance scope, before any authority check. Only
/// [`InstanceStreams::authorize`] consumes it; request and grant are different types
/// so a parse result cannot reach [`push_loop_with_peer_probe`] by mistake.
#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
pub(crate) struct RequestedInstance {
    /// The instance lifecycle stream: `EVENT * session-created <sid>` when any session
    /// spawns and `EVENT * session-exited <sid>
    /// reason=<shell-exit|ctl-close|ui-close|window-close|app-quit|unknown>` when one
    /// closes, so a supervisor learns of siblings without polling. `*` marks an
    /// instance-level event.
    ///
    /// Owner-only: it diffs the whole roster, which no per-target `ReadScreen` edge can
    /// authorize; every other scope gets `ERR denied`.
    ///
    /// JOURNAL-DRIVEN: each wake drains the store's roster journal, a monotonic log
    /// of every registration and exit that keeps the newest
    /// [`crate::session_store::ROSTER_JOURNAL_CAP`] (512) records. Every append rings
    /// the store's instance watchers, which the subscription registers on, so a
    /// sibling's spawn or exit wakes the loop at once; a sibling that spawns AND exits
    /// between two wakes is still reported, as both
    /// events in order, because the journal kept both records. The lossy case is a
    /// subscriber the journal has rolled past (more than 512 records between two of its
    /// wakes): it falls back to a set diff of the live roster, which reports only the
    /// NET change (`reason=unknown` on the exits) and cannot see a create→exit pair that
    /// cancelled out. The push loop's `drain_roster` has the detail.
    ///
    /// With `@*`, a live session's `session-created` follows its watch's `sub <local> <sid>`
    /// ack, except one gone again by the wake that saw it (its `session-exited` follows in
    /// the same frame) and one deferred at [`crate::control::MAX_SUBSCRIBE_TARGETS`]:
    /// announced at once with a trailing `watch=deferred`, and acked with no second
    /// announcement if a slot ever frees (see `drain_membership`).
    pub sessions: bool,
}

/// The instance-scoped streams a subscription was granted. The field is private, so outside
/// this module only [`InstanceStreams::authorize`], which demands a [`Scope`], sets
/// `sessions` (`Default` is the empty set). [`push_loop_with_peer_probe`] seeds its watermark
/// from the store's whole live roster, a read no per-target gate authorized; taking this type
/// makes that unreachable without a scope decision.
#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
pub(crate) struct InstanceStreams {
    sessions: bool,
}

impl InstanceStreams {
    /// Grant the instance-scoped streams `req` asked for: all of them to Owner (the
    /// scope the `sessions` and `who` verbs demand), the empty set to every other scope.
    ///
    /// The empty set is the invariant, not the user-visible behaviour:
    /// [`crate::control`] refuses a non-Owner `sessions` request with `ERR denied`
    /// before it reaches here. This fallback keeps the invariant if a caller forgets.
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

/// A parsed `<streams>` token, split by scope. The instance half is a
/// [`RequestedInstance`], not an [`InstanceStreams`], because parsing sees no
/// [`Scope`]: the halves are authorized by different checks (per resolved target
/// vs. once for the connection).
#[derive(Clone, Default, PartialEq, Eq, Debug)]
pub(crate) struct Requested {
    /// The per-target frame sources, gated once per resolved selector.
    pub targets: TargetStreams,
    /// `mail`'s `kinds=`/`from=` filter. Inert unless `targets.mail` is set; it
    /// carries no authority, only narrowing rows the gate already allowed.
    pub mail: crate::fabric::MailFilter,
    /// `log`'s `lvl=`/`class=turn` filter. Inert unless `targets.log` is set;
    /// it carries no authority, only narrows records the gate already allowed.
    pub log: LogFilter,
    /// The instance-wide frame sources, gated once for the connection.
    pub instance: RequestedInstance,
    /// A modifier, not a frame source: prefix a wake's frames with `T <tag> <t_us>`
    /// lines stamped on the same clock as `video`'s `index.json`, so a driver can align
    /// the stream against a `video` take. Opt-in; carries no authority.
    ///
    /// The ledger is per TAG per wake, not one stamp per wake: AT MOST one `T` line is
    /// written per tag per wake, immediately before that tag's FIRST frame, and it
    /// speaks for every later frame the same tag emits in that wake. Its `<t_us>` is
    /// when that frame's state was READ, never when it was written: the term hold
    /// that read a watch's content, its events drain, the store read of a membership
    /// pass, and a `bytes` burst's arrival at the reader. A watch's frames are read
    /// within one pass of it, so the stamp is the earliest of their instants. It is
    /// NOT monotonic on a channel that mixes `bytes` with content: the reader dates a
    /// burst before it waits for the term lock and tees it after, so a burst that
    /// arrived before a screen read can drain on the next wake under a smaller `T`
    /// than the screen's, and it is written as is. A tag
    /// that emits nothing this wake is not stamped at all, so a `T` line always has
    /// frames under it. A wake that writes three channels emits three `T` lines. That
    /// is what keeps a stamp attributable — a single leading stamp would date frames
    /// from channels whose data was read at a different point in the wake, and a
    /// client demultiplexing by leading token would have to guess which stream each
    /// `T` belonged to.
    ///
    /// `<tag>` is [`Tag`]'s rendering: `T <local> <t_us>` for a per-target frame,
    /// `T * <t_us>` for an instance (`sessions`) frame. The second token is therefore
    /// not always numeric.
    pub timestamps: bool,
}

/// Split a stream list into tokens, keeping a `mail:` token's own comma list
/// together: `mail:kinds=task,ask` arrives as two pieces, and a piece that names no
/// stream continues the `mail` token. Unambiguous because the stream names and the
/// kinds ([`aterm_types::fabric_grammar::KINDS`]) are closed sets, and fail-closed
/// either way: a continuation that is not a kind is rejected by
/// [`parse_mail_params`], and a mistyped stream name outside `mail:` still reaches
/// the `_ => return None` arm.
fn fold_mail_params(s: &str) -> Vec<String> {
    // The one vocabulary, plus the `ts` alias the catalog spells inside `timestamps`.
    // A piece names a stream by the part before any `:` (`log:class=turn` is
    // the `log` stream, never a continuation of `mail:`'s comma list).
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

/// One sender class or one principal, never a list. `from=` is singular, and the
/// fold that reassembles `kinds=`'s comma list can hand this `from=h-a,h-b`, which
/// would match nothing; a comma is refused, as is anything that is neither a class
/// nor a principal-shaped token.
fn is_sender_selector(v: &str) -> bool {
    if matches!(v, "human" | "agent" | "service" | "other") {
        return true;
    }
    // A principal: `<class>-<name>`, optionally `@<node>` for a peer session.
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

/// `` | `:kinds=<k,..>` | `:from=<class|principal>` | `:topic=<t>` | any
/// combination, in any order.
///
/// Fail-closed on anything else: an unknown or repeated key, an empty value, or a
/// kind outside [`aterm_types::fabric_grammar::KINDS`]. A filter that silently
/// matched nothing would look alive and say nothing.
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
            // Not `kinds=` and not `topic=`, so it must be `from=` — an unknown
            // key is the `None` this `?` produces.
            let v = part.strip_prefix("from=")?;
            if want.from.is_some() || !is_sender_selector(v) {
                return None;
            }
            want.from = Some(v.to_string());
        }
    }
    Some(want)
}

/// `log`'s filter (phase 3A): the deepest level shown (`log:lvl=<l>`) and the
/// turn class only (`log:class=turn`), as the `log` verb's `lvl=` and
/// `class=turn` read ([`crate::session_log::shows`]).
#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
pub(crate) struct LogFilter {
    /// `lvl=`: no record of a deeper level.
    pub lvl: Option<aterm_slog::Lvl>,
    /// `class=turn`: only the turn class.
    pub class_turn: bool,
}

/// `` | `:lvl=<session|run|turn|step>` | `:class=turn` | both, in either order.
/// FAIL-CLOSED on anything else (an unknown key, a repeat, a bad level), as
/// [`parse_mail_params`] is.
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
    /// Fail-closed twice: `None` on any unknown token, and `None` on a list that
    /// names no frame source. Every token of `subscribe @. ts` parses, so without the
    /// second check the client would get `OK subscribe 1` and then silence for the
    /// life of a push-only connection, indistinguishable from a hung server.
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

/// The subscription knobs that carry no authority: where each stream resumes from
/// and how a wake is rendered. Grouped so [`push_loop_with_peer_probe`]'s two
/// authority-bearing arguments, [`TargetStreams`] and [`InstanceStreams`], stay
/// visibly separate in its signature.
#[derive(Clone, Default, PartialEq, Eq, Debug)]
pub(crate) struct PushOptions {
    /// `since=<seq>`: the client's last-seen `content_seq`. Seeds each watch's
    /// `last_sent_seq` so the immediate catch-up fires exactly when content moved
    /// past it.
    pub since: Option<u64>,
    /// `since-turn=<id>`: resume the `events` turn stream from that ledger id.
    pub since_turn: Option<u64>,
    /// `since-block=<id>`: resume the `events` block stream from that block id.
    pub since_block: Option<u64>,
    /// `every-frame`: re-read the `cells` frame on every wake of its session
    /// even when the frame key stands (animation fidelity), sending it only
    /// when its body changed.
    pub non_coalesced: bool,
    /// The `timestamps`/`ts` modifier — see [`Requested::timestamps`].
    pub timestamps: bool,
    /// `mail`'s row filter ([`Requested::mail`]). It carries no authority, so it
    /// rides with the other knobs.
    pub mail: crate::fabric::MailFilter,
    /// `log`'s filter ([`Requested::log`]), no authority, like `mail`'s.
    pub log: LogFilter,
    /// `since-log=<seq> log-ep=<e>`: resume the `log` stream after that record
    /// of that epoch (a `log`/`brief`/`status`/`await log` reply's), the read
    /// saying any loss in between as a `GAP`. `None`: live from the head.
    pub log_since: Option<aterm_slog::Cursor>,
    /// `@*`: the roster journal's seq when the target set was resolved. A
    /// session the journal says was CREATED after it is news to this
    /// subscription: its `log` stream reads from the start of its log, so its
    /// opening (`open - spawn|adopt|restore parent= …`, the lineage only that
    /// record names) and whatever it logged before the adoption pass reach
    /// the reader. `None` (a named target set): every watch starts live.
    pub adopt_after: Option<u64>,
}

/// One watched target inside a subscription: its process-local id, the live engine
/// and [`WatchSource`] resolved ONCE at subscribe time, plus the per-target send
/// cursors (`last_sent` content seq, last block id) so the coalescing compare is
/// O(1) on each wake.
///
/// The `<local>` tag on every frame is the process-local channel id (the registry
/// key), not the `s-…` sid. The ack ([`push_loop_with_peer_probe`]) maps each
/// `<local>` to its sid with one `sub <local> <sid>` line.
struct Watch {
    /// The process-local id (the registry key + the `<local>` frame tag).
    local_id: u64,
    /// The live engine handle (cloned out of the store, clone-then-release).
    term: Arc<Mutex<Terminal>>,
    /// The session's leaves the streams read besides the engine, cloned out with
    /// `term`: the event digest reads the turn ledger and timeline, and the
    /// `mail` stream the inbox. Never the context, whose sink owns the master
    /// (see [`WatchSource`]).
    src: WatchSource,
    /// The last `content_seq` we emitted a DELTA for: the `since=` watermark. A
    /// seq past it, or a resync, restates every requested content stream; at an
    /// equal seq each stream asks its own question of the frame key
    /// (`last_text`, `last_cells_etag`, `last_cursor`).
    last_sent_seq: u64,
    /// The alt-screen flag as of the last content DELTA. An alt swap (1049) does not
    /// touch the main grid's `content_seq` (the poll path keys its cache on both), so a
    /// flip is treated like a backward seq: GAP plus a full resync frame.
    last_alt: bool,
    /// The `(row, col, visible, style)` of the last cursor DELTA. A caret move, a
    /// DECSCUSR style flip or a DECTCEM toggle bumps no `content_seq`, so this is
    /// compared on every wake. `visible` matches the poll `cursor` verb's field.
    last_cursor: (u16, u16, bool, &'static str),
    /// `screen`: the [`TextKey`] as of the last look. While it stands the
    /// visible rows are unchanged and no row is read; when it moves at an equal
    /// seq (any batch of output does, a caret move included) the rows are read
    /// and compared by `last_rows_hash`. Seeded to the live key.
    last_text: TextKey,
    /// `screen`: the screen's `hash=` (FNV-1a-64 of every visible row, before
    /// `trim`) as of the last screen DELTA, so a moved [`TextKey`] over the
    /// same text resends nothing. Seeded to the live hash.
    last_rows_hash: u64,
    /// `cells`: the [`FrameKey::etag`] as of the last look. A frame change that
    /// writes no content (a `C=1` Kitty placement, a VS16 width fix, a recolour,
    /// a selection, a caret move, a style-policy reload) moves it at an
    /// unchanged seq, and the styled frame is re-read; it is re-sent, with no
    /// GAP, when its body differs from `last_cells_body`: the client's seq
    /// cursor is still valid. Seeded to the live etag.
    last_cells_etag: u64,
    /// `cells`: the (FNV-1a-64, length) of the styled body as of the last
    /// gather. The key folds `screen_mutations`, which every batch of output
    /// moves, so a batch that changes no frame (a title, a bell, a DSR answer,
    /// a mode toggle) moves it too; at an equal seq only a body whose id
    /// differs from this one is sent. `None` until the first gather, so a
    /// subscriber caught up by `since=` may be sent its frame once more on the
    /// first key move.
    last_cells_body: Option<(u64, usize)>,
    /// `cells`: this watch's hold on its session's published styled frame, so a
    /// frame one watch gathers is the one every other reader of the session is
    /// served until the key moves. Held, never read; `None` without `cells`.
    _cells_hold: Option<CellsHold>,
    /// The id of the highest block we have already reported `block-complete` for,
    /// so re-scanning completed blocks on each wake never double-emits. `None`
    /// before any block has completed.
    last_block_id: Option<u64>,
    /// The highest turn id already reported from the session's TURN LEDGER,
    /// mirroring `last_block_id` — a live stream, so it is seeded to the current
    /// high at subscription.
    last_turn_id: Option<u64>,
    /// The highest EVENT TIMELINE id already scanned, mirroring `last_turn_id` —
    /// a live stream, seeded to the current high at subscription. The `events`
    /// digest pushes each `meta-change` record past it as `EVENT <sid> meta
    /// <payload>`, the push face of the `meta` verb.
    last_timeline_id: Option<u64>,
    /// The highest resize-ledger row already pushed (`EVENT <local> resize`), seeded to
    /// the ledger's high at subscription.
    last_resize_id: u64,
    /// The window title as of the last `EVENT … title`, seeded to the live title at
    /// subscription so only later changes push. `None` when `events` is off.
    last_title: Option<String>,
    /// The `bell_total` (monotonic fired-bell count) as of the last `EVENT … bell`,
    /// seeded to the live total at subscription so only later bells push.
    last_bell: u64,
    /// The live byte-stream subscription for the `bytes` stream (Item 2), or `None`
    /// when `bytes` was not requested. Drained into `BYTES`/`GAP` frames on every
    /// wake of its session.
    byte_sub: Option<ByteSubscription>,
    /// `every-frame` mode: re-read the `cells` frame on every wake of its
    /// session (a genuine wake) even when the frame key stands (animation
    /// fidelity), and send it when its body differs from `last_cells_body`:
    /// never a byte-identical re-send. Another session's wake is not one.
    non_coalesced: bool,
    /// When this watch's wakes last drove a content read and an `events` read,
    /// and whether each is owed ([`Paces`]).
    pace: Paces,
    /// The session's record count ([`Watchers::records_rung`]) as of this watch's
    /// last `events` read, seeded to the live count. A pass that finds the
    /// count past it reads the digest at once, unpaced ([`pass_streams`]).
    records_read: u64,
    /// The highest inbox row id already pushed (or stepped over), seeded to the
    /// ring's high at subscription: `mail` is a live stream, not a replay of the
    /// backlog. `await inbox` reads history.
    last_mail_id: u64,
    /// Which rows this subscriber asked for.
    mail: crate::fabric::MailFilter,
    /// Its registration's dirty flag ([`Subscription::watch`]): every ring of
    /// the session's watcher list sets it, and the pass that reads the watch
    /// swaps it false first, so a pass reads only the watches whose session
    /// rang. Clear until the registration that follows the seed hands it the
    /// registration's flag, which starts set.
    dirty: Arc<AtomicBool>,
    /// The `log` stream's cursor and filter, when it was asked for.
    log: Option<LogWatch>,
    /// Set for a closing watch's FINAL pass ([`Closing::drain`]), which the
    /// test-only `timeline` hold never withholds ([`frames_for_watch`]).
    closing: bool,
    /// The last pass's timeline drain was withheld by the test-only `timeline`
    /// hold ([`push_held`]). While it is set, every pass reads the watch as if
    /// its session rang ([`visit_watches`]) and the loop keeps a timer
    /// ([`timeline_owed`]), so the first [`LIVENESS`] pass after the hold goes
    /// drains what it withheld and the `GAP` the ring left, with no further
    /// record to ring for it. The first drain the hold does not withhold clears
    /// it.
    timeline_held: bool,
}

/// One target's `log` stream: where it has read to and what it shows. About
/// 32 bytes; each wake's records are built once and released.
#[derive(Clone, Copy, Debug)]
struct LogWatch {
    cursor: aterm_slog::Cursor,
    filter: LogFilter,
}

/// Authority for the `@*` live target set: whether this subscription may adopt
/// sessions created after it subscribed.
///
/// Constructible only by [`Self::authorize`], which requires Owner, as
/// [`InstanceStreams::authorize`] does: `@*` reveals siblings' existence and sids
/// exactly as the `sessions` stream does. With Owner the only holder, the per-target
/// `ReadScreen` gate an adopted session would face is satisfied by construction.
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

    /// Whether this subscription adopts.
    fn on(self) -> bool {
        self.0
    }
}

/// A frame's channel tag: which of a subscription's channels the frame speaks for.
/// `Instance` renders as `*`, the tag `EVENT * session-created …` already uses, so
/// stamping an instance frame (`T * <t_us>`) needs no change to the wire grammar.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Tag {
    /// A per-target frame, tagged with the target's process-local channel id.
    Channel(u64),
    /// A connection-level frame that speaks for no single target (the `sessions`
    /// lifecycle stream).
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

/// The wire `<local>` channel tag for a target: the rendering of [`Tag::Channel`].
/// One connection watching multiple sessions demultiplexes frames by this leading
/// token, resolving it to a stable sid via the ack's `sub <local> <sid>` map.
fn sid_tag(local_id: u64) -> String {
    Tag::Channel(local_id).to_string()
}

/// [`sid_tag`] formatted into a caller's stack buffer: the same decimal digits
/// (a `u64` has at most 20), without the heap `String` a per-tick producer that
/// usually emits nothing would allocate and throw away.
fn sid_on_stack(local_id: u64, buf: &mut [u8; 20]) -> &str {
    let len = {
        let mut w = &mut buf[..];
        let _ = write!(w, "{local_id}");
        20 - w.len()
    };
    std::str::from_utf8(&buf[..len]).unwrap_or("")
}

/// One run of a frame's wire bytes. A frame is the segments it is written
/// from, in order, so a body the frame shares is written from where it lives:
/// the session's published styled frame and the reader's output bursts are
/// never copied into a frame.
enum Seg {
    /// Text this producer formatted: headers, rows, events.
    Text(String),
    /// The session's published styled frame ([`FrameSlot`]): every `cells`
    /// watch of the session writes this one body.
    Shared(Arc<StyledFrame>),
    /// One output burst, the reader's own allocation (the `bytes` stream).
    Bytes(Arc<[u8]>),
    /// A fixed terminator.
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

/// One producer's already-formatted wire bytes for one channel, and the
/// instant they were read. Deliberately NOT one wire line: the lines a
/// producer builds for one tag from one read share its instant, so the stamp
/// ledger gains nothing from splitting them.
///
/// The fields are private and the constructors live here, so the ONLY route from
/// bytes to the socket is [`Egress::send`]. That is what turns "was this frame
/// stamped?" into a property of the door rather than of each write site — the six
/// old write sites each had to remember, and four of them structurally could not,
/// because their producers took no timestamp flag at all.
struct Frame {
    tag: Tag,
    /// When the state this frame carries was read, on `video`'s clock
    /// ([`crate::metrics::now_us`]): in the term hold that read a watch's
    /// content keys, at its events drain, at the store read for membership,
    /// and for a `bytes` frame its first burst's arrival at the reader. A
    /// `T` line says this instant, never the write's.
    t_us: u64,
    segs: Vec<Seg>,
}

impl Frame {
    /// A text frame: `DELTA` / `EVENT` / `GAP` bodies are all UTF-8.
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

/// The client hung up. A ZST, so `Result<(), Gone>` is as cheap as `()`. Not a
/// failure to report: a push-only connection ends exactly this way.
#[derive(Clone, Copy, Debug)]
struct Gone;

/// The one door from a [`Frame`] to the subscriber socket. It owns the writer for the
/// push loop's life, the timestamp policy and the per-wake stamp ledger, so
/// `T <tag> <t_us>` is emitted once per tag per wake for every frame kind, including
/// `bytes` bursts, instance lifecycle events, a closing target's `exited` and the
/// events-resume `GAP`.
///
/// NO BUFFER. The writer is the socket itself; each producer's frames go out
/// by one vectored write ([`Egress::send`]), so there is nothing to flush and
/// a client that hung up is seen by the write that meets it.
struct Egress<'w, W: Write> {
    writer: &'w mut W,
    /// The `timestamps`/`ts` modifier — see [`Requested::timestamps`].
    timestamps: bool,
    /// The tags already stamped this wake. Cleared, not reallocated, by
    /// [`Egress::begin_wake`]; a linear scan suits a tag count of watches + 1.
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

    /// Open a wake: clear the stamp ledger so each tag is stamped once for the frames
    /// this wake produces.
    fn begin_wake(&mut self) {
        self.stamped.clear();
    }

    /// Write one producer's `frames` in order by ONE vectored write: a watch's
    /// content, events and byte frames, a closing watch's tail, or one
    /// membership pass. Each producer is written as soon as it is built, so a
    /// watch's frames stay contiguous and only one producer's bodies are held
    /// at a time.
    ///
    /// With timestamps, the first frame of each tag still unstamped this wake
    /// is preceded by `T <tag> <t_us>`, where `<t_us>` is the earliest
    /// [`Frame::t_us`] among that tag's frames here: a `bytes` burst that
    /// arrived before the screen was read is dated when it arrived. An EMPTY
    /// frame writes nothing and does NOT consume its tag's stamp — otherwise a
    /// producer that had nothing to say would silence the stamp of the next
    /// producer on the same tag.
    fn send(&mut self, frames: Vec<Frame>) -> Result<(), Gone> {
        // The stamp lines this group opens, at the index of the frame each leads.
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

/// [`Write::write_all`] over `slices` by vectored writes: each call takes as
/// many slices as the platform does (std caps one at `IOV_MAX`), and a short
/// write resumes mid-slice. A writer with no `write_vectored` of its own
/// (Windows' `CtlStream`) writes one slice per call through the same loop.
/// Every slice is non-empty, so a write of zero bytes is a writer that can
/// take no more.
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

/// Format a full screen DELTA for `sid` at `seq`: a header line followed by one
/// row per screen line (CHANGED set is the whole screen here — a coalesced wake
/// re-reads the latest grid rather than a diff, the backpressure-safe choice). The
/// row count is on the header so a client can frame the body without guessing.
///
/// `DELTA <sid> seq=<n> screen <nrows>\n` then the first `<nrows>` rows of
/// `text`, each trimmed and followed by `\n`.
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

/// Format a cursor DELTA: `DELTA <sid> seq=<n> cursor <row> <col> <visible> <style>\n`.
/// `<visible>` (`0|1`) matches the poll `cursor` verb, so a DECTCEM toggle shows on
/// the push stream too.
fn frame_cursor(sid: &str, seq: u64, row: u16, col: u16, visible: bool, style: &str) -> String {
    format!(
        "DELTA {sid} seq={seq} cursor {row} {col} {} {style}\n",
        u8::from(visible)
    )
}

/// Format a styled cells DELTA, length-prefixed so the large single-line JSON body
/// frames cleanly: `DELTA <sid> seq=<n> cells <nbytes>\n<json>\n`. The body is
/// `styled_frame_payload`'s one line; the trailing `\n` is an uncounted terminator.
///
/// Returned as the segments it is written from: `head` (whatever the watch
/// formatted before it, then the header line), the session's published body
/// by reference, and the terminator.
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

/// Drain the `bytes` stream into `BYTES`/`GAP` frames as raw bytes (binary-safe: no
/// escaping, no UTF-8 decode). A counted `GAP <sid> bytes-dropped=<n>` precedes the
/// bursts whenever the per-subscriber queue overflowed since the last drain. Framing:
/// `BYTES <sid> <len>\n<len bytes>\n`.
///
/// Takes `&Watch`, not `&mut`: the byte queue is its own watermark (draining empties
/// it), so this producer moves no send cursor, which lets [`Closing::drain`] read the
/// tail of a watch it is consuming.
///
/// Each burst is written from the reader's own allocation, never copied: its
/// header text, then the burst, and one burst's terminator leads the next
/// header. The frame is dated by its first burst's arrival.
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
    // A burst is dropped only to make room for a newer one, so a GAP always
    // has a burst under it; the clock is read only if that ever changes.
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

/// Format a block-complete EVENT for `sid`:
/// `EVENT <sid> block-complete <id> exit=<code|->\n`.
fn frame_block_complete(sid: &str, id: u64, exit: Option<i32>) -> String {
    let exit = exit.map_or_else(|| "-".to_string(), |c| c.to_string());
    format!("EVENT {sid} block-complete {id} exit={exit}\n")
}

/// Emit `EVENT <sid> title <pct-encoded>` when the window title changed since the
/// last drain; returns the new watermark. Pct-encoded so the title stays one line.
///
/// Pure (takes the sampled change): the digest samples the title, the bell total and
/// the new completed blocks in one [`sample_engine_events`] lock hold, because that
/// lock is the one the renderer and the keystroke-encode path contend for.
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

/// Scan the target's event timeline past `last_id` and push each record kind
/// [`timeline_wire_kind`] carries as `EVENT <sid> <kind>[ <payload>]`, the payload
/// being the record's own pre-encoded tail (`meta`'s is `field=<f> value=<pct|->`).
/// Other kinds (`spawned`, `state-change`, `title-change`, `cwd-change`) are skipped:
/// title has its own emitter and lifecycle state is what `exited` says.
///
/// * `EVENT <sid> meta <payload>` for a `meta-change` — the push face of the
///   `meta` verb, next to the title emitter (a fleet supervisor learns a sibling
///   was renamed/annotated without polling `meta`); the payload is the record's
///   own pre-pct-encoded `field=<f> value=<pct|->` tail.
/// * `EVENT <sid> agent <word> rev=<n> gen=<e.s> fp=<hex16>` for an
///   `agent-change` — the server's agent verdict moved (`status agent=`),
///   with the screen generation and hash it was read from — for clients that
///   do not need the rows. The in-GUI supervisor classifies by design (decided
///   2026-09-25 under the owner's standing direction): it parks server-side on
///   `await seq|idle|gone`, and it must read the rows anyway to parse the
///   approval box, so it classifies those rows with aterm-phase itself and uses
///   the verdict only for one bounded `await agent prompt`.
/// * `EVENT <sid> task rev=<n> state=<s> attention=<0|1> [why=<w>] [exit=<n>]`
///   for a `task-change` — a worker's task record moved (`task`), the edge a
///   manager's `await task` parks on, pushed for a watcher that holds a stream.
/// * `EVENT <sid> status phase=<p> outcome=<o> exit_code=<n|-> signal=<n|->
///   confidence=<c>` for a `status-change` — the session's classified status
///   moved phase or outcome (`status phase=`), one per transition that survived
///   dwell.
/// * `EVENT <sid> human` for a `human` row — a person started typing,
///   clicking or scrolling in the session through a window after at least
///   [`crate::session_timeline::HUMAN_BURST_GAP_MS`] without (the burst edge;
///   `status human_ms=` is the running age).
/// * `EVENT <sid> closing <payload>` for the `closing` row the store writes as
///   it retires the session — the `reason=<token> by=<sid|human|ctl|bridge|->` the `exits`
///   ledger holds. This watch's own `Arc` is the ONLY wire path that can still
///   read that row: the `timeline` verb resolves its target through the
///   registry, and the sid stops resolving in the same store write that records
///   it. It lands in the dying watch's final pass ([`Closing::drain`]), so it
///   always precedes `EVENT <sid> exited`.
/// * `EVENT <sid> render <verdict>` for a `render` row (the resize ledger), and
///   `EVENT <sid> <kind> <payload>` for the fabric digest's kinds (`inbox`,
///   `post`, `hold`, …; [`crate::fabric::FABRIC_EVENT_KINDS`]) — metadata only,
///   no body.
///
/// Every other kind (`spawned`, `state-change`, `title-change`, `cwd-change`) is
/// skipped: title has its own emitter and lifecycle state is what `exited`
/// says. [`timeline_wire_kind`] is the one table both the filter and the frame
/// name come from. The returned watermark advances to the timeline HIGH (not
/// just the last pushed record), so skipped records are scanned once, never
/// re-walked every wake. A watermark below the retained low — the ring is
/// drop-oldest at [`crate::session_timeline::TIMELINE_CAP`] — is a hole this
/// watcher never saw, said first as `GAP <sid> events-dropped=<n>` (the mail
/// lane's `mail-dropped=` twin), so a bridge can re-read what a lost `topic`
/// row would have told it. The caller holds the timeline (one hold per visit,
/// shared with [`drain_resize_events`], never across a socket write), and the
/// rows are formatted straight from it into `out`, no payload cloned. The
/// test-only `timeline` hold ([`push_held`]) is the caller's to read, under
/// that same hold ([`frames_for_watch`]).
pub(crate) fn drain_timeline_events(
    tl: &crate::session_timeline::SessionTimeline,
    sid: &str,
    last_id: Option<u64>,
    out: &mut String,
) -> Option<u64> {
    // O(1) HIGH-WATER COMPARE BEFORE THE SCAN. `high_id()` is `back()`, so
    // this is one integer compare; a visit after a ring that recorded nothing
    // here (a burst, a turn) must not touch the retained deque at all.
    // Returning `Some(hi)` (not `last_id`) reproduces the old
    // `tl.high_id().or(last_id)` watermark byte-for-byte on this arm.
    let hi = match tl.high_id() {
        None => return last_id,
        Some(hi) if last_id.is_some_and(|a| hi <= a) => return Some(hi),
        Some(hi) => hi,
    };
    // Records between the watermark and the retained low are gone.
    let missed = match (last_id, tl.low_id()) {
        (Some(a), Some(lo)) => lo.saturating_sub(a + 1),
        _ => 0,
    };
    if missed > 0 {
        out.push_str(&format!("GAP {sid} events-dropped={missed}\n"));
    }
    // `since` SEEKS (partition_point) instead of filtering the whole retained
    // ring, so this costs O(log n + matched).
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

/// Evaluate the target's RESIZE LEDGER against this wake's engine `probe`
/// (recording any `render` verdict transition on the timeline, which the
/// timeline drain pushes; a probe older than what the ledger booked since it
/// was sampled settles nothing it could misread), then push the resizes booked
/// since `last_id` as ONE coalesced `EVENT <sid> resize n=<k> id=<last>
/// to=<CxR> run=<r>` — a window drag's sixty rows a second are one frame per
/// wake, never a flood. A watermark below the ledger's retained low adds
/// `dropped=<n>` to that frame (the rows the ring evicted unshown): a count on
/// this stream's own frame, not a `GAP`, which a consumer of the `events`
/// stream reads as lost lifecycle frames. Returns the new watermark. The
/// caller holds the timeline, as for [`drain_timeline_events`]. A visit with
/// nothing booked is a scan of the ledger's open runs (none, almost always)
/// and one integer compare.
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

/// The `EVENT <sid> <kind>` token a timeline record is pushed under on the `events`
/// digest, or `None` for a kind the digest does not carry. One table, so the drain's
/// filter and the frame it formats cannot disagree about which rows leave the process.
pub(crate) fn timeline_wire_kind(kind: &str) -> Option<&'static str> {
    match kind {
        "meta-change" => Some("meta"),
        "closing" => Some("closing"),
        // `SessionTimeline::publish_agent`: `EVENT <local> agent <word> rev=<n> gen=<e.s>
        // fp=<hex16>`, the push face of `status agent=`.
        "agent-change" => Some("agent"),
        // The session's TASK RECORD moved (`crate::task_record::mark_session_task`, or the
        // supervisor's attention on a tasked session): `EVENT <local> task
        // rev=<n> state=<s> attention=<0|1> [why=<w>] [exit=<n>]`, the push face
        // of `task` — no result text and no path; `task` reads the record.
        "task-change" => Some("task"),
        // The session's published status moved phase or outcome
        // (`session_status::StatusObserver::take_status_transition`): `EVENT <local>
        // status phase=<p> outcome=<o> exit_code=<n|-> signal=<n|->
        // confidence=<c>`, the push face of `status` — one per transition that
        // survived dwell, so a spinner cannot flood it.
        crate::session_status::STATUS_CHANGE => Some("status"),
        // `crate::app_input::note_person`: a person's input after `HUMAN_BURST_GAP_MS`
        // without any; no payload (`status human_ms=` says how long ago).
        "human" => Some("human"),
        // `crate::resize_ledger`'s verdict entering or leaving `desync-risk`:
        // `EVENT <local> render desync-risk run=<r> displaced=<+-k> net=zero at=<ms>` /
        // `EVENT <local> render healed run=<r> after_ms=<ms>`, the push face of `status render=`.
        "render" => Some("render"),
        // The `[harness]` policy cell moved (`App::announce_harness_policy`):
        // `EVENT <local> harness-policy gen=<n>`, on which an agent that
        // answers for itself re-reads `harness policy`.
        crate::harness_policy_verb::HARNESS_POLICY_EVENT => Some("harness-policy"),
        // A job's verdict moved (the stuck engine, `crate::vitals_host`'s
        // hand-over): `EVENT <local> job <verdict> job=<id> class=<class>
        // rev=<n> [task=<id>] [code=<n>]`, the push face of `status job=` and
        // `sys jobs` — join keys and closed words, no command text.
        crate::sys_host::JOB_CHANGE => Some("job"),
        // The FABRIC digest (design §11.2): `inbox`, `inbox-seen`, `post`,
        // `fetch`, `post-landed`, `hold`, `topic`. Their wire name IS the record kind, and the list
        // lives beside the code that WRITES them
        // ([`crate::fabric::FABRIC_EVENT_KINDS`]) rather than being retyped here,
        // so a new fabric event cannot ship recorded-but-never-pushed — the
        // exact drift the `closing` row shipped with. NONE of them carries a body:
        // `inbox` is a row id, the sender's full principal, a kind and an offset,
        // `post` an id, an address and a kind with their options. The
        // digest tells an agent that mail EXISTS; reading it is a separate,
        // deliberate `inbox` call, so a watcher is never handed message text it
        // did not ask for.
        other => crate::fabric::fabric_event_wire_kind(other),
    }
}

/// Emit `EVENT <sid> bell total=<n>` when the monotonic fired-bell count advanced since
/// the last drain (BEL / OSC 777 alert). Returns the new watermark. One event per drain
/// however many bells fired (the total says how many), so it cannot flood. Pure, like
/// [`drain_title_event`]: the engine read is in [`sample_engine_events`].
fn drain_bell_event(sid: &str, total: u64, last_bell: u64, out: &mut String) -> u64 {
    if total > last_bell {
        out.push_str(&format!("EVENT {sid} bell total={total}\n"));
    }
    total
}

/// A `sessions` subscriber's position on the instance roster: the journal
/// watermark it has drained to, plus the live set as of that watermark.
///
/// The watermark is the cursor; the set is carried only so the recovery path (a
/// watermark past the journal's drop-oldest low) can still diff correctly. On the
/// normal path journal records mutate it one at a time, so an idle tick costs nothing.
struct RosterCursor {
    /// Highest roster-journal seq already drained. `0` = "seen nothing".
    seq: u64,
    /// The sids this cursor counts as live as of `seq` (recovery input only —
    /// see the type doc): the live roster the subscription was seeded with,
    /// which is never announced (a fresh subscriber `ls`s for its baseline),
    /// plus every sid announced since, minus every exit announced since.
    known: std::collections::HashSet<String>,
}

/// Append `EVENT * session-created <sid>[ watch=deferred]` / `EVENT * session-exited <sid> reason=<…>` /
/// `EVENT * fabric-retire <sid>` / `EVENT * bye <quit|handoff>` to `out` for every roster change since the cursor's watermark — the INSTANCE lifecycle
/// stream (`*` = instance-level, not a per-channel event). Surfaces a SIBLING
/// spawn/exit a fleet supervisor is not watching, so it need not poll `ls`.
///
/// THREE THINGS CHANGED HERE, AND THE THIRD IS A CORRECTNESS FIX.
///
///  1. AN IDLE TICK IS O(1). The store now keeps a monotonic membership journal,
///     so "did anything change?" is one `u64` compare under a read lock. It used
///     to be: build a fresh `HashSet<String>` of every live sid (one allocation
///     and one hash per session), then run two `HashSet::difference` passes over
///     it — for every `sessions` subscriber, on every 250 ms wake, including the
///     bare liveness ticks that are the overwhelming majority.
///
///  2. A TICK WITH CHANGES IS O(changes), not O(sessions).
///
///  3. A SUB-TICK SPAWN+EXIT NOW SURFACES. The old design could not report it,
///     and the push loop's own module doc said so: "A sibling that both spawns
///     AND exits inside one 250ms window appears in neither snapshot and emits
///     neither event — the store keeps no monotonic lifecycle log." A snapshot
///     diff reports only the NET change between two instants; events that
///     cancelled out are gone. The journal records each transition as it
///     happens, so BOTH events are emitted, in order, on the next wake —
///     without shortening the tick or adding a notify path.
///
/// The store keeps a monotonic membership journal, so an idle tick is one `u64`
/// compare, a tick with changes is O(changes), and a sibling that spawns and exits
/// between two wakes is reported as both events in order. A cursor that fell past the
/// journal's retained low takes the recovery path: diff the live set against `known`
/// ([`diff_session_events`]), which reports only the net change.
///
/// It reads the registry through the caller's guard, the one read
/// [`drain_membership`] shares with adoption. `watched` is `Some` exactly when the
/// subscription adopts (`@*`) and says whether a local id has a watch once that pass's
/// adoptions are built. A `session-created` for a session live under this read that
/// `watched` answers `false` for carries ` watch=deferred`, on either arm, so on an
/// adopting subscription a plain one means the session is watched or already gone
/// (its `session-exited` is in the same batch).
///
/// The cursor advances only after its lines are produced.
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
    // The marker for a session live under this read with no watch, on a
    // subscription that adopts: its adoption was deferred at the cap.
    let mark = |sid: &str| {
        let deferred = watched.is_some_and(|w| {
            g.by_sid(&aterm_session::SessionId::new(sid))
                .is_some_and(|h| !w(h.local_id))
        });
        if deferred { " watch=deferred" } else { "" }
    };
    match g.roster_low_seq() {
        // Fast path: every record we have not seen is still retained, so the
        // delta is exact and complete.
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
                        // `reason=` trails, so a client keying on the sid ignores it.
                        out.push_str(&format!(
                            "EVENT * session-exited {} reason={}\n",
                            rec.sid,
                            rec.reason.as_str()
                        ));
                        cursor.known.remove(&rec.sid);
                    }
                    // An operator's request to the bridge, not a membership
                    // change: `known` is untouched.
                    crate::session_store::RosterChange::RetireRequested => {
                        out.push_str(&format!("EVENT * fabric-retire {}\n", rec.sid));
                    }
                    // The instance's last word to its bridge: how it leaves.
                    #[cfg(any(unix, test))]
                    crate::session_store::RosterChange::Bye(bye) => {
                        out.push_str(&format!("EVENT * bye {}\n", bye.word()));
                    }
                }
            }
        }
        // Recovery: records between our watermark and the retained window were
        // evicted (or the journal is empty at a nonzero high). Rebuild and diff, with
        // the fast path's marker; net-lossy for pairs that cancelled out.
        _ => {
            let live = g.live_sids();
            diff_session_events(&live, &cursor.known, &mark, out);
            cursor.known = live;
        }
    }
    cursor.seq = high;
}

/// [`drain_roster`] for a subscription that does not adopt, as the one frame
/// the push loop would emit — the shape the roster tests assert on. The loop
/// itself reaches [`drain_roster`] only through [`drain_membership`].
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

/// The pure set-diff half of [`drain_roster`] (store-free, unit-testable):
/// `session-created` for sids newly live, each followed by whatever `mark` answers
/// (`""`, or ` watch=deferred`), and `session-exited … reason=unknown` for sids gone
/// (a set diff knows that a session went, never why).
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
        // No journal row says why: `unknown` is the honest token, in the journal
        // path's frame shape.
        out.push_str(&format!(
            "EVENT * session-exited {sid} reason={}\n",
            crate::session_store::ExitReason::Unknown.as_str()
        ));
    }
}

/// Scan the target's TURN LEDGER and emit a turn EVENT for every record whose id
/// is strictly greater than `last_turn_id`, advancing it — the turn-lifecycle
/// twin of `drain_block_events`. The small `(id, submitted, status, dur_ms, by)`
/// tuples are cloned OUT under the lock; the lock is never held across a write.
/// Digest form (no text/hash — that is what the `turns` verb is for):
/// `EVENT <sid> turn <id> submitted=<0|1> status=<..> dur_ms=<..> by=<who>\n`,
/// `by=` the `history` word ([`crate::turn_ledger::TurnBy`]), so a watcher
/// tells the window harness's own turns (`by=harness`: its continuations,
/// done checks and `/usage`, the most common turn on an autonomous run)
/// from a manager's without a `history` read.
fn drain_turn_events(
    turns: &Arc<Mutex<TurnLedger>>,
    sid: &str,
    last_turn_id: Option<u64>,
    out: &mut String,
) -> Option<u64> {
    let fresh: Vec<(u64, bool, &'static str, u64, String)> = {
        let l = turns.lock().unwrap_or_else(|p| p.into_inner());
        // O(1) HIGH-WATER COMPARE BEFORE THE SCAN — the timeline twin. `high_id()`
        // is `back()`. An idle wake (one that rang this session for something
        // else: its output, a timeline row) now costs one integer compare instead of a
        // walk over up to `LEDGER_CAP` retained records. The early-out returns
        // `last_turn_id` unchanged, which is exactly what the old fold produced
        // when it collected nothing.
        match l.high_id() {
            None => return last_turn_id,
            Some(hi) if last_turn_id.is_some_and(|a| hi <= a) => return last_turn_id,
            // `since` seeks (partition_point): O(log n + matched).
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

/// Format the one-shot `exited` EVENT for `sid`: `EVENT <sid> exited\n`. Emitted
/// by the push loop when a watched session leaves the registry.
fn frame_exited(sid: &str) -> String {
    format!("EVENT {sid} exited\n")
}

/// Format a resync GAP for `sid`: `GAP <sid> resync=<seq>\n`. Emitted when the
/// engine's `content_seq` moved backward relative to our last-sent (a reset,
/// alt-screen swap or engine rebuild), so the client treats the next DELTA as a fresh
/// full snapshot.
fn frame_gap(sid: &str, seq: u64) -> String {
    format!("GAP {sid} resync={seq}\n")
}

/// The `mail` stream: one `MAIL` line per newly delivered row the filter wants,
/// oldest first, preceded by a `GAP` when the ring evicted rows this subscriber
/// was never shown. Appended to the wake's buffer like every other digest line,
/// so a wake is still ONE frame.
///
/// The cursor advances to the ring's HIGH, not to the last row emitted, so a
/// row the filter rejected is stepped over once and never reconsidered — what
/// stops a `kinds=`-filtered subscription re-walking the ring on every wake.
///
/// NO BODY REACHES HERE: [`crate::fabric::MailLine`] has no field for one. The
/// line is metadata, and `from=` is the sender's FULL principal for every row,
/// an unaccepted sender's too — never a class. Its room for words is the
/// principal grammar [`crate::fabric::valid_principal`] holds `deliver` to.
///
/// LATENCY is the delivery's own: `deliver` records the row's `inbox` event on
/// the session's timeline under the inbox guard, and the record rings the
/// session's watchers, so the row is pushed on the wake it causes.
fn drain_mail_events(watch: &mut Watch, sid: &str, out: &mut String) {
    let since = watch.src.fabric.mail_since(watch.last_mail_id, &watch.mail);
    // What was issued above the cursor, less what the ring still holds. Not
    // "everything below the oldest row": eviction is by class, so a pinned human row
    // at the front stays while agent rows behind it go.
    let issued = since.high.saturating_sub(watch.last_mail_id);
    let missed = issued.saturating_sub(since.present);
    if missed > 0 {
        out.push_str(&format!("GAP {sid} mail-dropped={missed}\n"));
    }
    for row in &since.rows {
        let re = row.re.map_or_else(String::new, |off| format!(" re={off}"));
        // `topic=` rides the push line as it rides the `msg` row: it answers "why is
        // this in my mailbox?" without the body this stream never carries.
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

/// Where a watch's `log` stream starts, for its `sub` line: ` log_seq=<seq>
/// log_ep=<e>` (the cursor it was seeded at), or nothing when it streams no
/// log. A reader that resumes from the last `LOG` line it read, or from here
/// when it read none, misses nothing between two streams and is shown nothing
/// twice (`aterm ctl`'s relay does, across an update).
fn log_anchor(watch: &Watch) -> String {
    watch.log.as_ref().map_or_else(String::new, |lw| {
        format!(" log_seq={} log_ep={}", lw.cursor.seq, lw.cursor.ep)
    })
}

/// The handshake with each `sub <local> <sid>` line of a log-streaming target
/// naming where its stream starts ([`log_anchor`]), the watches being seeded.
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

/// THE `log` STREAM's drain (phase 3A): the records past the watch's cursor,
/// read under the timeline's leaf lock alone and filtered as the `log` verb
/// filters (records filtered out move the cursor too: they are not loss), then
/// formatted with the lock released: `GAP <sid> log-lost=<n> why=<w>
/// resync_to=<seq> ep=<e>` for the loss the read crossed (exact: the read's own
/// count, holes and resets said apart), then `LOG <sid> <record>` per record at
/// the host tier. An idle wake compares the head and the epoch and reads
/// nothing.
fn drain_log_events(watch: &mut Watch, sid: &str, out: &mut String) {
    let Some(lw) = watch.log.as_mut() else {
        return;
    };
    // The records shown are the ring's own, shared: a count bump each under
    // the lock, encoded after it is released.
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

/// The `mail` stream: one `MAIL` line per newly delivered row the filter wants, oldest
/// first, preceded by a `GAP` when the ring evicted rows this subscriber was never
/// shown. Appended to the wake's buffer, so a wake is still one frame.
///
/// Returns a [`Frame`], not a `String`, because unlike its sibling formatters this
/// one is emitted STANDALONE rather than appended to a target's wake body — and a
/// standalone write is exactly what has to go through the egress to be stamped.
/// `t_us` is when the ledger's low-water was read.
fn frame_gap_events(tag: Tag, t_us: u64, floor: u64) -> Frame {
    Frame::text(tag, t_us, format!("GAP {tag} events-resync={floor}\n"))
}

/// Emit a `block-complete` EVENT for every already-sampled completed block, advancing
/// and returning the `last_block_id` watermark. Pure: [`sample_engine_events`] did the
/// scan under the lock. A fold, so the watermark never passes a block whose line was
/// not written.
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

/// Everything the `events` digest reads out of the engine for one target, in one
/// `term_lock` hold (the lock the renderer and the keystroke-encode path contend for):
/// the completed blocks past the watermark, the window title, the bell total and the
/// resize probe.
///
/// An idle tick is O(1): `newest_completed_block()` is the high-water of block
/// completion, and `all_blocks()` yields strictly increasing ids, so a newest complete
/// id at or below the watermark means nothing is new and the deque is never touched.
/// Otherwise the walk runs backward from the newest and stops at the watermark, keyed
/// on the id, not on `is_complete()` (a Ctrl-C'd prompt is archived without
/// OSC 133;D), then reverses to the wire's oldest-first order.
struct EngineSample {
    /// Blocks that completed past the watermark, oldest-first; empty on an idle wake.
    blocks: Vec<(u64, Option<i32>)>,
    /// The live window title (`OSC 0/2`), owned only when it differs from the
    /// watermark. `None` means unchanged; `Some("")` is a change to an empty title.
    title: Option<String>,
    /// The monotonic fired-bell count.
    bell: u64,
    /// The resize ledger's four engine counters (`crate::resize_ledger`), read in
    /// this same hold: what the ledger's verdict is evaluated against.
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
        // Nothing has ever completed, or nothing completed past the watermark:
        // the O(1) early-out. No deque walk, no allocation.
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
    // Small, bounded clones out under the lock; the caller formats after the guard
    // drops, and no guard is ever held across a socket write.
    EngineSample {
        blocks,
        title: (last_title != Some(t.title())).then(|| t.title().to_string()),
        bell: t.bell_total(),
        resize: crate::resize_ledger::EngineProbe::sample(&t),
    }
}

/// Build the frames a single wake produces for one watched target, mutating its
/// send cursors. Returns the (possibly empty) frames for the caller to hand to
/// [`Egress::send`]: at most a content frame, dated in the term hold that read
/// it, then an events and mail frame, dated at its drain. PURE w.r.t. the
/// socket — the caller does the write — so this is unit-testable headlessly with
/// no real connection.
///
/// It applies no `T <sid> <t_us>` stamp: [`Egress`] does, the only place that
/// sees EVERY frame kind, from the instant each frame carries.
///
/// COALESCING: a `content_seq` past `last_sent_seq`, or a resync, restates every
/// requested content stream. At an equal seq each stream answers its own
/// question: `screen` whether the visible text changed ([`TextKey`], then the
/// screen's `hash=`), `cells` whether the styled frame did ([`FrameKey::etag`],
/// then the body's id), `cursor` whether the caret tuple did. A pure viewport
/// scroll moves none of them and emits nothing. A wake always re-reads the
/// LATEST state, so N coalesced producer wakes collapse into ONE delta carrying
/// the newest grid.
fn frames_for_watch(watch: &mut Watch, streams: TargetStreams, woke: bool) -> Vec<Frame> {
    #[cfg(test)]
    crate::work_counts::watch_visited();
    let mut sid_buf = [0u8; 20];
    let sid = sid_on_stack(watch.local_id, &mut sid_buf);
    let tag = Tag::Channel(watch.local_id);
    let mut frames = Vec::new();
    let mut out = String::new();

    if streams.wants_content() {
        // ONE lock hold reads everything this wake needs. The keys are cheap
        // (O(1) for the text, O(rows) plus the palette fingerprint for the frame,
        // read only for a `cells` watch); the EXPENSIVE reads (the visible rows,
        // the styled snapshot) run ONLY when their stream's key moved, and then
        // come from the session's published frame when it is that key's. Two
        // payoffs:
        //   • Atomicity (R6): seq, keys, cursor, rows and cells all come from a
        //     single consistent engine snapshot — a DELTA's seq can never pair
        //     with a grid or caret sampled a lock-acquisition later. A published
        //     frame is the frame of the key read in this hold.
        //   • Efficiency (R5): a wake that moved nothing pays only for the key
        //     reads — no row walk, no styled gather. A change is built once per
        //     session, however many watches and readers want it.
        // The slot is looked at BEFORE the term lock: the two never nest.
        let mut look = watch.src.frame.look_frame();
        let (t_us, seq, alt, gap, advance, cur, text, rows, cells_etag, styled) = {
            let t = crate::term_lock(&watch.term);
            // The frame's instant, read in the hold its state is read in.
            let t_us = crate::metrics::now_us();
            let seq = t.content_seq();
            let alt = t.is_alternate_screen();
            // RESYNC: the engine's content seq moved BACKWARD (reset / engine
            // rebuild), or the alt buffer swapped — a swap landing on the SAME
            // seq is a full-screen change, as 1049 does not move the main grid's
            // content_seq. The client's delta cursor is meaningless either way.
            // Never the epoch: it also steps on an ordinary region scroll
            // (DECSTBM, IL/DL, RI at a region top), and the fabric bridge reads
            // every GAP as a recovery trigger.
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
            // Rows only when the text may have changed; whether it did is the
            // rows hash's call, made below with the guard dropped.
            let rows = text
                .filter(|text| advance || *text != watch.last_text)
                .map(|_| look.text(&t));
            let cells_etag = key.map(|k| k.etag());
            // The styled frame is re-read when the key moved, or under
            // `every-frame` on a genuine wake (never the closing drain);
            // whether it is sent is the body's call, made below with the guard
            // dropped.
            let styled = key
                .filter(|k| {
                    advance || k.etag() != watch.last_cells_etag || (watch.non_coalesced && woke)
                })
                .map(|k| (k, look.styled(&t, &k)));
            (
                t_us, seq, alt, gap, advance, cur, text, rows, cells_etag, styled,
            )
        };
        // A miss is serialized with the lock RELEASED (expensive per-cell
        // JSON/base64) so it never blocks keystroke encode or a frame snapshot,
        // and published for the session's other readers. At an equal seq the
        // body is sent only when its id moved: the key also moves on a batch
        // that changes no frame, and every-frame re-reads an unchanged one.
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
        // The screen's `hash=` over the whole grid, so `trim` cannot hide a
        // change below the last non-blank row from the compare.
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
            // `trim`: the pushed twin of `text trim` — the frame stops after the
            // last non-blank row by the ONE shared rule, so a subscriber and a
            // poller agree on the row count.
            let n = if streams.trim {
                crate::control::trimmed_len(text.rows())
            } else {
                text.row_count()
            };
            out.push_str(&frame_screen(sid, seq, text, n));
        }
        // CURSOR: restated with every advance, and otherwise pushed when the
        // caret datum changed — a pure caret reposition / DECSCUSR style flip /
        // DECTCEM visibility toggle bumps no content_seq and would starve the
        // `cursor` stream on a seq gate.
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

    // The events, mail and log drains read their sources from here, after the
    // content: their frame's instant.
    let drained_at = (streams.events || streams.mail || streams.log).then(crate::metrics::now_us);
    if streams.events {
        // ONE Terminal-lock hold covers all three ENGINE-sourced sources (blocks,
        // title, bell); the turn ledger and the event timeline have their own
        // small mutexes, each taken once and gated by an O(1) high-water compare.
        // A watch whose session rang for something else (a burst, a turn) is
        // therefore one lock acquisition per source and a few integer compares,
        // where it used to be a walk over ~2000 retained records per target, and
        // a flood's rings take them at most once per `EVENTS_PACE`, a record
        // (a timeline row, a resize row, a turn) once per record; a watch whose
        // session did not ring is read only for an owed read come due
        // ([`pass_streams`]).
        //
        // EMISSION ORDER IS UNCHANGED — blocks, turns, timeline (meta, then the
        // closing row), title, bell — which is the only order the wire has ever
        // promised. What did move is the SAMPLING INSTANT of title and bell:
        // they are now read at the top of the events pass instead of the
        // bottom. Each of the five sources carries its own independent
        // watermark and none is derived from another, so the only effect is
        // which side of a 250 ms tick boundary a title/bell change that races
        // the pass lands on — the same one-tick skew the old order had in the
        // opposite direction, and a skew every watermark-driven stream here
        // already tolerates by construction.
        let engine = sample_engine_events(
            &watch.term,
            watch.last_block_id,
            watch.last_title.as_deref(),
        );
        watch.last_block_id =
            drain_block_events(sid, &engine.blocks, watch.last_block_id, &mut out);
        watch.last_turn_id = drain_turn_events(&watch.src.turns, sid, watch.last_turn_id, &mut out);
        {
            // One timeline hold for both of its drains. The resize ledger is
            // evaluated first, so a `render` verdict it records on the timeline
            // this wake is pushed by the timeline drain right below, in the
            // same frame.
            let mut tl = watch.src.timeline.lock().unwrap_or_else(|p| p.into_inner());
            watch.last_resize_id =
                drain_resize_events(&mut tl, sid, watch.last_resize_id, engine.resize, &mut out);
            // THE `timeline` HOLD IS READ UNDER THIS LOCK, which is what makes
            // it exact. A record is appended under the same lock, so a drain
            // either ran before the record existed or reads a marker that was
            // written before the record was asked for: no record made after
            // the marker reaches the wire while it stands. A held drain leaves
            // the watermark where it was and marks the watch, and the first
            // drain after the marker goes reports whatever the ring evicted
            // meanwhile as the `GAP`. A closing watch's final pass is never
            // held: a reader that stopped draining gets its tail late, never
            // not at all.
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

    // LAST in a wake (content, events, mail, then the log), so a closed
    // session's `close - out=…` precedes its `EVENT <local> exited`.
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

/// What a watch reads from its session besides the engine: the byte fan-out
/// (the `bytes` stream), the turn ledger and the event timeline (the `events`
/// digest), the inbox (`mail`), and the published frame (`screen`, `cells`);
/// and the watcher list its subscription registers on, which the writes to
/// all of these ring. [`WatchSource::of`] is its one constructor and clones
/// exactly these leaves out of a [`crate::SessionCtx`].
///
/// NEVER THE CONTEXT ITSELF. `SessionCtx::sink` owns the PTY master, which
/// closes only when the last `Arc<SinkWriter>` drops. A watch outlives its
/// tab's close until [`prune_closed`] runs, through the [`Closing`] drain, and
/// for as long as the subscriber's socket write stalls; a watch holding the
/// context would keep the closed tab's master open, and its child without a
/// hangup, for all of that, and would hand the read-only push path the
/// session's write sink.
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

/// A resolved subscribe target, cloned OUT of the store at subscribe time: the
/// process-local id, the live engine handle, and the session's [`WatchSource`].
/// The engine travels beside the source because the context does not hold it.
pub(crate) struct ResolvedTarget {
    pub(crate) local_id: u64,
    pub(crate) term: Arc<Mutex<Terminal>>,
    pub(crate) src: WatchSource,
}

/// The engine half of a watch's seed: every live value a fresh watch starts
/// from that only the `Terminal` knows, read under ONE guard by
/// [`engine_seed`]. Each field is read only for the streams that use it, and
/// is its zero otherwise.
#[derive(Default)]
struct EngineSeed {
    /// `events`: the live window title, so a fresh subscriber gets title
    /// CHANGES from here, not a spurious event for the title already showing.
    title: Option<String>,
    /// `events`: the live bell total, so a fresh subscriber gets bells from here.
    bell: u64,
    /// `events`: the newest completed block's id, so only blocks that COMPLETE
    /// after subscription push, never the historical backlog. It equals the
    /// `all_blocks().filter(is_complete).map(id).max()` walk: `all_blocks()`
    /// yields strictly increasing ids, so the newest complete block by position
    /// is also the largest complete id. The per-tick early-out in
    /// [`sample_engine_events`] rests on the same identity.
    newest_block: Option<u64>,
    /// content: the live alt-screen flag, so a subscriber that connects while a
    /// TUI is already on the alt buffer does not spuriously resync on its first
    /// wake; a genuine swap after this flips it.
    alt: bool,
    /// `screen`: the live [`TextKey`] and the screen's `hash=` under it (the
    /// session's published text, built by one row walk if it is stale), so a
    /// subscriber caught up by `since=` is sent the text again only when it
    /// changes.
    text: TextKey,
    rows_hash: u64,
    /// `cells`: the live [`FrameKey::etag`], so a subscriber caught up by
    /// `since=` is sent the styled frame again only when it changes.
    cells_etag: u64,
}

/// Read a watch's [`EngineSeed`] under one `term` guard, or take no guard at all
/// when `streams` wants neither content nor events (`bytes`, `mail`). A `screen`
/// watch's rows hash is the session's published text's, looked at before the
/// guard (the slot and the term lock never nest).
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

/// Build one target's send cursors: the one seeding rule for the initial targets and for
/// a `@*` adoption, so an adopted watch neither replays a backlog nor starts blind. The
/// `opts.since*` resume anchors require a single target and are refused for `@*`, so in
/// adopt mode they are `None` here.
///
/// Each source is read under its own lock, one at a time: the engine's values
/// under one `term` guard ([`engine_seed`]), then the turn ledger, the timeline
/// and the inbox, none of them nested in another.
fn new_watch(target: &ResolvedTarget, streams: TargetStreams, opts: &PushOptions) -> Watch {
    new_watch_at(target, streams, opts, false)
}

/// [`new_watch`], its `log` stream read from the START of the target's log
/// when `log_from_start` (a session created after the subscription, which
/// holds nothing the reader could have read before it: [`drain_membership`]).
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
    // Before the seed reads its watermarks, so a record that lands meanwhile
    // is past the count the watch starts from (it is read at once either way:
    // the registration's flag starts set and the first pass reads).
    let records_read = src.watchers.records_rung();
    let engine = engine_seed(term, &src.frame, streams);
    let (last_timeline_id, last_resize_id) = initial_timeline_watermarks(&src.timeline, streams);
    Watch {
        local_id: *local_id,
        term: term.clone(),
        // The ring's high: only mail delivered after subscription pushes.
        last_mail_id: if streams.mail {
            src.fabric.last_msg_id()
        } else {
            0
        },
        mail: opts.mail.clone(),
        // `log`: resumed after `since-log=`, else live from the reader's head
        // (a live stream replays no backlog, as `mail` does not) — but a
        // session created after the subscription is read from its log's start
        // (its seq 0 in its own epoch: every record it holds came after the
        // subscribe, and a loss among them is a `GAP` like any other).
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
        // `since-turn=<id>` resumes after that id; absent, the live high, so only
        // post-subscription turns push. An anchor above every turn id this process has
        // minted comes from elsewhere (a relay resumed across an update onto a successor
        // that restarted the count) and would hide every turn until the count passed it,
        // so it seeds at the live high too.
        last_turn_id: opts
            .since_turn
            .filter(|anchor| *anchor <= crate::control::turn_ids_minted())
            .or_else(|| initial_turn_watermark(&src.turns, streams)),
        // The live high: only records after subscription push.
        last_timeline_id,
        last_resize_id,
        last_title: engine.title,
        last_bell: engine.bell,
        last_sent_seq: opts.since.unwrap_or(0),
        last_alt: engine.alt,
        // A sentinel no real caret matches, so the first wake emits the cursor.
        last_cursor: (u16::MAX, u16::MAX, false, ""),
        last_text: engine.text,
        last_rows_hash: engine.rows_hash,
        last_cells_etag: engine.cells_etag,
        // No gather at subscribe: the first one sets it.
        last_cells_body: None,
        _cells_hold: streams.cells.then(|| FrameSlot::hold_cells(&src.frame)),
        // A `since-block=` ABOVE the newest completed block is from elsewhere
        // (a successor that could not carry the session's blocks counts them
        // afresh): it is lowered to the live high,
        // so it can never hide a block the engine completes from here.
        last_block_id: resume_block_watermark(opts.since_block, engine.newest_block),
        // On the byte fan-out only when `bytes` is requested, so an unwatched channel
        // costs nothing.
        byte_sub: streams.bytes.then(|| src.byte_fanout.subscribe()),
        non_coalesced: opts.non_coalesced,
        pace: Paces::default(),
        records_read,
        src: src.clone(),
        dirty: Arc::default(),
    }
}

/// The PUSH LOOP for a `subscribe` connection. The connection has already
/// AUTHORIZED every target via the control gate; here we just register for wakes,
/// emit an immediate catch-up (so a fresh subscriber sees the current screen) and
/// optionally honor `since=<seq>`, then block on the sessions' wakes and push a
/// coalesced frame on each wake until the client disconnects (write fails) or the
/// loop is asked to stop.
///
/// PUSH-ONLY: once here, the connection never reads another request line. The
/// writer is wrapped in the connection's one [`Egress`] and is the ONLY thing this
/// loop touches on the socket. A write failure (broken pipe / slow-then-dead client)
/// ends the loop and drops the [`Subscription`] (deregistering), so the producer
/// never pays for a dead subscriber.
///
/// `opts.since` (optional, applied per target): the client's last-seen
/// `content_seq`. If the live content has advanced past it, the first wake's compare
/// already emits a catch-up DELTA; we seed each watch's `last_sent_seq` to it so the
/// immediate catch-up fires exactly when content moved past `since`.
///
/// The two stream sets are SEPARATE parameters because they were authorized by two
/// different checks. `streams` is per-target: every entry of `targets` passed the
/// `ReadScreen` gate individually. `instance` is connection-wide and can only be
/// built by [`InstanceStreams::authorize`], which is why the roster read below
/// cannot be reached by a subscriber that only proved per-target authority.
/// The three authorization tokens a push connection carries, kept together
/// because they are checked in three different places and must travel as a set.
///
/// `streams` is PER-TARGET: every entry of `targets` passed the `ReadScreen` gate
/// individually. `instance` is CONNECTION-WIDE and can only be built by
/// [`InstanceStreams::authorize`]. `adopt` gates late-joining targets. Bundling
/// them is not just argument-count hygiene: a caller can no longer pass two of
/// the three and silently default the third, because the struct has no `Default`
/// and every field must be named at the construction site.
#[derive(Clone, Copy)]
pub(crate) struct PushScopes {
    /// Per-target `ReadScreen` authority, one check per entry of `targets`.
    pub streams: TargetStreams,
    /// Connection-wide authority; the only key to the whole-instance roster.
    pub instance: InstanceStreams,
    /// Whether targets that appear after subscription may be adopted.
    pub adopt: AdoptScope,
}

/// What a push connection advances as it runs over the instance: the roster
/// cursor (whole-instance lifecycle), the adopt sequence (late-joining
/// targets), the roster seq [`prune_closed`] last checked, and the instance
/// list's dirty flag. Bundled so [`pump`] takes one `&mut` — they are advanced
/// together, and a caller holding only some of them would be describing an
/// inconsistent point in the stream.
struct PushCursors {
    roster: Option<RosterCursor>,
    adopt_seq: u64,
    /// The roster seq at [`prune_closed`]'s last check; `None` before the
    /// first, which probes every watch.
    pruned_at: Option<u64>,
    /// The instance list's registration flag ([`Subscription::watch`]), when
    /// the subscription watches the list (the `sessions` stream or `@*`):
    /// every roster journal append sets it, and so does a prune that closes a
    /// watch ([`prune_closed`]); [`drain_membership`] runs only when it was
    /// set. `None` for a subscription with no membership to drain, and for a
    /// test that drives the membership pass by hand.
    instance_dirty: Option<Arc<AtomicBool>>,
    /// This loop's place among the `sessions` stream's readers, beside
    /// `roster` ([`RosterReader`]).
    reader: Option<RosterReader>,
}

/// THE INSTANCE STREAM'S READERS: every live push loop that carries the
/// `sessions` stream, by a key of its own, with the store it reads (by
/// address: a process has one, a test process many, each with its own
/// journal) and the newest roster-journal seq whose lines it has WRITTEN to its
/// client. The instance's last word, `EVENT * bye quit` (`fabric_launch::bye`),
/// is a journal row like any other, and the quit closes every connection soon
/// after it; so the quit waits, bounded, until every reader here has written
/// past it ([`await_roster_written`]) and a watcher learns the instance quit
/// instead of meeting a bare end of file.
struct RosterReaders {
    next: u64,
    /// By reader key: its store's address and the seq written.
    written: std::collections::BTreeMap<u64, (usize, u64)>,
}

static ROSTER_READERS: Mutex<RosterReaders> = Mutex::new(RosterReaders {
    next: 0,
    written: std::collections::BTreeMap::new(),
});

/// Notified whenever [`ROSTER_READERS`] moves: a reader wrote, joined or left.
static ROSTER_WRITTEN: std::sync::Condvar = std::sync::Condvar::new();

/// One push loop's place in [`ROSTER_READERS`], left when the loop ends (its
/// client gone, its targets closed).
struct RosterReader {
    key: u64,
    /// Its store, by address ([`store_key`]).
    store: usize,
    /// The seq last published for it.
    written: u64,
}

/// A store's key in [`ROSTER_READERS`]: its address, alive as long as any
/// reader of it is (each holds the store's `Arc`).
fn store_key(store: &Store) -> usize {
    Arc::as_ptr(store).addr()
}

impl RosterReader {
    /// Join `store`'s readers with `seq` already written. Called UNDER THE STORE
    /// READ GUARD the loop's cursor was seeded under: a row journaled after the
    /// seed is one this loop will drain and is waited for, and one journaled
    /// before is no part of its stream.
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

    /// The loop has written its client every row up to `seq`.
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

/// Wait up to `within` until every live `sessions` stream of `store` has
/// written its roster row `seq` (and everything before it) to its client, or
/// has ended; `true` when all had. The quit's `bye` ([`RosterReaders`]).
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

/// The longest one wait of a push loop that keeps a timer lasts: a loop whose
/// peer is probed on a timer ([`PeerLiveness::rings`]) probes it at least this
/// often, and a held membership decision ([`membership_owed`]) or timeline
/// drain ([`timeline_owed`]) is retaken this often. A pass that ends such a
/// wait reads only the content reads owed and due: nothing else changed unless
/// a list rang.
const LIVENESS: Duration = Duration::from_millis(250);

/// The least time between two content reads (`screen`, `cursor`, `cells`) that
/// wakes drive on one watch.
///
/// A content read takes the term lock to read the frame key, and on a moved
/// key builds the text or gathers the styled frame under it. During an output
/// flood every batch moves the key, so an unpaced watch would read, gather and
/// push once per batch, fighting the reader for the lock. A wake inside the
/// window is owed, not dropped: the loop's wait ends when the owed read comes
/// due, and that read sees the latest state, so the final state of a flood
/// always lands. `bytes`, `mail` and `log` are drained on every wake of their
/// session; `events` is paced the same way on the PTY reader's rings
/// ([`EVENTS_PACE`]), and read at once for a record.
pub(crate) const CONTENT_PACE: Duration = Duration::from_millis(16);

/// The least time between two `events` reads that the PTY reader's wakes drive
/// on one watch.
///
/// The PTY reader rings a session's watchers on every output burst
/// (`spawn::publish_burst`), and an `events` read takes the session's term
/// lock (its blocks, title, bell and resize counters: [`sample_engine_events`]),
/// then its turn ledger's and timeline's. During an output flood an unpaced
/// events watch would take all three once per burst, the term lock against the
/// reader writing the flood; the fabric bridge's `@* events,sessions` loop is
/// one. The whole digest is paced together, so one read still drains its
/// sources in their one order (blocks, turns, timeline, title, bell). A lone
/// burst is read at once; a burst's wake inside the window is owed and read
/// when it comes due, as [`CONTENT_PACE`]'s are.
///
/// A RECORD IS NEVER PACED. A timeline row, a booked resize row or a landed
/// turn rings as a record ([`Watchers::ring_record`]), and a pass that finds
/// the session's record count moved reads the digest at once, whatever the
/// window ([`pass_streams`]). Records are aterm's own, one per thing that
/// happened, never one per burst: the fabric's `post`, `inbox`, `inbox-seen`
/// and `post-landed` are records, and a ping-pong of them arrives every
/// millisecond or two, faster than any pace an output flood needs. Held to
/// this pace, each of its hops waited out the window, which set the fabric's
/// whole latency (2026-10-21, `docs/measured/fabric`). A read a record drives
/// opens the window like any other, so over a run of length T a watch takes
/// its session's locks at most ceil(T/EVENTS_PACE)+1 times for the reader's
/// rings plus once per record.
pub(crate) const EVENTS_PACE: Duration = CONTENT_PACE;

/// One watch's paced reads: its content streams' ([`CONTENT_PACE`]) and its
/// `events` digest's ([`EVENTS_PACE`]).
#[derive(Default)]
pub(crate) struct Paces {
    content: ReadPace,
    events: ReadPace,
}

impl Paces {
    /// When the first owed read comes due, if one is owed.
    fn due(&self) -> Option<Instant> {
        let content = self.content.due(CONTENT_PACE);
        let events = self.events.due(EVENTS_PACE);
        content.into_iter().chain(events).min()
    }
}

/// One paced read of a watch: when a wake last drove it, and whether one is
/// owed ([`CONTENT_PACE`], [`EVENTS_PACE`]).
#[derive(Default)]
pub(crate) struct ReadPace {
    /// When a wake last drove the read; `None` before the first.
    read_at: Option<Instant>,
    /// A wake arrived inside the pace window and its read is still owed.
    owed: bool,
}

impl ReadPace {
    /// Whether the pass at `now` makes the read: `Some(true)` for a read a wake
    /// drove (a genuine wake to `every-frame`), `Some(false)` for a liveness
    /// pass with no wake owed, `None` while a wake's read is paced.
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

    /// The read at `now` that a record drove ([`EVENTS_PACE`]): made whatever
    /// the window, and covering any read owed, since it reads the latest
    /// state. It opens a window as a paced read does, so the next burst's
    /// wake is paced from here.
    pub(crate) fn read_now(&mut self, now: Instant) {
        self.owed = false;
        self.read_at = Some(now);
    }

    /// When the owed read comes due, if one is owed.
    pub(crate) fn due(&self, pace: Duration) -> Option<Instant> {
        self.read_at.filter(|_| self.owed).map(|at| at + pace)
    }
}

/// How long the push loop waits for a ring before its next pass at `now`:
/// until the first owed paced read comes due, and never past [`LIVENESS`]
/// while the loop keeps a timer (`timed`: its peer is probed on one, or a held
/// membership decision or timeline drain waits to be retaken). `None` parks
/// until a ring: everything the loop reads rings a list it watches, the screen
/// included (a host's own write rings at the end of its hold,
/// [`crate::TermGuard`]) and its peer's hang-up too, so an idle subscription
/// makes no pass at all.
fn next_wait(watches: &[Watch], now: Instant, timed: bool) -> Option<Duration> {
    let owed = watches
        .iter()
        .filter_map(|w| w.pace.due())
        .min()
        .map(|due| due.saturating_duration_since(now));
    owed.into_iter().chain(timed.then_some(LIVENESS)).min()
}

/// How a push loop learns that its client hung up.
///
/// A push-only connection can stay quiet for hours, so a dead reader cannot be
/// discovered by waiting for the next write: that pins a push worker for as
/// long as the targets are quiet. [`Self::gone`] is asked after every wait. A
/// probe asked on a timer is any `FnMut() -> bool`; the control socket's push
/// streams use the peer watch on unix ([`crate::peer_watch`]), which rings a
/// list when the client goes, so the loop needs no timer for it.
pub(crate) trait PeerLiveness {
    /// Whether the client is gone. Never blocks.
    fn gone(&mut self) -> bool;

    /// The list rung when the client goes, while that ring is how the loop
    /// learns it: the loop watches it with its sessions' lists and keeps no
    /// timer for its peer. `None`: [`Self::gone`] is asked at least every
    /// [`LIVENESS`].
    fn rings(&self) -> Option<&Arc<Watchers>>;
}

/// A probe asked on a timer: a test's, or the read-side probe where no peer
/// watch runs.
impl<F: FnMut() -> bool> PeerLiveness for F {
    fn gone(&mut self) -> bool {
        self()
    }

    fn rings(&self) -> Option<&Arc<Watchers>> {
        None
    }
}

/// Whether a membership decision is owed with nothing to ring for it: the
/// instance flag is still set when the loop goes to wait. Only a held decision
/// leaves it so ([`push_held`]); a ring that sets it also leaves a wake pending.
fn membership_owed(cursors: &PushCursors) -> bool {
    cursors
        .instance_dirty
        .as_ref()
        .is_some_and(|dirty| dirty.load(Ordering::Acquire))
}

/// Whether a timeline drain is owed with nothing to ring for it: a watch's last
/// drain was held ([`Watch::timeline_held`]), and the hold's release rings no
/// list.
fn timeline_owed(watches: &[Watch]) -> bool {
    watches.iter().any(|w| w.timeline_held)
}

/// The production push loop, ended by `liveness` ([`PeerLiveness`]) when its
/// client hangs up. A test's probe is also the one hook the loop calls on its
/// own thread between a wake's wait and its closing pass, which is where a
/// test that must act at that instant acts.
///
/// `handshake` (`OK subscribe <n>` and one `sub <local> <sid>` line per target, formatted
/// by the control dispatch) is written once every watch is seeded, before the first
/// frame. A reader acting on a `sub` line (the fabric bridge reads that session's
/// `topic ls`) must not race the seed, or a change recorded between its read and the
/// seed would sit below the watermark and be pushed by nobody. Every `sub` line, the
/// handshake's and each adoption's ([`drain_membership`]), follows its watch's seed.
pub(crate) fn push_loop_with_peer_probe<W: Write, L: PeerLiveness>(
    store: &Store,
    targets: &[ResolvedTarget],
    scopes: PushScopes,
    opts: PushOptions,
    handshake: &str,
    writer: &mut W,
    mut liveness: L,
) {
    // `adopt` stays in `scopes`: `pump` decides adoption per wake, and a second read
    // here could diverge.
    let PushScopes {
        streams, instance, ..
    } = scopes;

    // INSTANCE lifecycle watermark: seed to the CURRENT journal high (and the
    // current live set, for the recovery path) so only spawns/exits AFTER
    // subscription are pushed — a fresh subscriber `ls`s for the baseline. Every
    // journal append rings the store's instance list, which this subscription
    // registers on below with its watches. This is the WHOLE-INSTANCE roster,
    // hence the `InstanceStreams` gate on reaching it at all.
    //
    // BOTH fields are read under ONE guard. That is not cosmetic: taking the
    // seq and the set in two separate lock acquisitions would let a spawn land
    // between them and be counted twice (once in the baseline set, once again
    // when the journal replayed it), which is exactly the class of bug a
    // watermark is supposed to remove.
    // The reader joins under the same guard ([`RosterReader::join`]).
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

    // `since` seeds `last_sent_seq`, so the catch-up fires exactly when content moved
    // past the client's last-seen seq; without it, 0 gets a full snapshot.
    let mut watches: Vec<Watch> = targets
        .iter()
        .map(|t| new_watch(t, streams, &opts))
        .collect();

    // REGISTER AFTER THE WATCHES ARE SEEDED, never before. Every live stream's
    // watermark — mail's cursor, the turn/block/timeline ids, title, bell — is
    // taken inside `new_watch`, and a row that lands between a register and
    // that read is folded into the SEED and pushed to nobody: registered means
    // "wakes reach me", and a wake that finds its own arrival already counted
    // as backlog is a wake wasted. Seeding first makes the registration the
    // real barrier it is documented to be. The instance list follows the
    // roster seed the same way: the roster stream and adoption both drain the
    // journal its appends ring (a wake, not an authority: what is adopted is
    // still `pump`'s decision).
    //
    // Each registration's flag starts set, so the catch-up below reads every
    // watch, and with it whatever landed between the watch's seed and its
    // registration; the instance flag's first pass replays the journal from
    // the adoption watermark's zero.
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
    // The peer's list too, while its hang-up rings it. Its flag is never
    // read: the loop asks `liveness` after every wait. A verdict that settles
    // before this registration rang nobody, and is read off `rings()` going
    // `None` at the next wait, whose bound then holds.
    if let Some(peer) = liveness.rings() {
        let _ = sub.watch(peer);
    }

    // The handshake, now that every watch is seeded (see the doc above). A failed write
    // is the client gone.
    // Each log-streaming target's `sub` line names where its stream starts,
    // read from the watch just seeded ([`with_log_anchors`]).
    let handshake = with_log_anchors(handshake, &watches);
    if !handshake.is_empty()
        && (writer.write_all(handshake.as_bytes()).is_err() || writer.flush().is_err())
    {
        return;
    }

    let mut egress = Egress::new(writer, opts.timestamps);
    // The adoption watermark starts at zero: the initial targets came from a store
    // snapshot the dispatch took before this point, so a session created in between is in
    // neither. The first pass replays the bounded retained journal once; a record for a
    // session already watched or gone resolves to nothing. `pump` ending in `Gone` is the
    // client hanging up, the normal end; dropping `sub` deregisters.
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

/// The push loop proper: the catch-up wake, then one wake per notify (or per bounded
/// timeout) until the client hangs up or every watched session has closed. Separate from
/// [`push_loop_with_peer_probe`] so `?` on the private [`Gone`] carries the "client is
/// gone" exit out of every write site.
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
    // The scope set travels whole, never re-split, so `pump` reads only what the
    // connection proved.
    let PushScopes { streams, adopt, .. } = scopes;
    let since_turn = opts.since_turn;
    // The catch-up is a wake like any other, so it opens one: its frames share a
    // stamp ledger.
    egress.begin_wake();

    // A `since-turn=<n>` anchor below the ledger's retained low means turn records between
    // the anchor and the window are gone (evicted, or not carried by a self-update
    // handoff). Say so: turn ids are process-global, so a per-session id gap cannot reveal
    // the loss. Resume anchors are single-target, so at most one watch matches.
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

    // IMMEDIATE catch-up: emit the current state once so a fresh subscriber is not
    // blind until the next output burst. With `since`, this fires a DELTA only if
    // content already advanced past `since`; without it, it sends the full screen.
    // Every watch's flag is still set from its registration, so this is a pass
    // that reads them all: a genuine first emit (an every-frame subscriber gets
    // its opening cells frame), and the content read that opens each pace
    // window. The `bytes` stream has no backlog to replay; it drains what was
    // teed since the watch's seed.
    visit_watches(watches, streams, egress)?;

    loop {
        // The wait: on a ring we push at once (content and events reads at most
        // once per pace per watch, the owed one when it comes due, and an events
        // read for a record at once whatever the pace), and every
        // LIVENESS while the loop keeps a timer ([`next_wait`]). A loop whose
        // peer rings its hang-up and owes no read parks until a ring. The
        // producer never waits on us regardless (single-slot notify), so the
        // bound only paces OUR reads, not the producer. A pass reads what rang
        // and what is owed, whichever ended the wait.
        let timed =
            liveness.rings().is_none() || membership_owed(cursors) || timeline_owed(watches);
        let _ = sub.wait(next_wait(watches, Instant::now(), timed));
        // A quiet stream performs no writes, so only the socket's read side
        // can release its push worker after the reader disappears. The client
        // contract keeps its write half open for the subscription's lifetime;
        // any byte or EOF on this push-only channel ends the stream.
        if liveness.gone() {
            return Err(Gone);
        }
        egress.begin_wake();

        // CLOSINGS FIRST. A target deregistered from the store (its pane closed) must
        // leave the watch set so we stop reading a dead engine — but its buffered TAIL
        // is delivered before it goes, because `prune_closed` hands the watch back
        // instead of dropping it and `Closing::drain` consumes it by value. Draining
        // here rather than in the live pass below also closes the residual window the
        // live pass leaves: bursts teed BETWEEN a live drain and the liveness re-check
        // were still lost. The close's own records (`closing`, `state-change`)
        // wake this pass, and bursts the reader teed after the last live drain
        // are still queued. Asked on every pass, not only on a dirty watch's: a
        // pass that read a watch in the instant its close rang has consumed
        // that flag, and the close is then in the journal and on no flag. The
        // roster seq makes the question one compare until something leaves.
        for closing in prune_closed(store, watches, cursors) {
            // Leave the session's watcher list too. Harmless-but-wasteful on a
            // fixed-target subscription (it ends soon after its last target
            // dies); load-bearing on a `@*` one, which outlives an unbounded
            // number of sessions and would otherwise hold a slot on every dead
            // session's list.
            sub.unwatch(closing.watchers());
            egress.send(closing.drain(streams))?;
        }

        // MEMBERSHIP (connection-level, once per wake). ADOPT: a `@*`
        // subscription's target set is LIVE — sessions created after it
        // subscribed join it here, each acked with the same `sub <local> <sid>`
        // line the initial handshake emits, so a client demultiplexes an
        // adopted channel exactly the way it does an original one. And the
        // INSTANCE lifecycle: a sibling spawn/exit the subscriber is not
        // watching, so a fleet supervisor need not poll `ls`. One pass under one
        // store read, so no plain announcement comes before its watch exists,
        // and one the cap leaves unwatched says `watch=deferred` — see
        // [`drain_membership`]. It runs only on a pass the instance list rang.
        egress.send(drain_membership(
            store, watches, sub, streams, opts, adopt, cursors,
        ))?;
        // Written, so said: the roster rows this wake drained reached the client.
        if let (Some(reader), Some(cursor)) = (cursors.reader.as_mut(), cursors.roster.as_ref()) {
            reader.wrote(cursor.seq);
        }

        // The "everything closed" exit sits below both drains: the last watch's tail and
        // the `EVENT * session-exited` reporting its death come from the wake that finds
        // it. A `@*` subscription's empty set is not the end: the next session is adopted.
        if watches.is_empty() && !adopt.on() {
            return Ok(());
        }

        visit_watches(watches, streams, egress)?;
    }
}

/// Register `sub` on a watch's session list, as a reader of its screen when
/// the subscription has a content stream ([`Subscription::watch_frame`]): a
/// host's hold of the session's engine that moves its frame rings it then.
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

/// One pass over the watch set: each watch is read for what its flag and its
/// pace say ([`pass_streams`]). Per watch, the UTF-8 text/cells frames, then
/// the RAW binary byte frames, written together as one producer: writing per
/// watch keeps each session's frames contiguous, and the byte frames are
/// length-prefixed so a client demuxes text vs binary unambiguously.
///
/// The flag is swapped BEFORE the read, so a ring during the read sets it again
/// and the next pass reads the watch once more: a change is never left on a
/// flag this pass cleared after reading past it. A watch whose last timeline
/// drain was held ([`Watch::timeline_held`]) is read as if it rang: the records
/// the hold withheld rang their list before that drain, and no ring is left to
/// read them once the hold goes.
///
/// The session's record count ([`Watchers::records_rung`]) is loaded AFTER the
/// swap: a record's ring steps the count before it sets the flag, so a record
/// whose ring this pass found is in the count, and its read is made at once. A
/// record whose ring lands after the swap rings the next pass. The count the
/// read was made at is the watch's from then on, so a record that lands during
/// the read is past it.
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

/// What one pass reads of one watch; `None` reads nothing. Every content or
/// events read it allows is one a wake drove (a genuine wake to `every-frame`).
///
/// - A watch whose session rang (`dirty`) drains its `bytes`, `mail` and `log`,
///   and reads its content ([`CONTENT_PACE`]) and its `events` ([`EVENTS_PACE`])
///   paced: a read inside its window is owed.
/// - A watch whose session's record count moved since its last `events` read
///   (`recorded`: a timeline row, a booked resize row or a landed turn,
///   [`Watchers::ring_record`]) reads its `events` at once, whatever the
///   window, and that read covers any owed. Its content stays paced: a record
///   moves no screen.
/// - A watch that did not ring is read only for the paced reads owed and due,
///   whatever ended the wait.
///
/// Nothing else is read: every stream changes only with a ring of its
/// session's list, the screen included, which a host's own write rings at the
/// end of its hold ([`crate::TermGuard`]).
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

/// TEST-ONLY: whether one of [`drain_membership`]'s two decisions is HELD —
/// `adopt` ([`pick_adoptions`]) or `sessions` ([`drain_roster`]) — or a
/// watch's `timeline` drain ([`drain_timeline_events`]), because a file of
/// that name exists in the directory `$ATERM_TEST_PUSH_HOLD` names (or, under
/// a unit test, because its thread holds it: [`HELD_HERE`]). A held step is
/// skipped for that wake with its cursor untouched, and the loop keeps a timer
/// while one stands ([`membership_owed`], [`timeline_owed`]), so the first
/// [`LIVENESS`] pass after the file goes resumes from that cursor: it replays
/// what the journal or the timeline retains past it, and reports what the ring
/// evicted meanwhile the way a slow reader's is reported — the timeline's as a
/// `GAP`, the roster's by its rebuild from the live set. The two membership
/// holds are read before the pass takes its store guard (they stat a file,
/// which has no business under the store lock); the `timeline` hold is read
/// under the session's timeline lock ([`timeline_held`] takes its guard),
/// because exactness needs that read there and the stat costs only in a debug
/// build with the seam set.
/// It never holds a closing watch's final pass.
///
/// THE STATES IT HOLDS OPEN ARE REAL, and only how long they last is chosen.
/// A session registered in the store is listed by it at once and adopted
/// (`sub <local> <sid>`, its watch seeded at the timeline's high) only on the
/// push loop's next pass, which the registration's journal append wakes — so
/// a reader of the store's roster (an attach, a `GAP`, a roster round) can
/// read a session before its watch exists. And at [`crate::control::MAX_SUBSCRIBE_TARGETS`]
/// a session is announced `watch=deferred` and adopted only when a slot
/// frees, if ever. Holding `adopt` while `sessions` runs is that deferral
/// without 256 sessions: the pass announces the session marked
/// `watch=deferred`, which is true, and acks it on the first pass after the
/// file goes. A watch whose reader stops draining — a stopped bridge, a full
/// socket on a busy node — keeps its timeline watermark while the ring moves
/// on; holding `timeline` is that state without the race between the push
/// loop's last drain and the record a test wants to fall in the hole, for the
/// timeline's own order and its `GAP` count only. The hold withholds the
/// timeline stream alone, so a held record reaches the wire after the frames of
/// other streams (and `sub` acks) made after it, an order a stalled socket
/// never writes; a test may rely on the timeline's order and the `GAP`, nothing
/// across streams. A test
/// that raced these states from outside would be the flake this codebase
/// refuses, so it holds each open and closes it at a named point.
///
/// The states it holds open are real; only their length is chosen. A session the store
/// lists is adopted only on the next wake, up to a 250 ms tick later, and at
/// [`crate::control::MAX_SUBSCRIBE_TARGETS`] a session is announced `watch=deferred` until
/// a slot frees: holding `adopt` while `sessions` runs is that deferral without 256
/// sessions. No hold can split the two decisions, which still share one guard.
///
/// TEST-ONLY IS ENFORCED, not documented: the variable is read only in a build
/// with `debug_assertions` — aterm-link's `Fault` rule — and only through
/// `dev_seam!`, which compiles to `None` in a release build, so a released
/// binary has no hold to arm.
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

/// A release build holds nothing: see the debug twin.
#[cfg(not(debug_assertions))]
const fn push_held(_step: &str) -> bool {
    false
}

/// Whether the test-only `timeline` hold ([`push_held`]) stands, read under the
/// session's timeline lock: it takes the guard, so the read cannot move out from
/// under the lock that makes the hold exact ([`frames_for_watch`]).
fn timeline_held(_under: &crate::session_timeline::SessionTimeline) -> bool {
    push_held("timeline")
}

#[cfg(test)]
thread_local! {
    /// TEST-ONLY: the steps a unit test holds ([`push_held`]), per thread.
    /// The marker directory is the process's, named once, and every unit test
    /// shares the process, so a marker one test wrote would hold every other
    /// test's push loop; a unit test runs its push loop on its own thread.
    static HELD_HERE: std::cell::Cell<&'static [&'static str]> = const { std::cell::Cell::new(&[]) };
}

/// The block watermark a watch starts from: the `since-block=` anchor, lowered to the
/// engine's newest completed block (`live`), since an anchor past it names no block of
/// this engine. With no block completed yet, such an anchor is dropped.
fn resume_block_watermark(anchor: Option<u64>, live: Option<u64>) -> Option<u64> {
    match (anchor, live) {
        (Some(anchor), Some(live)) => Some(anchor.min(live)),
        (Some(_), None) => None,
        (None, live) => live,
    }
}

/// Seed the turn watermark to the ledger's current high so the `events` digest
/// streams only turns that COMPLETE after subscription (live, never the backlog —
/// mirrors [`EngineSeed::newest_block`]). `None` when `events` was not requested.
fn initial_turn_watermark(turns: &Arc<Mutex<TurnLedger>>, streams: TargetStreams) -> Option<u64> {
    if !streams.events {
        return None;
    }
    turns.lock().unwrap_or_else(|p| p.into_inner()).high_id()
}

/// The timeline twin of [`initial_turn_watermark`], under one guard: the
/// timeline scan seeds at the CURRENT timeline high, so only post-subscription
/// records (a `meta-change`, the `closing` row) push — never the `spawned`
/// history — and the resize watermark at the resize ledger's high, so only
/// resizes AFTER the subscription push. `(None, 0)` when events are off.
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

/// Which live sessions this `@*` subscription should ADOPT now — every one it
/// is not already watching, up to the cap — driven by the STORE'S ROSTER
/// JOURNAL, so an idle wake costs one integer compare and a busy one costs
/// O(sessions created). Returns each as the [`ResolvedTarget`] [`new_watch`]
/// takes, cloned out of `g`, with its sid; [`drain_membership`] builds the
/// watches.
///
/// THE COST CATEGORY THIS DELETES. `subscribe` froze its target list at
/// subscribe time, and the push loop is documented PUSH-ONLY — it never reads
/// another request line — so there was NO way to add a session to a live
/// subscription. A federation bridge that wanted a newly-opened tab therefore
/// had to open a WHOLE NEW subscription for it: a fresh child process, a fresh
/// UDS connection, and a fresh server push thread, once per DISCOVERY MOMENT
/// rather than once per instance. The server then admitted four of those at a
/// time, so the fifth tab-open was refused — and refused silently, because a
/// push-only client that has already marked the sid as seen never asks again.
/// Past four staggered discoveries, sessions simply stopped being federated.
/// One live target set replaces all of it with one connection per instance,
/// holding one subscription lane (`control_subscriptions_cap`) however many
/// tabs it follows.
///
/// [`crate::control::MAX_SUBSCRIBE_TARGETS`] bounds it as it bounds an explicit selector
/// list. At the cap adoption is deferred, not dropped: the watermark stays, so a later
/// wake with a free slot picks the session up. The `sessions` stream has already announced
/// it `watch=deferred` ([`drain_roster`]) and the adopting wake's ack is the rest. A
/// session the cap left over when `@*` arrived is in the stream's baseline, so its ack is
/// the only news.
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
        // Only the sids the journal says were created. An `Exited` record needs nothing
        // (`prune_closed` retired that watch), and a created sid that has since exited
        // resolves to `None`, so replaying a stale watermark is safe.
        Some(low) if adopt_seq.saturating_add(1) >= low => g
            .roster_since(*adopt_seq)
            .filter(|r| r.change == crate::session_store::RosterChange::Created)
            .filter_map(|r| g.by_sid(&aterm_session::SessionId::new(&r.sid)))
            .filter(|h| unwatched(h))
            .map(carve)
            .collect(),
        // Recovery: the watermark fell past the journal's retained window (or the journal
        // rolled before the first pass), so walk the registry once.
        _ => g
            .live_handles()
            .filter(|h| unwatched(h))
            .map(carve)
            .collect(),
    };
    // Advance only if everything was taken: a watermark moved past a truncated batch
    // would leave the deferred sessions behind it, never retried.
    if picked.len() <= room {
        *adopt_seq = high;
    } else {
        picked.truncate(room);
    }
    picked
}

/// One wake's membership pass: adopt the sessions a `@*` subscription is not yet watching
/// ([`pick_adoptions`]) and drain the instance roster ([`drain_roster`]) under one store
/// read, then seed the adopted watches before the caller writes a frame of either.
///
/// One read leaves no instant for a registration to land between the two decisions: one
/// landing after it is above both cursors, and a later pass decides both at once. Every
/// `session-created` in the batch is then for a session gone again, already watched,
/// adopted by this pass, or deferred at [`crate::control::MAX_SUBSCRIBE_TARGETS`], which
/// [`drain_roster`] marks `watch=deferred`. The adoptions are seeded after the guard drops
/// and before return, and the loop writes the `sub <local> <sid>` acks before the roster
/// lines, so a reader acting on an announcement or an ack acts after that session's watch
/// was seeded. The law is aterm-spec's `SubscribeAnnouncementOrder`, bound here by
/// `membership_passes_refine_the_announcement_model`.
///
/// A deferred session is announced at once, since a slot may never free; the marker keeps
/// the line from claiming a watch. It rides one line a reader may drop, so a consumer that
/// must track unwatched sessions treats every session it holds no `sub` ack for as
/// unwatched, as the fabric bridge does (aterm-link `bridge.rs`, `Bridge::watched`).
///
/// DISCIPLINE. Clone-then-release: handles are cloned out under the store read
/// guard, which is dropped before `new_watch` takes any `Terminal` lock and
/// before any write. A subscription that neither adopts nor asked for the
/// `sessions` stream takes no read at all, and neither does a pass the
/// instance list did not ring: both decisions read only the journal and the
/// watch set, and every journal append rings the list.
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
    // Both decisions read only the journal and the watch set: every journal
    // append rings the instance list, and a prune that frees a slot sets the
    // flag itself ([`prune_closed`]). Swapped before the read, so an append
    // after it rings the next pass.
    if let Some(dirty) = &cursors.instance_dirty
        && !dirty.swap(false, Ordering::AcqRel)
    {
        return Vec::new();
    }
    // The test-only holds ([`push_held`]) are read BEFORE the guard: they
    // stat a file, which has no business under the store lock. A held
    // decision leaves the flag set, so the first pass after the file goes
    // takes it, ring or no ring.
    let (adopt_held, roster_held) = (push_held("adopt"), push_held("sessions"));
    let held = (adopt.on() && adopt_held) || (cursors.roster.is_some() && roster_held);
    if held && let Some(dirty) = &cursors.instance_dirty {
        dirty.store(true, Ordering::Release);
    }
    let (t_us, picked, roster) = {
        let g = store.read().unwrap_or_else(|p| p.into_inner());
        // The pass's instant: both decisions read the store under this guard.
        let t_us = crate::metrics::now_us();
        let picked = if adopt.on() && !adopt_held {
            pick_adoptions(&g, watches, &mut cursors.adopt_seq)
        } else {
            Vec::new()
        };
        // Which of them the journal says were CREATED after the subscription
        // resolved its targets ([`PushOptions::adopt_after`]): their `log`
        // streams read from the start. One whose record the journal no longer
        // holds cannot be told from a session left over by the cap, and
        // starts live.
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
        // The guard drops here, before `new_watch` takes any Terminal lock.
    };
    let mut out = Vec::with_capacity(picked.len() + 1);
    for (target, sid, fresh) in picked {
        // SEED, THEN REGISTER — the main path's rule (see its "REGISTER AFTER
        // THE WATCHES ARE SEEDED"). A screen's content would survive either
        // order, since a wake re-reads it whole; an event stream's would not: a
        // timeline row recorded between a register and the seed is folded into
        // the seed and pushed to nobody (the bridge's `fetch` event was, in the
        // 2026-10-06 world-suite red). A burst that lands between the seed and
        // the registration rings nobody here, and loses nothing: the
        // registration's flag starts set, so this pass's per-watch visit, which
        // follows, reads the new watch.
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

/// A watch whose session has left the store but whose buffered tail is not yet emitted.
/// Made only by [`prune_closed`] and consumed only by [`Closing::drain`], which takes
/// `self` by value, so the [`ByteSubscription`] inside drops strictly after its queue is
/// read and no reordering can discard the queued bursts and the final content DELTA.
#[must_use = "a closing watch still holds an undelivered tail — call drain()"]
struct Closing(Watch);

impl Closing {
    /// The watcher list of the session inside — read BEFORE `drain` consumes
    /// the watch, so the subscription leaves it in the same pass.
    fn watchers(&self) -> &Arc<Watchers> {
        &self.0.src.watchers
    }

    /// The final frames for a dying target, in protocol order: one last content /
    /// events pass, then every byte burst still queued, then `EVENT <sid> exited`.
    ///
    /// Reading the target's `Terminal` AFTER the store deregistered it is deliberate
    /// and sound: `Watch.term` is an independent `Arc` clone taken at subscribe time,
    /// and [`crate::session_store`] records the death mark BEFORE the handle leaves
    /// the registry precisely so a holder that kept the handle — naming "a live
    /// subscribe watch" — still reads an honest final event. That final event is
    /// the `closing reason= by=` row, and the events pass here is the only path
    /// that delivers it (`EVENT <sid> closing …`, then the `exited` marker below):
    /// the `timeline` verb cannot resolve a deregistered sid, so a driver that was
    /// not already subscribed asks `exits` instead.
    ///
    /// `woke = false`: this pass exists to deliver state the client has not SEEN, not
    /// to re-emit. An `every-frame` re-emit at an unchanged seq would only append a
    /// duplicate frame to a session that is already gone.
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

/// Partition the watch set by liveness: live watches stay in `watches`, and the ones
/// whose session has been DEREGISTERED from the store (their pane closed) are HANDED
/// BACK as [`Closing`] values rather than dropped. It decides only WHO died — it lost
/// its `TargetStreams` parameter along with the `EVENT … exited` it used to format,
/// because deciding what a dead target still owes the client is `Closing::drain`'s
/// job, and keeping the two together is what makes the tail impossible to skip.
///
/// THE JOURNAL SAYS WHETHER TO LOOK. Only a deregistration retires a session,
/// and each one appends an `Exited` row to the roster journal under the write
/// that removes the handle. So a roster seq equal to `pruned_at` (the seq of
/// the last check) means no watch died: one compare, no probe, which is every
/// pass but the few after a membership change. Past it, the journal's rows
/// since `pruned_at` say whether an `Exited` names a watched local id, and only
/// then are the watches probed. Before the first check, or once the journal
/// has rolled past `pruned_at`, every watch is probed: the journal cannot
/// answer, and a session that closed before this subscription registered is
/// in no row it will read.
///
/// A PROBE ASKS FOR THE SESSION, NOT THE ID: a watch is live while its local id
/// resolves to the context whose watcher list it holds. A local id that left
/// and was registered again between two checks resolves, but to another
/// session, and the old watch is handed back.
///
/// A CLOSE IS A MEMBERSHIP INPUT. A watch handed back frees a slot, and a
/// session deferred at [`crate::control::MAX_SUBSCRIBE_TARGETS`] waits on one,
/// so a prune that closes anything sets the instance flag and the membership
/// pass that follows runs ([`drain_membership`]). The `Exited` row's own ring
/// does not cover it: the session can leave between a pass's prune and its
/// membership pass, which then takes that ring with the watch still in the
/// set and no room, and the slot frees only at the next pass's prune.
///
/// `mem::take` + `partition` rather than `retain` for the same reason: `retain` drops
/// the removed watches in place, and a dropped `Watch` drops the `ByteSubscription`
/// it owns, whose `Drop` frees every burst teed since the last drain.
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

/// Test fixture for the `events` digest: K watched targets whose ledgers are filled to a
/// requested depth, woken through the shipping [`frames_for_watch`].
#[cfg(test)]
pub(crate) mod digest_fixture {
    use super::{
        PushOptions, ResolvedTarget, TargetStreams, Watch, WatchSource, frames_for_watch, new_watch,
    };
    use crate::session_timeline::SessionTimeline;
    use crate::turn_ledger::{TurnLedger, TurnRecord};
    use aterm_core::terminal::Terminal;
    use std::sync::{Arc, Mutex};

    /// One shell-integration command cycle: prompt, command, output, exit 0.
    /// Two archived rows per cycle, one `OutputBlock` per cycle.
    const CYCLE: &[u8] =
        b"\x1b]133;A\x07$ \x1b]133;B\x07cmd\r\n\x1b]133;C\x07out\r\n\x1b]133;D;0\x07";

    /// K watched targets whose three per-target event ledgers are filled to a requested
    /// retained depth, the variable the digest's cost tracks.
    pub(crate) struct DigestFixture {
        watches: Vec<Watch>,
        streams: TargetStreams,
        terms: Vec<Arc<Mutex<Terminal>>>,
        turns: Vec<Arc<Mutex<TurnLedger>>>,
        timelines: Vec<Arc<Mutex<SessionTimeline>>>,
        next_turn_id: u64,
    }

    impl DigestFixture {
        /// `targets` watches, each carrying `blocks` shell blocks, `turns` turn records
        /// and `timeline` timeline events. Depths above the shipping caps saturate;
        /// [`Self::retained`] reports what was reached.
        ///
        /// Not named `new`: the lock-order census resolves a held one-hop call by callee
        /// name, so a `fn new` here would capture every `Vec::new()` made under a held
        /// `term` guard and report a false OB-7 re-entrancy suspect.
        pub(crate) fn build(targets: usize, blocks: usize, turns: usize, timeline: usize) -> Self {
            let mut f = DigestFixture {
                watches: Vec::with_capacity(targets),
                // Events only: the cost priced is an idle events-only wake.
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
                // `fixture_term`, not `term`: the census keys identity on the receiver
                // name, so `term` would merge this private engine into the production
                // lock graph.
                let fixture_term = Arc::new(Mutex::new(Terminal::new(24, 80)));
                {
                    let mut t = fixture_term.lock().expect("fresh engine");
                    for _ in 0..blocks {
                        t.process(CYCLE);
                    }
                    // A settled title, so the title watermark seeds to a real value.
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
                        // A mix: the digest filters for `meta-change`, so an all-meta
                        // fixture would flatter a whole-ring scan and an all-other one
                        // would never reach the emit path.
                        if j % 8 == 0 {
                            t.record_raw_meta_change("field=role value=lead".to_string());
                        } else {
                            t.record("state-change", "state=alive".to_string());
                        }
                    }
                }
                // Seeded by the shipping seed, every watermark at the live
                // high, so only what happens AFTER this point is pushed.
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

        /// ONE wake: the shipping per-target body for every watch, as a pass
        /// in which every watched session rang reads them. Returns the total
        /// frame bytes produced — `0` is what an idle wake must produce.
        pub(crate) fn wake(&mut self, woke: bool) -> usize {
            let mut bytes = 0usize;
            for w in &mut self.watches {
                for f in frames_for_watch(w, self.streams, woke) {
                    bytes += f.segs.iter().map(|seg| seg.bytes().len()).sum::<usize>();
                }
            }
            bytes
        }

        /// What the fixture reached on target 0, `(blocks, turns, timeline events)`: the
        /// reach guard, since unfilled ledgers would price an empty scan.
        pub(crate) fn retained(&self) -> (usize, usize, usize) {
            // Named locals, not `self.terms[0]`: the lock-order census takes a lock's
            // identity from the receiver name, and an index expression has none. The
            // `fixture_` prefix keeps them out of the production identities.
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

        /// Land one new turn record on every target: the other half of the reach guard,
        /// after which a wake must produce frames.
        pub(crate) fn land_turn(&mut self) {
            let mut id = self.next_turn_id;
            // Named, not `l`: the census cannot identify a single-letter receiver either.
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

    /// A small, realistic turn record (the ledger clamps text itself).
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
    /// A resumed `since-block=` anchor replays the blocks after it but never rises above
    /// the engine's newest completed block, so a foreign anchor cannot hide new blocks.
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

    /// THE INSTANCE-WIDE GRANTS ARE OWNER-ONLY, at the type level: `@*`
    /// adoption and the `sessions` stream both name sessions the subscriber
    /// could not have named, and the per-target `ReadScreen` gate resolves only
    /// the selectors the client named. Neither capability has a constructor
    /// that yields a permissive value from a non-Owner scope, so a future
    /// caller that forgets the refusal in `run_subscribe_with_peer_probe` still
    /// cannot grant it; and an Owner that did not ask does not get it.
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

    /// A `@*` subscription adopts a session created after it subscribed: acked with the
    /// handshake's `sub <local> <sid>` line, registered for wakes, and seeded live, so it
    /// replays no backlog.
    #[test]
    fn adoption_picks_up_a_session_created_after_subscribe() {
        let store = crate::session_store::new_store();
        let mut sub = Subscription::new();
        let mut watches: Vec<Watch> = Vec::new();
        // No `sessions` stream: this pins adoption alone.
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

    /// `@* log` PUSHES A NEW SESSION'S OPENING: a session the roster journal
    /// says was created after the subscription resolved its targets is news to
    /// it, so its `log` stream reads from its log's start, and its first `LOG`
    /// line is `open - spawn` (seq 1, the only record naming its lineage),
    /// which an adoption seeded at the head skipped with no `GAP`. Each `sub`
    /// line names where its stream starts (` log_seq= log_ep=`). NEGATIVE
    /// CONTROL: a session that was there when the subscription resolved its
    /// targets (one the cap left over) starts live and replays nothing.
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

    /// A [`Watch`] on `term` seeded the way a subscription with no resume anchor
    /// seeds one: every send cursor at its "nothing sent yet" sentinel. Tests
    /// override only the field they are actually varying (`..watch_on(..)`), so what
    /// a case is testing stays visible instead of being buried in a 15-field literal
    /// repeated eighteen times. Its source is a fresh session's (empty ledger,
    /// timeline and inbox), built without locking `term`, so a literal may build it
    /// while another field holds the engine's guard.
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

    /// Resize the watch's engine to `rows` and book the journal the way the
    /// window pass does (copied under the resize's hold, ingested after it), at
    /// `at_ms` on the ledger's clock.
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

    /// The resize ledger on the `events` digest: a net-zero alt-screen flap before the
    /// next wake is one `EVENT resize n=2`; the `render desync-risk` transition is pushed
    /// once, from the timeline; an evicted-past watermark is `dropped=` on the frame,
    /// never a `GAP`. The flap is booked at a far-future ledger time, so only the test
    /// decides when the one-second risk is reached.
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
        // A mode set between the halves (output that draws nothing) drops the engine's
        // resize undo without closing the ledger's run, so the grow appends and the
        // screen stays shifted; a quiet flap is undone and would never turn.
        crate::term_lock(&term).process(b"\x1b[?1000h");
        ledgered_resize(&w, 8, far);
        // The app's diff frame lands on the shifted rows (it read 8 == 8).
        crate::term_lock(&term).process(b"\x1b[8;1HF1\x1b[K\x1b[6;3H");
        let first = wake_text(&mut w, streams, true);
        assert_eq!(first, "EVENT 7 resize n=2 id=2 to=20x8 run=1\n");
        assert_eq!(
            wake_text(&mut w, streams, false),
            "",
            "coalesced, not repeated"
        );

        // A second on the ledger's clock: the verdict turns, recorded once.
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

        // The app clears and repaints: healed, pushed once.
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

        // A watch that fell behind the ring: the hole is said on the frame.
        // Seventy flaps a second apart: seventy runs (2..=71).
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

    /// The wire bytes a producer's frames carry, in emission order, without what
    /// [`Egress`] prepends, so frame-shape tests stay independent of the stamp policy.
    fn frame_bytes(frames: &[Frame]) -> Vec<u8> {
        frames
            .iter()
            .flat_map(|f| &f.segs)
            .flat_map(|seg| seg.bytes().iter().copied())
            .collect()
    }

    /// [`frame_bytes`] decoded — the text streams are all UTF-8 by construction.
    fn text(frames: &[Frame]) -> String {
        String::from_utf8(frame_bytes(frames)).expect("text frames are UTF-8")
    }

    /// One wake's text for a watch: the shape every coalescing case asserts on.
    fn wake_text(w: &mut Watch, streams: TargetStreams, woke: bool) -> String {
        text(&frames_for_watch(w, streams, woke))
    }

    /// Everything an [`Egress`] wrote for `frames`, one producer in one wake, stamps
    /// included — the actual wire bytes. `Vec<u8>` is a `Write`, so no socket is
    /// involved.
    fn wire(timestamps: bool, frames: Vec<Frame>) -> Vec<u8> {
        let mut sink: Vec<u8> = Vec::new();
        let mut egress = Egress::new(&mut sink, timestamps);
        egress.begin_wake();
        egress.send(frames).expect("a Vec sink never hangs up");
        sink
    }

    /// A per-target [`TargetStreams`] with the given fields set, for the terse
    /// `requested(..)` expectations below.
    fn targets_of(screen: bool, cursor: bool, events: bool) -> TargetStreams {
        TargetStreams {
            screen,
            cursor,
            events,
            ..Default::default()
        }
    }

    /// The `mail` grammar: `mail:kinds=task,ask` arrives split on the list's commas, and
    /// [`fold_mail_params`] rejoins it ("a piece that names no stream continues the mail
    /// token"; a stream name ends the fold). An unknown kind or key, a repeat or an empty
    /// value fails closed, since a filter matching nothing would look like a quiet stream.
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

        // The fold ends at the next stream name, and the streams around it still parse.
        let both = got("screen,mail:kinds=task,ask,cursor");
        assert_eq!(both.mail, mail(&["task", "ask"], None));
        assert!(both.targets.screen && both.targets.cursor && both.targets.mail);

        // `mail:topic=` narrows to one broadcast face and composes with the other two in
        // any order.
        assert_eq!(got("mail:topic=build.failed").mail, topic("build.failed"));
        assert_eq!(
            got("mail:topic=build.failed:kinds=note").mail,
            crate::fabric::MailFilter {
                kinds: vec!["note".to_string()],
                from: None,
                topic: Some("build.failed".to_string()),
            }
        );

        // `mail` alone is a frame source, so it satisfies the no-source rule.
        assert!(Requested::parse("mail").is_some());

        for bad in [
            "mail:kinds=nope",           // not one of KINDS
            "mail:kinds=",               // empty value
            "mail:from=",                // empty value
            "mail:bogus=1",              // unknown key
            "mail:kinds=task:kinds=ask", // repeated
            "mail:from=a:from=b",        // repeated
            "mail:topic=",               // empty value
            "mail:topic=Build",          // not the topic grammar
            "mail:topic=a/b",            // nor is this
            "mail:topic=a:topic=b",      // repeated
            "mailish",                   // not this stream, not any stream
            // `from=` IS SINGULAR: the fold that reassembles `kinds=`'s comma
            // list hands it a second piece, and `mail:from=h-a,h-b` was taken
            // verbatim and matched NOTHING.
            "mail:from=h-andrew,h-bob",
            "mail:from=human,agent",
            "mail:from=andrew", // no class prefix, not a class
            "mail:from=h-",     // a prefix and nothing else
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

    /// The gap frame as wire text: `GAP <local> mail-dropped=<n>`, one line before the
    /// wake's MAIL lines, counting what the ring lost (with class eviction, not
    /// "everything below the front row": see `fabric`'s
    /// `mail_since_reports_what_was_evicted_even_when_the_front_row_is_pinned`).
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
        // A subscriber that joined before any of it.
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

    /// Stream parsing: a subset of the known streams parses, comma or space
    /// separated. `trim` and `ts`/`timestamps` are MODIFIERS: beside a frame
    /// source they land on the per-target set (`trim`) or outside both stream
    /// sets (`timestamps`, which restamps frames and authorizes none); alone, or
    /// with only other modifiers, they name no source and fail closed, as an
    /// empty list, a typo or one bad token does — otherwise the connection acks
    /// `OK subscribe 1` and stays silent forever, indistinguishable from a hang.
    /// `sessions` alone is a frame source (an instance-scoped one): it parses
    /// here and is REFUSED later, by authority, not by the grammar.
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

    /// A screen DELTA is emitted only when `content_seq` advances; a viewport scroll emits
    /// nothing. Each frame is `<sid>`-tagged.
    #[test]
    fn screen_delta_on_content_change_none_on_viewport_scroll() {
        let term = Arc::new(Mutex::new(Terminal::new(24, 80)));
        let streams = TargetStreams {
            screen: true,
            ..Default::default()
        };
        let mut w = watch_on(4, &term);

        // The first wake emits a catch-up DELTA tagged with our sid (4).
        crate::term_lock(&term).process(b"hello");
        let f1 = wake_text(&mut w, streams, true);
        assert!(
            f1.starts_with("DELTA 4 seq="),
            "sid-tagged screen delta: {f1:?}"
        );
        assert!(f1.contains("hello"), "delta carries the live screen text");
        let seq_after_write = w.last_sent_seq;
        assert!(seq_after_write > 0);

        // A pure viewport scroll bumps no content_seq and emits nothing.
        crate::term_lock(&term).scroll_display(1);
        let f2 = wake_text(&mut w, streams, true);
        assert!(f2.is_empty(), "viewport scroll produces no delta: {f2:?}");
        assert_eq!(
            w.last_sent_seq, seq_after_write,
            "seq unchanged by a scroll"
        );

        // A real content change advances the seq and re-emits a delta.
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

    /// `trim` cuts a screen DELTA after the last non-blank row (interior blanks kept), by
    /// `text trim`'s rule, and `screen <nrows>` counts the rows sent. Without it the frame
    /// is the whole grid.
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

    /// A FRESH WATCH STARTS AT EVERY LIVE HIGH. A session with history in every
    /// source a watch seeds from — a completed block, a title, a bell, a turn, a
    /// timeline record, a resize, a mail row, the alternate screen — is watched
    /// through the shipping seed ([`new_watch`]): its catch-up replays none of
    /// that history, and its screen catch-up is a DELTA, not an alt-screen
    /// resync. NEGATIVE CONTROL: the same session watched from the unseeded
    /// cursors pushes every one of them.
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
        // Booked far ahead on the ledger's clock, so the digest's real-clock
        // evaluation records no verdict of its own.
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

    /// `since=<seq>` SEMANTICS at the frame level, through the shipping seed
    /// ([`new_watch`]): `since=` the CURRENT content seq means a fresh wake emits
    /// NOTHING (the client is already caught up, and the seed starts every frame
    /// key at the live one); `since=` BELOW it emits an immediate catch-up DELTA.
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

    /// CURSOR STREAM completeness: a pure caret move (CSI H) and a DECTCEM
    /// hide bump no `content_seq`, so a seq-gated stream would never push
    /// them. The `cursor` stream emits a DELTA on each anyway (its whole
    /// purpose), carrying the `<visible>` bit, matching the poll `cursor` verb.
    #[test]
    fn cursor_stream_emits_moves_and_visibility_on_unchanged_seq() {
        let term = Arc::new(Mutex::new(Terminal::new(24, 80)));
        crate::term_lock(&term).process(b"abc"); // content advances; cursor at (0,3)
        let streams = TargetStreams {
            cursor: true,
            ..Default::default()
        };
        let mut w = watch_on(5, &term);
        // First wake syncs the cursor watermark off the content advance.
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

    /// A RENDER-ONLY change mutates the styled output WITHOUT bumping
    /// `content_seq`, so a seq-gated `cells` subscriber would show stale state:
    /// a recolor (OSC 11 default background; OSC 4 is allowed by default in
    /// the app, but the engine default refuses it), DECSCNM reverse video,
    /// the CELLS frame's own cursor object (position, DECSCUSR shape, OSC 12
    /// colour) for a subscriber without the `cursor` stream, and a stationary
    /// selection's OSC 17/19 colours. The per-wake render signature notices
    /// each and re-emits a `cells` DELTA at the unchanged seq, with no GAP; an
    /// unchanged render stays coalesced.
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
        // First wake emits cells and syncs the frame-key watermark.
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

    /// A config reload of the host STYLE POLICY (`bold_is_bright`,
    /// `faint_opacity`) re-colours bold and dim cells with no output and no seq
    /// move. The hold that applies it moves the frame, so its end rings the
    /// session's screen readers ([`crate::TermGuard`]; `App::install_colors`
    /// is that hold, pinned by `a_projection_change_rings_every_sessions_watchers`),
    /// and the pass that ring drives re-emits cells at the unchanged seq.
    /// Negative control: applying the config it already has rings nobody, and
    /// the pass after it reads nothing.
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
        // One pass of the loop at `at`: the watch is read for what its flag
        // and its pace say.
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

    /// A selection change is render-only: a coalesced cells-only subscriber still gets the
    /// typed, side-adjusted selection (sparse blank tail included), and a clear removes
    /// the old rectangle at the same seq.
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

    /// An alt-screen swap (1049) leaves the main grid's `content_seq` alone, so a flag
    /// flip forces a resync (GAP + full frame) even at an equal seq.
    #[test]
    fn alt_screen_flip_forces_resync_even_at_equal_seq() {
        let term = Arc::new(Mutex::new(Terminal::new(24, 80)));
        crate::term_lock(&term).process(b"\x1b[?1049h"); // enter the alternate screen
        let (seq, alt) = {
            let t = crate::term_lock(&term);
            (t.content_seq(), t.is_alternate_screen())
        };
        assert!(alt, "1049h enters the alternate screen");
        let streams = TargetStreams {
            screen: true,
            ..Default::default()
        };
        // Caught up at this seq but on the main buffer (`last_alt == false`): the alt
        // flip is the only change.
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

    /// What the push gate before the frame key compared, as of a watch's last
    /// frame: `content_seq`, the alt flag and the render signature.
    fn gate_inputs_before_the_frame_key(t: &Terminal) -> (u64, bool, u64) {
        (
            t.content_seq(),
            t.is_alternate_screen(),
            crate::frame_key::render_sig(t),
        )
    }

    /// NEGATIVE CONTROL: the push gate before the frame key, verbatim in what it
    /// decided — one "content changed" bit for every stream, set when any of
    /// [`gate_inputs_before_the_frame_key`] moved.
    fn content_changed_before_the_frame_key(then: (u64, bool, u64), t: &Terminal) -> bool {
        gate_inputs_before_the_frame_key(t) != then
    }

    /// A FRAME CHANGE THAT WRITES NO CONTENT PUSHES `cells`. A Kitty image placed
    /// with `C=1` (the cursor does not move) changes the styled frame and moves
    /// neither `content_seq`, the alt flag nor the render signature, so the push
    /// gate before the frame key — the negative control — saw no change, while
    /// `screen since=` (the same frame, keyed on the etag) did. The frame key
    /// moves, and the frame is re-sent at the unchanged seq with no GAP.
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
        // Transmit-and-display a 2x1-pixel RGBA image over 2x1 cells, `C=1`, `q=2`.
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

    /// A BATCH THAT CHANGES NO FRAME PUSHES NO `cells`. The frame key folds
    /// `screen_mutations`, which every non-empty batch moves, so a batch that
    /// only sets the title (or rings the bell, answers a DSR, toggles a mode)
    /// moves the key at an unchanged seq: the frame is re-read, and its body is
    /// the one already sent. NEGATIVE CONTROL: the key moved, so the etag-only
    /// rule pushed the whole frame again, byte for byte, for each batch.
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

    /// ONE CHANGE IS GATHERED ONCE FOR EVERY READER. Two `cells` watches on one
    /// session and a `screen` read, after one write: the first watch gathers
    /// and publishes the frame, and the second watch and the read are served
    /// it, byte for byte. NEGATIVE CONTROL: the same three readers with no
    /// `cells` hold on the session (nothing keeps the frame) gather three times.
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

    /// A `screen` READ PINS NO FRAME, AND THE LAST `cells` WATCH FREES IT.
    /// With no `cells` watch on the session, two reads of one unchanged screen
    /// gather twice: the first read stored nothing. With a watch, a read is
    /// served the frame it published; once the watch is dropped, the frame goes
    /// with it and the next read gathers again.
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

    /// EACH STREAM ASKS ITS OWN QUESTION. A caret move (BS) is a batch of output
    /// that moves the caret and no glyph: the `cursor` stream pushes the new
    /// caret, and the `screen` stream, whose text is unchanged, pushes nothing.
    /// The text key did move (the premise), so the rows were read and their
    /// hash compared, not skipped. NEGATIVE CONTROL: the gate before the frame
    /// key saw a change (the caret is in the render signature) and re-sent the
    /// whole screen beside the cursor.
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

    /// THE EPOCH IS IN THE KEY, NEVER IN THE GAP RULE. A TUI that leaves and
    /// re-enters the alternate screen and draws a second dialog with as many
    /// writes reaches the first dialog's seq on the same buffer: the gate before
    /// the frame key (the negative control) saw no change and never pushed the
    /// second dialog. The epoch moved, so the text key did, and the new screen
    /// is pushed as a plain DELTA — the seq did not go back and the buffer did
    /// not change, so there is no GAP. A region scroll steps the epoch too (RI at
    /// the region's top); it is pushed with no GAP either, since the fabric
    /// bridge reads every GAP as a recovery trigger.
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

    /// The stamp is the egress's, not one producer's: a `bytes`-only subscription with
    /// `timestamps` gets its `BYTES` bursts stamped.
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

        // NEGATIVE CONTROL: the stamp is absent when the modifier is off (else
        // `starts_with("T 7 ")` would be testing the `BYTES` header's own shape),
        // and it appears exactly ONCE per tag per wake (else "stamped" would be
        // satisfied by a per-frame stamp, a different, chattier contract).
        fan.tee(&Arc::from(&b"a"[..]), 0);
        let unstamped = String::from_utf8(wire(false, drain_bytes_frames(&w))).unwrap();
        assert_eq!(
            unstamped, "BYTES 7 1\na\n",
            "without the modifier the stream is byte-identical to the untimed one"
        );

        // Two frames on one tag in one wake share a stamp; the instance tag `*` is
        // another channel and gets its own.
        let tagged = String::from_utf8(wire(
            true,
            vec![
                Frame::text(Tag::Channel(7), 2, "EVENT 7 bell total=1\n".to_string()),
                Frame::text(Tag::Channel(7), 1, "BYTES 7 1\nz\n".to_string()),
                Frame::text(Tag::Instance, 3, "EVENT * session-exited s-a\n".to_string()),
            ],
        ))
        .unwrap();
        // Counted by line prefix: `EVENT 7 …` contains the substring `T 7 `.
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

    /// THE STAMP IS THE SAMPLE INSTANT. A content frame is dated inside the
    /// read that built it, and its `T` line says exactly that instant however
    /// much later it is written. Within one producer a tag's stamp is the
    /// earliest instant among its non-empty frames (an empty frame neither
    /// consumes nor dates it), a later producer on the same tag in the same
    /// wake writes no second stamp, and the next wake stamps again. The
    /// instants are far beyond any test process's uptime, so a stamp read off
    /// the clock at write time cannot equal them.
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

    /// A `T` CAN STEP BACK on a channel that mixes `bytes` with content. The
    /// reader dates a burst before it waits for the term lock and tees it
    /// after, so a burst that arrived before a screen read drains on the next
    /// wake dated before that read, and its stamp is written as is.
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

    /// A writer that counts its `write_vectored` calls and takes at most `cap`
    /// bytes per call: a short write when `cap` is small.
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

    /// A WATCH IS ONE VECTORED WRITE. A pass over a watch that rang for its
    /// screen, its caret, its title and two bursts writes all of it with one
    /// `write_vectored`, every byte in order, the bursts from the reader's own
    /// allocations. A writer that takes five bytes a call gets the same wire:
    /// a short write resumes mid-slice. NEGATIVE CONTROL: the same frames
    /// written one frame per send, as the per-frame egress wrote them, take
    /// one call per frame.
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

    /// A `cells` FRAME WRITES THE PUBLISHED BODY BY REFERENCE. Two `cells`
    /// watches on one session, after one write: both frames carry the very
    /// frame the session's slot publishes, and the wire writes that body,
    /// never a copy of it.
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

    /// A `cells` DELTA carries the lossless styled-screen JSON, length-prefixed.
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
        // The length prefix matches the JSON body byte length.
        let header = f.lines().next().unwrap();
        let nbytes: usize = header.rsplit(' ').next().unwrap().parse().unwrap();
        let body = &f[header.len() + 1..f.len() - 1]; // between header\n and trailing \n
        assert_eq!(body.len(), nbytes, "length prefix matches body: {f}");
    }

    /// The events digest pushes `EVENT <sid> meta <payload>` for each `meta-change`
    /// record past the watermark, skips records the wire does not carry (`spawned`,
    /// `state-change`), and advances the watermark to the timeline high, so nothing
    /// double-emits or is re-walked. A watermark seeded at subscription replays nothing.
    #[test]
    fn meta_change_drains_as_a_meta_event_and_watermark_advances() {
        let timeline = std::sync::Arc::new(std::sync::Mutex::new(
            crate::session_timeline::SessionTimeline::for_test(),
        ));
        // Pre-subscription history: a spawn and an old meta change.
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

        // Nothing new: no frames, watermark stays.
        let mut out = String::new();
        let wm = drain_timeline_events(&timeline.lock().unwrap(), "7", seeded, &mut out);
        assert!(
            out.is_empty(),
            "no replay of pre-subscription history: {out}"
        );
        assert_eq!(wm, Some(2));

        // A post-subscription meta change and a non-meta record: only the meta record
        // pushes, and the watermark passes both.
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
        // Drained again: silence (no double emit).
        let mut out = String::new();
        let wm2 = drain_timeline_events(&timeline.lock().unwrap(), "7", wm, &mut out);
        assert!(out.is_empty());
        assert_eq!(wm2, Some(4));
        // A person started typing (`app_input::note_person` records the
        // `human` row once per burst): `EVENT <local> human`, bare — no
        // trailing space, nothing a parser must strip.
        timeline.lock().unwrap().record("human", String::new());
        let mut out = String::new();
        drain_timeline_events(&timeline.lock().unwrap(), "7", wm2, &mut out);
        assert_eq!(out, "EVENT 7 human\n");
    }

    /// A worker's task record moved (`task_record::mark_session_task`): the digest pushes
    /// `EVENT <local> task rev=<n> state=<s> attention=<0|1> [why=] [exit=]`,
    /// the record's own payload — no result text, no path. A mark that
    /// changes nothing records nothing, so nothing is pushed for it.
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

    /// A watermark below the drop-oldest ring's retained low is a hole, counted as
    /// `GAP <sid> events-dropped=<n>` before the retained rows (the twin of mail's
    /// `mail-dropped=`), so a bridge can re-read what a lost `topic` row would have said.
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
        // Within the ring: the record, no GAP.
        timeline
            .lock()
            .unwrap()
            .record("topic", "add t since=head".to_string());
        let mut out = String::new();
        let wm = drain_timeline_events(&timeline.lock().unwrap(), "7", seeded, &mut out);
        assert_eq!(out, "EVENT 7 topic add t since=head\n");
        assert_eq!(wm, Some(2));
        // Past the ring: the ring keeps the newest TIMELINE_CAP of these.
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
        let missed = low - 2 - 1; // ids 3 ..= low-1 are gone
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
        // Drained again: silence.
        let mut out = String::new();
        drain_timeline_events(&timeline.lock().unwrap(), "7", wm, &mut out);
        assert!(out.is_empty());
    }

    /// The exit ledger on the wire, end to end through [`push_loop_with_peer_probe`]: an
    /// `events` watch whose session a control-socket `close` deregisters gets exactly the
    /// store's `closing reason=ctl-close by=<caller>` row, then `exited`, and nothing else
    /// (the `state-change state=closed` beside it is no wire kind). The close runs inside
    /// the peer probe, which the loop calls only after seeding and the catch-up wake, so
    /// the row is a push, and it pins that the row is written at deregistration.
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

    /// Hold `steps` on this thread ([`HELD_HERE`]) until the guard drops.
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

    /// A subscriber socket a test reads while the push loop writes to it: the
    /// loop's peer probe runs on the loop's own thread, between its writes.
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

    /// THE `timeline` HOLD STANDS IN FOR A STALLED READER (for the timeline's
    /// own order), AND A STALLED READER'S TAIL IS LATE, NEVER LOST. While the
    /// hold stands, a live wake drains nothing off
    /// the timeline and leaves the watermark where it was, so the record made
    /// on the first wake is still unwritten at the second. A session that
    /// leaves the store under the hold gets its closing pass all the same — a
    /// stopped reader's socket keeps that pass's frames until it reads again —
    /// so the held record and the `closing` row both precede `exited`. A hold
    /// that also held the closing pass loses both; an inert one writes the
    /// record on the first wake.
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

    /// A LIFTED `timeline` HOLD IS DRAINED ON THE NEXT PASS, WITH NO FURTHER
    /// RING. One record is read; then, under the hold, the ring moves past the
    /// watch's watermark, and the pass those records rang withholds them and
    /// marks the watch, which keeps the loop's timer ([`timeline_owed`]). Once
    /// the hold goes nothing has rung since that pass, and the next pass writes
    /// the `GAP` (the one record the ring evicted) and every record the ring
    /// retains. NEGATIVE CONTROL: the same watch with its mark cleared after
    /// the held pass, as a held drain once left it, is read by no later pass
    /// and keeps no timer: its records wait for a ring that never comes.
    #[test]
    fn a_lifted_timeline_hold_is_drained_on_the_next_pass_with_no_ring() {
        use crate::session_timeline::TIMELINE_CAP;
        let streams = TargetStreams {
            events: true,
            ..Default::default()
        };
        // Each pass comes past the events pace window ([`EVENTS_PACE`]), as the
        // loop's next pass does when it waits for an owed read: what a pass
        // reads here is what rang and what the hold marked, never the pace.
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

    /// A peer whose hang-up rings a list, as [`RingingPeer`]'s does, whose
    /// liveness check runs `check` on the loop's own thread after each wait,
    /// handed the count of checks so far. It never reports the peer gone.
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

    /// A HELD TIMELINE DRAIN KEEPS A TIMER, AND THE PASS AFTER THE HOLD GOES
    /// READS WHAT IT WITHHELD. The bridge's shape: an events-only loop whose
    /// peer's hang-up rings, so with no hold standing its wait has no timer.
    /// The loop's thread holds `timeline` from the start, so its catch-up's
    /// drain is held. Its first liveness check, which only that drain's timer
    /// reaches, records on the session (the one ring after the catch-up); its
    /// second lifts the hold. The record reaches the wire and the sink hangs up
    /// on it. A held drain that kept no timer parks the loop before the record,
    /// and one that left its watch clean is never read once the hold goes:
    /// either way the join's hang detector fails the test.
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

    /// A subscriber socket whose reader STALLS once the wire holds `at` bytes.
    /// `seeded` runs on its first write, the handshake, which the loop writes
    /// once its watch is seeded and registered. The write that crosses `at` is
    /// cut short there, and the next one is where the push loop blocks: `stall`
    /// runs inside it — on the loop's own thread, so while the loop is in
    /// `Egress::emit` with the watermark at the high its drain read — and the
    /// call is refused (`Interrupted`). From then on the reader takes three
    /// bytes a call. It hangs up once the wire ends with `last`: it takes no
    /// byte more, and says so as the peer watch says it of a real socket, by
    /// setting `gone` and ringing `wakes` ([`RingingPeer`]), which ends the
    /// loop's wait at once.
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

    /// Everything a stalled reader ([`Stalling`]) gets from the shipping push
    /// loop after its handshake: one `meta-change` record made as the handshake
    /// is written, which the catch-up pass reads, and `n` more made while the
    /// loop is blocked writing that record's frame. No pass waits for a timer
    /// but the events pace: the records ring the session, the hang-up rings
    /// the peer, and the join is the hang detector.
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

    /// A STALLED READER'S RECORDS COME AFTER THE STALL, OR IN ITS GAP. A push
    /// loop blocked writing to a reader that stopped draining has already moved
    /// the watch's watermark to the high its drain read, and the egress writes
    /// every frame whole however the reader paces it — cut short, refused,
    /// three bytes a call. So a record made during the block is above the
    /// watermark: the next drain pushes it, or counts it in the `GAP` before
    /// the retained ones if the ring evicted it meanwhile. That is the state
    /// the test-only `timeline` hold stands in for. A watermark moved past what
    /// was drained, a frame cut at the short write, or a hole with no `GAP`
    /// each changes the wire.
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

    /// The `bytes` stream drains every burst byte-exactly (non-UTF-8 included) as
    /// length-prefixed `BYTES` frames, with no coalescing.
    #[test]
    fn bytes_drain_is_byte_lossless_and_every_frame() {
        let fan = Arc::new(ByteFanout::new());
        let term = Arc::new(Mutex::new(Terminal::new(2, 4)));
        let bs = fan.subscribe();
        fan.tee(&Arc::from(&b"\x1b[31m"[..]), 0);
        fan.tee(&Arc::from(&[0x80u8, 0x00][..]), 0); // non-UTF-8 + NUL
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

    /// A queue overflow surfaces as a counted `GAP … bytes-dropped=` before the
    /// surviving (newest) bursts.
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

    /// The `events` digest emits `EVENT <sid> title <pct>` once per title change, never on
    /// an unchanged re-scan, pct-encoded so a space or newline stays on one line.
    #[test]
    fn drain_title_event_emits_once_per_change_pct_encoded() {
        let term = Arc::new(Mutex::new(Terminal::new(24, 80)));
        // OSC 2 sets the window title to "my dir" (a space -> pct-encoded).
        crate::term_lock(&term).process(b"\x1b]2;my dir\x07");
        let mut out = String::new();
        // Sampled through the shipping sampler, so the lock hold is covered too.
        let title = |last: Option<&str>| sample_engine_events(&term, None, last).title;
        // First drain with no watermark emits the current title.
        let sampled = title(None).expect("no watermark must sample a title");
        let wm = drain_title_event("3", Some(sampled), None, &mut out);
        assert!(
            out.contains("EVENT 3 title my%20dir\n"),
            "title emitted + pct-encoded: {out:?}"
        );
        // A re-scan with the same title emits nothing.
        out.clear();
        let sampled = title(wm.as_deref());
        assert!(sampled.is_none(), "an idle sample is no change");
        let wm = drain_title_event("3", sampled, wm, &mut out);
        assert!(out.is_empty(), "unchanged title emits nothing: {out:?}");
        assert_eq!(wm.as_deref(), Some("my dir"), "keep the watermark");
        // A new title emits again.
        crate::term_lock(&term).process(b"\x1b]2;other\x07");
        out.clear();
        let sampled = title(wm.as_deref());
        let wm = drain_title_event("3", sampled, wm, &mut out);
        assert!(
            out.contains("EVENT 3 title other\n"),
            "change re-emits: {out:?}"
        );
        assert_eq!(wm.as_deref(), Some("other"));

        // Clearing a title is a real change, distinct from an unchanged sample.
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

    /// The block half of [`sample_engine_events`] (the O(1) early-out and the reverse
    /// walk) agrees with the forward `all_blocks().filter(is_complete && id > wm)` scan
    /// at every watermark, including across a block archived without completing (a
    /// Ctrl-C'd prompt), which a walk stopping on `!is_complete()` would truncate.
    #[test]
    fn sample_engine_events_blocks_match_the_forward_filter() {
        let term = Arc::new(Mutex::new(Terminal::new(24, 80)));
        let run = |bytes: &[u8]| {
            crate::term_lock(&term).process(bytes);
        };
        // Two completed commands, an abandoned prompt, then a third completed one.
        run(b"\x1b]133;A\x07$ \x1b]133;B\x07one\n\x1b]133;C\x07o\n\x1b]133;D;0\x07");
        run(b"\x1b]133;A\x07$ \x1b]133;B\x07two\n\x1b]133;C\x07t\n\x1b]133;D;1\x07");
        run(b"\x1b]133;A\x07$ \x1b]133;B\x07abandoned");
        run(b"\x1b]133;A\x07$ \x1b]133;B\x07three\n\x1b]133;C\x07x\n\x1b]133;D;0\x07");

        // The forward filter, kept as the oracle.
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

        // Every watermark from below the floor to past the ceiling, plus `None`.
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

        // The two arms the digest stands on, named explicitly.
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

    /// The cursors of a push loop before its first pass, with no `sessions`
    /// stream and no instance list watched.
    fn fresh_cursors() -> PushCursors {
        PushCursors {
            roster: None,
            adopt_seq: 0,
            pruned_at: None,
            instance_dirty: None,
            reader: None,
        }
    }

    /// A cursor seeded at the current journal high, as [`push_loop_with_peer_probe`] seeds one.
    fn roster_cursor(store: &Store) -> RosterCursor {
        let g = store.read().unwrap_or_else(|p| p.into_inner());
        RosterCursor {
            seq: g.roster_seq(),
            known: g.live_sids(),
        }
    }

    /// A BYE RIDES THE INSTANCE STREAM, each one once and in order, as
    /// `EVENT * bye <word>`, and moves no membership: it is the instance's
    /// word to its bridge, not a session coming or going.
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

    /// THE QUIT'S LAST WORD IS ON THE WIRE BEFORE ITS WAIT ANSWERS. A
    /// `sessions` push loop (the bridge's scopes: `subscribe @* events,sessions`)
    /// joins its store's readers as it seeds, and a quit's `bye quit` journalled
    /// after that is waited for ([`await_roster_written`]) until the loop has
    /// WRITTEN it: when the wait answers `true` the line is in the sink. A
    /// reader of another store holds nothing up; one that never writes past
    /// its store's bye makes that wait answer `false` at its bound (the
    /// control), and once it leaves, `true`.
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
        // Another store's reader that never writes.
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

    /// A sibling that both spawns and exits between two wakes surfaces as both events, in
    /// order, on the next wake: the journal records each transition. The negative control
    /// is a snapshot diff over the same two instants, which must see nothing.
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

        // Negative control: a snapshot diff over the same instants sees nothing.
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
        // The cursor advanced once: a second drain is silent.
        assert!(
            drain_session_events(&store, &mut cursor).is_empty(),
            "a caught-up cursor emits nothing"
        );
    }

    /// A cursor past the journal's drop-oldest low-water rebuilds and diffs the whole set,
    /// reporting only the net change and leaving the cursor caught up.
    #[test]
    fn a_cursor_past_the_journal_low_water_rebuilds_and_diffs() {
        use crate::session_store::ROSTER_JOURNAL_CAP;
        let store = crate::session_store::new_store();
        // A cursor from before anything happened...
        let mut cursor = RosterCursor {
            seq: 0,
            known: std::collections::HashSet::new(),
        };
        // ...then churn far past the cap. One session (id 0) survives.
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

    /// The `events` digest emits `EVENT <sid> bell total=<n>` when the fired-bell count
    /// advances, and nothing on an unchanged re-scan.
    #[test]
    fn drain_bell_event_emits_on_new_bells() {
        let term = Arc::new(Mutex::new(Terminal::new(24, 80)));
        let mut out = String::new();
        let bell = |t: &Arc<Mutex<Terminal>>| sample_engine_events(t, None, None).bell;
        // No bells yet: baseline 0, nothing emitted.
        let wm = drain_bell_event("4", bell(&term), 0, &mut out);
        assert!(out.is_empty() && wm == 0, "no bell yet: {out:?}");
        // A BEL fires: total advances to 1, event emitted.
        crate::term_lock(&term).process(b"\x07");
        out.clear();
        let wm = drain_bell_event("4", bell(&term), wm, &mut out);
        assert!(
            out.contains("EVENT 4 bell total=1\n"),
            "bell emitted: {out:?}"
        );
        // A re-scan with no new bell emits nothing.
        out.clear();
        let _ = drain_bell_event("4", bell(&term), wm, &mut out);
        assert!(
            out.is_empty(),
            "unchanged bell total emits nothing: {out:?}"
        );
    }

    /// The `events` digest surfaces new TURN records as `EVENT <sid> turn …`
    /// lines, scanning the ledger by id watermark exactly like block-complete:
    /// a record past the watermark emits once and advances it; a re-scan with no
    /// new record emits nothing (no double-report). Each line names who typed
    /// it, `history`'s `by=`: the window harness's own turn reads `by=harness`
    /// whatever its status, beside a manager's `by=ctl`.
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

        // Seed the watermark at the current high (a live stream ignores backlog).
        push(1, true, "settled");
        let mut wm = ledger.lock().unwrap().high_id();
        let mut out = String::new();
        wm = drain_turn_events(&ledger, "3", wm, &mut out);
        assert!(
            out.is_empty(),
            "backlog before subscription is not replayed"
        );

        // A new turn emits one digest line and advances the watermark.
        push(2, false, "timeout");
        out.clear();
        wm = drain_turn_events(&ledger, "3", wm, &mut out);
        assert_eq!(
            out,
            "EVENT 3 turn 2 submitted=0 status=timeout dur_ms=7 by=ctl\n"
        );
        assert_eq!(wm, Some(2));

        // Re-scan with nothing new: silent.
        out.clear();
        let wm2 = drain_turn_events(&ledger, "3", wm, &mut out);
        assert!(out.is_empty() && wm2 == Some(2), "no double-report");

        // The harness's own turn, a `turn` fallback's `settled` included.
        push_by(3, true, "settled", TurnBy::Harness);
        out.clear();
        let _ = drain_turn_events(&ledger, "3", wm2, &mut out);
        assert_eq!(
            out,
            "EVENT 3 turn 3 submitted=1 status=settled dur_ms=7 by=harness\n"
        );
    }

    /// `every-frame` re-reads the `cells` frame on every genuine wake, frame key
    /// moved or not, and sends it only when the body changed: an unchanged frame
    /// is sent once. NEGATIVE CONTROL: the rule before the body id re-sent on
    /// every genuine wake (`non_coalesced && woke`), and what it re-sent is the
    /// body already sent, byte for byte.
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
        // The re-read stands: a body the watch has not sent (a fabricated last
        // id, as for a change the key does not fold) is sent on a genuine wake
        // while the key stands.
        w.last_cells_body = Some((0, 0));
        assert!(
            wake_text(&mut w, streams, true).ends_with(&format!("\n{reread}\n")),
            "every-frame sends a body that differs from the last one sent"
        );
        // R5 efficiency: a read no wake drove (`woke == false`: the closing
        // drain's) re-serializes nothing, even for a body the watch has not
        // sent, which a genuine wake would send.
        w.last_cells_body = Some((0, 0));
        assert!(
            wake_text(&mut w, streams, false).is_empty(),
            "every-frame must not re-emit on a read no wake drove",
        );
    }

    /// THE PACE, AS THE LOOP DRIVES IT: `wakes` wakes `every` apart, and between
    /// them the loop's wait ends when an owed read comes due ([`next_wait`]).
    /// Returns the reads the wakes drove, the span from the first wake to the
    /// last read, and whether the last read came at or after the last wake.
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

    /// A FLOOD OF WAKES READS CONTENT AT MOST ONCE PER PACE, AND THE LAST WAKE
    /// IS READ. 200 wakes 1 ms apart drive at most ceil(T/PACE)+1 reads, and the
    /// last read comes at or after the last wake, so the final state lands.
    /// NEGATIVE CONTROL: with no pace, every one of the 200 wakes reads.
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

    /// What [`events_rings`] saw of one events watch's reads.
    #[derive(Debug, PartialEq, Eq)]
    struct EventsReads {
        /// Reads made in the pass their own ring started.
        at_once: u32,
        /// Of those, the reads of a ring that brought a record.
        records: u32,
        /// Reads made for an owed read come due: a read that waited out the
        /// pace.
        owed: u32,
        /// Whether the last read came at or after the last ring.
        last_landed: bool,
        /// From the first ring to the last read.
        span: Duration,
    }

    /// ONE EVENTS WATCH'S RINGS, AS THE LOOP DRIVES THEM: `rings` rings `every`
    /// apart, ring `i` a record's (the session's record count moved:
    /// [`Watchers::ring_record`]) when `record(i)` and otherwise the PTY
    /// reader's, and between them the loop's wait ends when an owed read comes
    /// due ([`next_wait`]). Every pass is [`pass_streams`]'s, on a clock the
    /// test owns.
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

    /// A TRICKLE OF RECORDS IS READ ONCE PER RECORD, AND NONE WAITS. Fifty
    /// records 1 ms apart, each after the read before it (the fabric's
    /// ping-pong rings its sessions about that often): every one is read in the
    /// pass its own ring starts, and no read is ever owed. NEGATIVE CONTROL:
    /// the same trickle under main's rule since 7eebb21923, where every ring
    /// was paced alike (`ReadPace::visit` at [`EVENTS_PACE`], which is what
    /// [`pass_streams`] still calls for a ring with no record), reads one ring
    /// a window at once and holds the rest until the window ends.
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

    /// AN OUTPUT FLOOD WITH RECORDS AMONG ITS RINGS STAYS PACED, AND EACH
    /// RECORD IS READ AT ONCE. 2000 rings 100 µs apart, every 50th a record's
    /// (R = 40): the digest is read at most ceil(T/EVENTS_PACE)+1+R times, every
    /// record in its own ring's pass, and the last ring is read, so the final
    /// state lands. NEGATIVE CONTROL: were every ring a record's (no source
    /// told apart), all 2000 would read.
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

    /// AN OUTPUT FLOOD THROUGH THE REAL LOOP. After the catch-up, N batches each
    /// rewrite the screen and wake the watch; the screen DELTAs number at most
    /// ceil(T/PACE)+1 for the loop's whole run T, and the last one is the final
    /// screen. The loop ends when it is written (the sink hangs up); the hang
    /// detector panics on a final screen that was never pushed.
    #[test]
    fn a_flooded_screen_watch_is_paced_and_ends_on_the_final_screen() {
        const N: usize = 2000;
        let store = crate::session_store::new_store();
        let term = Arc::new(Mutex::new(Terminal::new(2, 16)));
        // Content past seq 0, so the catch-up pushes a screen.
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

    /// AN EVENTS WATCH UNDER AN OUTPUT FLOOD, THROUGH THE REAL LOOP. Once the
    /// handshake says the watch is seeded, N bursts each write the engine and
    /// ring the session's watchers, as the PTY reader does, and the last sets the
    /// title. The events digest takes the flooding session's term lock
    /// ([`sample_engine_events`], counted where it takes it) at most
    /// ceil(T/EVENTS_PACE)+1 times over the loop's whole run T, and the final
    /// title is pushed. The loop ends when it is written (the sink hangs up); the
    /// hang detector panics on a final title that was never pushed.
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

    /// A TRICKLE OF RECORDS THROUGH THE REAL LOOP WAITS OUT NO PACE: the fabric
    /// bridge's shape, an events watch whose session's records arrive one
    /// after another. Each record is written the moment the one before it is
    /// on the wire, by the sink on the loop's own thread, so each lands inside
    /// the window the read before it opened. Every one is read in the pass its
    /// own ring starts: no read waits for an owed one to come due, and each
    /// read takes the session's term lock once. Under main's rule since
    /// 7eebb21923 every record after the first waited out the window; that
    /// negative control is
    /// `a_trickle_of_records_is_read_once_per_record_with_no_owed_wait`'s,
    /// on a clock the test owns, since this loop's clock is the machine's.
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

    /// AN OUTPUT FLOOD WITH RECORDS AMONG ITS BURSTS, THROUGH THE REAL LOOP. As
    /// [`a_flooded_events_watch_is_paced_and_ends_on_the_final_title`], and
    /// every 100th burst also writes a timeline record, R in all, before the
    /// burst's ring. The digest takes the flooding session's term lock at most
    /// ceil(T/EVENTS_PACE)+1+R times over the loop's run T, every record is
    /// pushed, and the final title lands, after the last record.
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

    /// A subscribe target on a session the store never knew — which is what a
    /// deregistered (pane-closed) session looks like to [`prune_closed`], since the
    /// only thing it asks is whether the local id still resolves to that session —
    /// with the session's byte fan-out, which the test tees its tail into.
    fn dead_target(local_id: u64) -> (Store, Vec<ResolvedTarget>, Arc<ByteFanout>) {
        let h = crate::session_store::test_handle_with(
            local_id,
            Arc::new(Mutex::new(Terminal::new(4, 8))),
        );
        let fan = h.ctx.byte_fanout.clone();
        (crate::session_store::new_store(), vec![target_of(&h)], fan)
    }

    /// 4.3 — the DATA-LOSS regression test. A session that closes with output still
    /// queued must deliver that tail, and deliver it BEFORE `EVENT <sid> exited`:
    /// `exited` is the client's end-of-stream marker, so anything after it is
    /// unreachable and anything dropped before it is silent loss (the protocol
    /// promises `GAP` as the ONLY loss marker). The same holds with `events` off
    /// (no `exited` frame at all): the "every watched session closed" exit sits
    /// BELOW the closing drain, or a `bytes`-only client would get a bare EOF with
    /// its last burst destroyed and no marker of any kind.
    ///
    /// The session here is already gone when [`push_loop_with_peer_probe`] starts
    /// (a subscribe target the store never knew — what a deregistered session
    /// looks like to [`prune_closed`]), so the first liveness pass classifies it
    /// dead before ANY live per-watch pass can run for it — which means every byte
    /// in the output below came from [`Closing::drain`] and from nowhere else. That
    /// is the exact ordering the old `Vec::retain` broke: it dropped the `Watch`,
    /// and with it the `ByteSubscription` holding the queue.
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

            // `push_loop` subscribes to the fan-out itself, and its first wake's
            // closing pass drains the dead watch: the burst must land between the
            // two. It is teed from the loop's OWN peer probe, which that wake asks
            // after its bounded wait and before the closing pass — an instant in
            // the loop's order, not a race against its 250 ms wait (a feeder
            // thread, descheduled past the wait, teed after the drain, into
            // nothing).
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

    /// PRUNE ASKS THE JOURNAL, THEN THE SESSION. The first check probes every
    /// watch; a check over an unchanged roster, or one moved only by a spawn or
    /// by an unwatched session's exit, probes none; and a local id that left
    /// and was registered again between two checks hands the old watch back.
    /// NEGATIVE CONTROL: the probe the journal replaced asked only whether the
    /// local id resolves, and the reused id does.
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

    /// A WAKE VISITS ONLY THE WATCH WHOSE SESSION RANG. Over four `events,mail`
    /// watches the catch-up reads all four; a wake whose ring the catch-up
    /// already read reads none; a ring of one session's list reads that watch
    /// alone. NEGATIVE CONTROL: a ring of every list reads all four, the pass
    /// every wake used to be. The handshake rings session 1 after the
    /// registration, so the first wait ends at once; each later ring is made
    /// from the loop's own peer probe, between a wait and the pass it ends.
    /// `mail` drains on every ring, so a watch whose `events` read is still
    /// inside its pace window ([`EVENTS_PACE`]) is visited for its ring all the
    /// same.
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

    /// A HOST'S SELECTION RINGS ITS SESSION, HOWEVER BUSY ANOTHER IS. Two
    /// `cells` watches. The handshake rings A after the registration, and the
    /// loop's own peer probe rings A again before every pass, so every wait
    /// ends on A's wake and none times out; on its first call the probe
    /// selects text on B under the term lock, a write the PTY reader never
    /// makes, which reaches B's watch only on its hold's ring
    /// ([`crate::TermGuard`]): a pass reads no watch that did not ring. The
    /// scenario ends when B's cells DELTA carries the selection; the probe is
    /// also the hang detector. NEGATIVE CONTROL: the selection moves no
    /// `content_seq`, so no seq compare can stand in for the ring.
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
        // Set by B's cells header, so the payload line that follows is B's.
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

    /// ONLY A CONTENT STREAM ARMS ITS SESSION'S HOLDS. A push loop watches a
    /// session as a reader of its screen only for `screen`, `cursor` or
    /// `cells`; an `events`, `bytes` or `mail` subscription (the bridge's `@*
    /// events,sessions` watches every session so) leaves a host's mutable hold
    /// of that terminal reading no frame key, and a selection ringing nobody.
    /// Each content stream is the negative control: it arms the hold, and the
    /// selection rings it. The real loop's registration is read as its
    /// handshake is written; the ring, on `watch_session`'s own registration,
    /// which the membership pass's adoptions share.
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

    /// WHAT A PASS READS OF ONE WATCH: when its session rang, its `mail`, `log`
    /// and `bytes` at once and its content and `events` reads paced; otherwise
    /// only the paced reads owed and due. A watch that did not ring and owes
    /// nothing is not read, however long since its last read and before its
    /// first alike: its registration's flag is its first ring, and every later
    /// change rings.
    ///
    /// An events watch under a flood: a ring inside its window reads nothing,
    /// so it takes no lock of the flooding session, and its read is owed. The
    /// negative control: a mail watch's ring inside the same window still drains
    /// its mail, so the ring did reach the pass. A pass that finds the
    /// session's record count moved reads the digest at once inside the same
    /// window, and only the digest.
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
        // A watch whose last paced reads were at t0.
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

        // A RECORD IS READ AT ONCE: inside the window a burst's ring is paced
        // in, a ring that brought a record reads the digest, and the owed read
        // goes with it; the content beside it stays paced.
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

    /// WHEN THE LOOP WAITS WITHOUT A BOUND: whenever nothing it reads needs a
    /// timer. With a peer whose hang-up rings, the wait is `None`, for an
    /// events watch (the bridge's `@* events,sessions` and `aterm fleet
    /// events`) and a content watch alike, whose screen rings: an idle
    /// subscription makes no pass. Negative controls: a peer probed on a
    /// timer, and a held membership decision, bound it at LIVENESS, and an
    /// owed paced read bounds it sooner.
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

    /// A peer whose hang-up rings a list, as the peer watch's does
    /// (`control::WatchedPeer`), driven by the test: `gone` is the verdict it
    /// stores, `wakes` the list it rings, and `asked` counts the loop's calls
    /// to [`PeerLiveness::rings`], one when it registers and one before each
    /// wait.
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

    /// THE PUSH LOOP PARKS ON ITS PEER'S LIST: an events-only loop in a wait
    /// with no bound ends on the ring of its client's hang-up. The target is
    /// quiet, so nothing else rings, and with no content watch and a peer that
    /// rings, the wait has no timer (the table above): only the list
    /// `push_loop_with_peer_probe` registered from `rings()` can end it. The
    /// hang-up is staged once `rings()` has been asked twice, at registration
    /// and for the first wait after the catch-up, so the loop is in that wait
    /// or committed to it, and a ring that lands before it parks is a wake
    /// still pending. Counterfactual, run: with the registration removed, the
    /// loop parks for good and the join's hang detector fails the test.
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

    /// The opt-in the announcement tests below are about.
    const PROBE_TOPIC: &str = "probe";

    /// The record proving a watch has pushed everything up to the instant it was
    /// written ([`OptInProbe`]).
    const HEARTBEAT: &str = "heartbeat";

    /// A subscriber socket that ACTS on what the push loop writes, on the push
    /// loop's own thread, at the instant each line is written — the earliest
    /// instant any real reader could act on it. `Egress::send` writes every
    /// producer's frames through as soon as that producer has built them (a
    /// watch's frames with its bursts, a closing tail, a membership pass), so
    /// a reaction here lands at an exact point inside a wake: after the
    /// producer whose frame it reacts to, before whatever the loop does next. That is what makes the announcement tests deterministic
    /// with no hook in the shipping code. `on_line` answering `false` hangs the
    /// socket up, which ends the loop exactly as a client disconnect does.
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

    /// The peer-liveness probe as a hang detector: it never says the peer went,
    /// and a scripted scenario still running [`aterm_spec::wait::MUST_HAPPEN`]
    /// after it began PANICS, naming `what` it waited for. A correct run ends by
    /// its sink hanging up or its last watch closing, and never meets it; a
    /// probe that answered "gone" instead would end a wedged run as a clean one.
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

    /// A handle carved into the target `run_subscribe_with_peer_probe` hands the
    /// push loop.
    pub(super) fn target_of(h: &crate::session_store::SessionHandle) -> ResolvedTarget {
        ResolvedTarget {
            local_id: h.local_id,
            term: h.term.clone(),
            src: WatchSource::of(&h.ctx),
        }
    }

    /// The scopes the fabric bridge subscribes with: `subscribe @*
    /// events,sessions` from the Owner.
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

    /// The fabric bridge's handling of an announced session, reduced to what the
    /// announcement tests need.
    ///
    /// The bridge reads a session's opt-ins (`topic ls`, answered by
    /// [`crate::fabric::SessionFabric::topics`]) when the push lane first announces it,
    /// on every `sub <local> <sid>` ack of its watch, and on every 2 s roster round while
    /// it holds no ack (aterm-link `bridge.rs`, `Bridge::topics_sampled`,
    /// `Bridge::watched`). This probe makes the first two reads and no rounds. The agent
    /// opts into [`PROBE_TOPIC`] right after the first read, so the bridge can learn it
    /// only from a later read, the watch's push, or a `GAP`. Once the add is made and the
    /// watch acked, it records [`HEARTBEAT`]; that record is newer than any seed and a
    /// watch drains its timeline in id order, so when the heartbeat returns, everything
    /// the watch will push up to it has been pushed.
    struct OptInProbe {
        local: u64,
        sid: String,
        ctx: Arc<crate::SessionCtx>,
        /// Every read the bridge made of the opt-ins, in order.
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

        /// One `topic ls`, and the add that follows the first one.
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

        /// React to one wire line; `false` once the heartbeat is back.
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

        /// Whether the add reached the bridge by any road: a read after it, the watch's
        /// push, or a `GAP` that sends it back to `topic ls`.
        fn learned(&self, wire: &str) -> bool {
            self.reads
                .iter()
                .any(|topics| topics.iter().any(|t| t == PROBE_TOPIC))
                || wire.contains(&format!("EVENT {} topic add {PROBE_TOPIC} ", self.local))
                || wire.contains(&format!("GAP {} events-dropped=", self.local))
        }

        /// Wire offsets of the watch's ack and of the plain and the marked announcement.
        fn order(&self, wire: &str) -> (Option<usize>, Option<usize>, Option<usize>) {
            (
                wire.find(&format!("{}\n", self.ack_line())),
                wire.find(&format!("{}\n", self.created_line())),
                wire.find(&format!("{}\n", self.deferred_line())),
            )
        }
    }

    /// A session registered between adoption and the roster drain is announced only
    /// with its watch. The registration lands on the write of the `sub` ack for an
    /// earlier session T, a frame between the two decisions were they separate reads.
    /// The order assertion is the regression check; `learned` is the end-to-end claim
    /// (the bridge also re-reads on the ack, see [`OptInProbe`]).
    #[test]
    fn a_spawn_between_adoption_and_the_roster_is_announced_with_its_watch() {
        const T: u64 = 1;
        let store = crate::session_store::new_store();
        // T predates the subscription but is not an initial target, so the first wake
        // adopts it, and acking that adoption is the injection point.
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

    /// The deferred session, one pass at a time: announced at once and marked
    /// by the pass that could not adopt it; one that exits first is announced
    /// exiting like any other; and the journal's RECOVERY arm marks a newcomer
    /// the same way, and acks it when a slot opens,
    /// while the churned sessions nobody could watch net to nothing, as
    /// recovery always has.
    #[test]
    fn a_deferred_session_is_announced_at_once_and_acked_when_adopted() {
        use crate::control::MAX_SUBSCRIBE_TARGETS as CAP;
        use crate::session_store::{ROSTER_JOURNAL_CAP, test_handle};
        let store = crate::session_store::new_store();
        let mut sub = Subscription::new();
        let term = Arc::new(Mutex::new(Terminal::new(4, 8)));
        // CAP watches on local ids no session carries: nothing here prunes,
        // so they hold the cap without CAP real sessions.
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

        // Deferred, then gone before a slot opens.
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

        // RECOVERY. Still at the cap: c is deferred, then d, e registers and
        // d leaves while the journal rolls past the roster cursor, so recovery
        // has to find e by the set diff. (Deferred, then adopted when a slot
        // opens: `membership_passes_refine_the_announcement_model`.)
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
        // Slots are freed by dropping PLACEHOLDERS (index 0).
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

    /// A SLOT FREED BY A PRUNE RUNS THE MEMBERSHIP PASS THAT ADOPTS, though the
    /// instance list rang for the exit before. At the cap with S deferred, a
    /// watched session X leaves between one pass's prune and its membership
    /// pass: that pass takes the `Exited` row's ring with X still watched, so
    /// no room, and only the next pass's prune frees the slot. That prune sets
    /// the instance flag, and the membership pass after it acks S. NEGATIVE
    /// CONTROL: nothing rang the instance list between the pass that took the
    /// ring and the one that acks S, so the flag that runs it is the prune's.
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

    /// The handshake's ack follows its watch's seed, as an adoption's does: a reader
    /// acting on `sub <local> <sid>` must find the watch seeded, or a `topic add` made
    /// between its read and the seed is pushed by nobody. The subscription is the
    /// bridge's own, `subscribe @* events,sessions` through the control handler, and S
    /// opts in at the instant its ack is written, on the writing thread.
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

    /// What [`close_under_subscription`] saw on the loop's thread at the close.
    #[cfg(unix)]
    #[derive(Debug)]
    struct SeenAtClose {
        /// The context's strong count, the test's own clone included.
        count: usize,
        /// Whether the master was still open once the test's clone dropped:
        /// read at once under a pin, which must keep it open, and waited for
        /// ([`master_closes`]) without one, where it must close.
        master_open: bool,
        /// Everything the subscription wrote.
        wire: String,
    }

    /// Whether the master behind `peer` is open now: a read would block. Open
    /// is read at once.
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

    /// Whether the master behind `peer` closes: a read sees EOF. Closed waits
    /// (a hang detector, never a budget; `peer`'s read timeout, set while the
    /// pair is connected, since Darwin refuses the option on a disconnected
    /// socket): a sibling test's `fork()` that lands before the close hands its
    /// child a copy of the master, which stays open until that child execs
    /// ([`crate::parked_fork`]).
    #[cfg(unix)]
    fn master_closes(peer: &std::os::unix::net::UnixStream) -> bool {
        use std::io::Read as _;
        matches!((&*peer).read(&mut [0_u8; 1]), Ok(0))
    }

    /// Watch a tab whose sink owns its master through the control handler's
    /// `subscribe @* events,bytes,sessions`, the fabric bridge's kind of
    /// subscription, and close the tab (deregister it; the store drops its
    /// handle) at the instant its handshake ack is written. That is on the
    /// loop's thread, with the loop standing in egress where a stalled
    /// subscriber's write would hold it, so no pump pass, prune or wake comes
    /// between the close and the look. `pin` holds a context clone beside the
    /// subscription, as its watch and its resolved target each once did, and
    /// is handed back with the master's peer. The subscription then runs on
    /// until it has written the tab's `exited`.
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

    /// A CLOSED TAB'S MASTER CLOSES UNDER A LIVE SUBSCRIPTION. A watch, its
    /// resolved target and the subscription's target list hold the session's
    /// [`WatchSource`] leaves, never its context, whose sink owns the master.
    /// So when the tab closes under a subscription that is stalled in egress
    /// ([`close_under_subscription`]), the context's only strong count is the
    /// test's own clone, and once that drops the master is closed: no prune
    /// or wake has to come first. The subscription runs on and still delivers
    /// the tab's `closing` row and `exited` from the leaves its watch kept.
    ///
    /// NEGATIVE CONTROL: the same close with a context clone held beside the
    /// subscription leaves the count at 2 and the master open, until that
    /// clone drops.
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

    /// A `@*` subscription arriving over [`crate::control::MAX_SUBSCRIBE_TARGETS`] is
    /// bounded from its first wake: the handshake watches and acks the cap, and the
    /// session left over is adopted (acked) when a slot frees.
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

    /// Tier-1 for aterm-spec's `SubscribeAnnouncementOrder`
    /// (`derive::subscribe_announcement_order_model`), endpoint half: the real
    /// [`drain_membership`] through every branch the model has, each pass projected onto
    /// the model's steps and every step and environment move checked against the machine
    /// ([`aterm_spec::verify::validate_transition_tiered`], plus `ty trace validate` where
    /// `ty` is installed), with the law on every state. No bridge reads this world, so the
    /// bridge's variables stay at their initial values; aterm-link's
    /// `bridge_rounds_refine_the_announcement_model` binds the other half.
    ///
    /// A pass is atomic here, so its steps are built from what it decided (the `sub`
    /// ack, the `session-created` line, the watch set and roster cursor it left), and the
    /// state it leaves must equal the projection of the real state. `snap_*` is what the
    /// store held when the pass ran.
    ///
    /// The negative control is a two-read pass rebuilt from the same functions
    /// ([`pick_adoptions`] under one read, then [`drain_roster`] under a second with no
    /// watch set) with the registration between the two, and again deferred at the cap:
    /// the healthy machine must reject its roster step, and `Buggy=1` must admit it and
    /// reach a state the law refutes.
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

        // Every scenario, as the model's environment moves and passes.
        // `RegisterDuringPass` registers S after the pass's read and before its frames
        // would be written: the case the one read exists for.
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

        // The negative control: the two-read pass, a registration between the reads
        // (room free), and its cap twin (registered before, room zero).
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

    /// One session S (local 7) and one `@*` + `sessions` subscription, held
    /// at the cap by placeholder watches until a slot is freed — the world
    /// [`membership_passes_refine_the_announcement_model`] projects.
    struct AnnounceWorld {
        store: Store,
        sub: Subscription,
        watches: Vec<Watch>,
        cursors: PushCursors,
        /// S, built once: a handle's sid is minted when it is built.
        handle: crate::session_store::SessionHandle,
        sid: String,
        /// The roster seq of S's `Created` record, once S is registered.
        created: Option<u64>,
        told: i64,
        /// A pass has written S's `sub` ack.
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

        /// Drop a placeholder: an adopted watch is pushed at the end.
        fn free_slot(&mut self) {
            assert_ne!(self.watches[0].local_id, Self::S);
            drop(self.watches.remove(0));
        }

        fn watched(&self) -> bool {
            self.watches.iter().any(|w| w.local_id == Self::S)
        }

        /// The real state as the model's variables, between passes.
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
                // No bridge reads this world: its half stays at Init.
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

        /// What `frames` say about S: whether they ack its watch, and its
        /// `session-created` line — `0` none, `1` plain, `2` `watch=deferred`.
        /// The told state and the ack on the wire move with the frames, as the
        /// write would move them.
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

        /// One real membership pass; `before_write` runs after it returns and
        /// before its frames would reach the wire.
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

        /// The negative control's two-read pass: adoption under one read, `between` with
        /// no guard held, then the roster under a second read with no watch set to
        /// consult, then the adopted watches.
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

/// THE `log` STREAM (phase 3A): its grammar, its live and resumed cursors, its
/// exact gap, the `log` verb's filter, the host tier, and its place in a wake.
#[cfg(test)]
mod log_stream_tests {
    use super::*;

    /// A watch of `timeline`'s log only, from `cursor`, filtered by `filter`.
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

    /// A timeline whose log holds its session's open.
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

    /// The handshake's `sub` line of a log-streaming target names where its
    /// stream starts — the cursor the watch was seeded at, the head for a live
    /// stream, the resume cursor for `since-log=` — so a reader that resumes
    /// from the last `LOG` it read (or from there) misses nothing and is shown
    /// nothing twice. NEGATIVE CONTROL: a watch with no log stream leaves its
    /// line as it was, byte for byte.
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
        // Regression: a `log:` token after `mail:`'s comma list is its own stream.
        let both = Requested::parse("mail:kinds=task,log:class=turn").expect("two streams");
        assert!(both.targets.mail && both.targets.log);
        assert_eq!(both.mail.kinds, vec!["task".to_string()]);
        assert!(both.log.class_turn);
        assert!(aterm_types::control_verbs::SUBSCRIBE_STREAMS.contains(&"log"));
    }

    /// Live from the head: what was written before the subscribe is not
    /// pushed; what comes after is, in seq order. Resumed with `since-log=`,
    /// the records after it are pushed first. An idle wake reads nothing.
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

    /// A PROGRAM'S OWN REPORT STREAMS AS A `LOG` FRAME (6F-4, U17): the move
    /// the status sweep's gate shows, through its one writer
    /// (`SessionTimeline::note_program_state`), reaches a `log` watch as
    /// `LOG <sid> <seq> mark - program state=working src=engine conf=claim`.
    /// It is no turn-class record, so a `log:class=turn` watch never carries
    /// it. NEGATIVE CONTROL (scratch): `note_program_state` without its
    /// mark streams nothing, and this fails.
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

    /// The GAP is the read's own exact count: an eviction (`why=evicted`), a
    /// cursor of another epoch (`why=reset`), a carried update's hole
    /// (`why=handoff`); NEGATIVE: no loss, no GAP line.
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

    /// The `log` verb's filter, shared: `class=turn` drops steps and plain
    /// marks, `lvl=run` drops turns; a filtered record moves the cursor (it is
    /// no loss). The stream never pushes the content tier (no `title=`).
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
        // The same filter the `log` verb applies.
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

    /// A closed session's `close - out=…` is pushed before its `EVENT <local>
    /// exited` (the log drains last in the wake `Closing::drain` runs).
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
