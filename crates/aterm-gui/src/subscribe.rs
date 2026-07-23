// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Real-time SUBSCRIBER REGISTRY + PUSH face (design P1.3) — the additive layer
//! that turns the poll-only control socket into a server-PUSH face, so one agent
//! watches its OWN and OTHER sessions LIVE without busy-polling.
//!
//! ## Shape
//!
//! A `subscribe` connection (already its own thread, see [`crate::control`])
//! registers a [`SubscriberHandle`] keyed by the process-local `u64` id of each
//! session it watches — the SAME id the GUI routes `Wake::Output { session }` with
//! (main.rs `Wake::Output`). The handle is a SINGLE-SLOT non-blocking notify: an
//! `mpsc::sync_channel(1)` whose `try_send` NEVER blocks. The producer side (the
//! GUI/reader thread, via the one `Wake::Output` hook) calls [`Subscribers::notify`],
//! which `try_send`s a unit to every subscriber of that session and IGNORES a full
//! channel or a hung-up receiver. The subscriber thread blocks on `recv`, then
//! reads the session's CURRENT state and emits deltas.
//!
//! ## Coalescing — backpressure-safe BY CONSTRUCTION
//!
//! The notify slot has capacity ONE and `try_send` drops on a full slot, so a
//! pending-but-unread notify simply stays pending: N producer wakes between two
//! subscriber reads collapse into AT MOST ONE pending notify. When the subscriber
//! finally wakes it reads the LATEST state (the current `content_seq` and grid),
//! not a queue of every intermediate frame. A slow subscriber
//! therefore gets COARSER / fewer deltas and CANNOT block, backpressure, or even
//! slow the producing session's reader thread — the producer's `try_send` is O(1)
//! and infallible by design (it discards rather than waits). If the subscriber's
//! own socket write blocks or fails, its thread drops the connection (and
//! deregisters) — the producer is never involved in that path.
//!
//! ## Discipline
//!
//! The registry lock is a leaf: `notify` takes it, `try_send`s, and releases —
//! it is NEVER held across a `Terminal` lock or a socket write. The subscriber
//! thread reads the registry only to (de)register itself; it resolves and reads
//! target terminals through the [`crate::session_store::Store`] with the same
//! clone-then-release discipline the rest of the control path uses.
//!
//! ## Push-only
//!
//! Once a connection issues `subscribe` and the verb authorizes, [`crate::control`]
//! FLIPS that connection to push mode: it stops reading requests and enters
//! [`push_loop`]. A subscribed connection is PUSH-ONLY for the rest of its life —
//! the client reads `DELTA`/`EVENT`/`GAP` frames and never sends another verb.

use std::collections::HashMap;
use std::io::Write;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, SyncSender, TrySendError, sync_channel};
use std::sync::{Arc, LockResult, Mutex, MutexGuard};
use std::time::Duration;

use aterm_core::terminal::Terminal;

use crate::cast::{ByteFanout, ByteSubscription};
use crate::session_store::Store;
use crate::turn_ledger::TurnLedger;

/// The streams a subscription may watch. A subscription requests a subset and only
/// emits frames for the requested streams. `screen`/`cursor` ride the `content_seq`
/// delta path; `events` rides the block-complete (OSC 133 D) signal.
#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
pub struct Streams {
    /// Emit `DELTA <sid> seq=<n> screen <changed rows>` when content advances.
    pub screen: bool,
    /// Emit `DELTA <sid> seq=<n> cursor <row> <col> <visible> <style>` on any caret
    /// change (position, DECSCUSR style, or DECTCEM visibility) — even at an
    /// unchanged seq. `<visible>` is `0|1`, matching the poll `cursor` verb.
    pub cursor: bool,
    /// The lifecycle DIGEST: `EVENT <sid> block-complete <id> exit=<code>` on
    /// OSC 133 D, `EVENT <sid> turn <id> submitted=<0|1> status=<..> dur_ms=<..>`
    /// on each completed `turn` (scanned from the session TURN LEDGER), `EVENT <sid>
    /// title <pct>` when the window title changes (OSC 0/2 — often the cwd/command
    /// via shell integration), `EVENT <sid> bell total=<n>` on a BEL/alert, and
    /// `EVENT <sid> exited` once when the session closes.
    /// One low-rate stream an orchestrator watches for N sessions on one fd, pulling
    /// `screen`/`image` only when an event says something happened.
    pub events: bool,
    /// Emit `DELTA <sid> seq=<n> cells <nbytes>\n<styled-json>` when content
    /// advances — the LOSSLESS styled-screen frame (Item 1's payload) pushed live,
    /// so an outer agent sees the inner TUI's colour/attrs, not plaintext.
    pub cells: bool,
    /// Emit `BYTES <sid> <len>\n<raw bytes>` for EVERY program-output burst — the
    /// live, byte-lossless, every-frame channel (Item 2). Unlike screen/cells this
    /// never coalesces: the per-subscriber queue holds every burst between wakes.
    pub bytes: bool,
    /// A MODIFIER (not a frame source): when set, each wake that produces frames for
    /// a target is prefixed with a `T <sid> <t_us>` line stamping the instant on the
    /// SAME clock as `video`'s `index.json`. This turns the live stream into a TIMED
    /// frame source (frames-over-time) a driver can align against a `video` take.
    /// Opt-in, so a subscriber that does not request it sees the byte-identical
    /// un-timestamped stream.
    pub timestamps: bool,
    /// The INSTANCE-lifecycle stream (not per-target): emit `EVENT * session-created
    /// <sid>` when a session is spawned (by anyone) and `EVENT * session-exited <sid>`
    /// when one closes — so a fleet supervisor watching `@.` learns of SIBLING
    /// sessions it is not watching, without polling `ls`/`instances`. The `*` tag
    /// marks an instance-level (not per-channel) event.
    ///
    /// BEST-EFFORT, 250ms-SAMPLED: this is a point-in-time set diff on each wake, and
    /// an unwatched sibling never fires the notify, so the loop observes it only on the
    /// bounded 250ms poll. A sibling that both spawns AND exits inside one 250ms window
    /// appears in neither snapshot and emits neither event — the store keeps no
    /// monotonic lifecycle log to recover a sub-tick create→exit. Adequate for
    /// supervision (a session that lived <250ms is rarely actionable); a driver needing
    /// every ephemeral lifecycle must poll `ls` faster or drive from an event log.
    pub sessions: bool,
}

impl Streams {
    /// Parse a whitespace-or-comma separated stream list (`screen,cursor,events`).
    /// Returns `None` if EMPTY or any token is not a known stream — fail-closed so a
    /// typo does not silently subscribe to nothing.
    #[must_use]
    pub fn parse(s: &str) -> Option<Streams> {
        let mut out = Streams::default();
        let mut any = false;
        for tok in s.split([',', ' ', '\t']).filter(|t| !t.is_empty()) {
            any = true;
            match tok {
                "screen" => out.screen = true,
                "cursor" => out.cursor = true,
                "events" => out.events = true,
                "cells" => out.cells = true,
                "bytes" => out.bytes = true,
                "timestamps" | "ts" => out.timestamps = true,
                "sessions" => out.sessions = true,
                _ => return None,
            }
        }
        if any { Some(out) } else { None }
    }

    /// Whether any `content_seq`-driven stream (screen/cursor/cells) is requested.
    #[must_use]
    fn wants_content(self) -> bool {
        self.screen || self.cursor || self.cells
    }
}

/// One subscriber's wake handle: a single-slot notify the producer `try_send`s into.
/// Keyed in [`Subscribers`] by every process-local session id this subscriber
/// watches, plus a unique `token` so a dropped subscriber removes EXACTLY its own
/// entries (never another connection's that happens to watch the same session).
struct SubscriberHandle {
    /// Unique per registration, for precise removal across all watched sessions.
    token: u64,
    /// Single-slot, non-blocking notify. A full slot means "already pending" — the
    /// producer drops the extra wake (coalescing); the subscriber will read the
    /// latest state on its next wake regardless.
    notify: SyncSender<()>,
}

/// The process-wide subscriber index. Keyed by the GUI's process-local `u64`
/// session id so the existing `Wake::Output { session }` fan-out can find every
/// subscriber of the session that just produced output in O(1).
#[derive(Default)]
pub struct SubscriberSet {
    /// session local id -> the handles watching it. A session with no subscribers
    /// has no entry (and `notify` is then a cheap miss).
    by_session: HashMap<u64, Vec<SubscriberHandle>>,
    /// Monotonic source of unique registration tokens.
    next_token: u64,
}

/// The subscriber registry plus a LOCK-FREE "is anyone subscribed?" flag. The
/// `Mutex<SubscriberSet>` is the source of truth; `any` mirrors `!by_session
/// .is_empty()` and is updated under the SAME lock acquisitions that mutate the map
/// (register / drop), with `Release`/`Acquire` so a producer that observes `true`
/// also sees the registration. It lets the producer's per-`Wake::Output` hook skip
/// the mutex entirely in the overwhelmingly common ZERO-subscriber case — a single
/// atomic load instead of an acquire/release of the lock. A stale `true` only costs
/// one redundant lock+miss; a stale `false` is possible only in the instant after a
/// register and is benign — the next output burst observes `true`, and the
/// subscriber's `recv_timeout` requeries, so no update is permanently lost.
pub struct SubscriberRegistry {
    inner: Mutex<SubscriberSet>,
    any: AtomicBool,
}

impl SubscriberRegistry {
    /// Lock the underlying set. Same shape as `Mutex::lock`, so every existing
    /// `registry.lock()` call site is unchanged.
    pub fn lock(&self) -> LockResult<MutexGuard<'_, SubscriberSet>> {
        self.inner.lock()
    }
    /// Lock-free fast-path: `true` iff at least one session has a subscriber. The
    /// producer checks this before taking the lock to `notify`. `Acquire` pairs with
    /// the `Release` store in [`Self::refresh_any`] — the textbook publish-a-flag
    /// idiom — so once the producer observes `true` it also sees the registration that
    /// set it. (A momentarily-stale `false` right after a register is benign: the next
    /// output burst observes `true`, and the subscriber's own `recv_timeout` requeries,
    /// so no update is permanently lost.)
    #[must_use]
    pub fn any(&self) -> bool {
        self.any.load(Ordering::Acquire)
    }
    /// Refresh the flag from the (locked) set's emptiness. Called by register/drop
    /// while they already hold the lock, so it can never disagree across a mutation.
    /// `Release` so the matching `Acquire` in [`Self::any`] sees the map mutation.
    fn refresh_any(&self, set: &SubscriberSet) {
        self.any
            .store(!set.by_session.is_empty(), Ordering::Release);
    }
}

/// Shared handle to the subscriber registry: held by `App` (the producer side,
/// for the one `Wake::Output` notify hook) and cloned into the control thread
/// (the consumer side, where a `subscribe` connection registers itself).
pub type Subscribers = Arc<SubscriberRegistry>;

/// A new, empty subscriber registry.
#[must_use]
pub fn new_registry() -> Subscribers {
    Arc::new(SubscriberRegistry {
        inner: Mutex::new(SubscriberSet::default()),
        any: AtomicBool::new(false),
    })
}

/// The consumer-side end of a registration: the subscriber thread `recv`s wakes
/// here, and on drop deregisters itself from every session it watched. RAII so a
/// subscriber that returns (write failure / client hangup) cannot leak an entry
/// that would make `notify` pay for a dead receiver forever.
pub struct Subscription {
    registry: Subscribers,
    /// The sessions this subscription registered under (for precise deregistration).
    sessions: Vec<u64>,
    /// This subscription's unique token (matches its handles in the registry).
    token: u64,
    /// The blocking wake end. `recv` parks until the producer notifies (or every
    /// sender is dropped, which only happens when the registry entry is removed —
    /// i.e. after our own `Drop`, so in practice `recv` returns on a real notify).
    rx: Receiver<()>,
}

impl Subscription {
    /// Block until the producer notifies this subscriber (output landed on one of
    /// the watched sessions), or until `timeout` elapses. Returns `true` on a wake,
    /// `false` on timeout. A spurious/coalesced wake is fine: the caller re-reads
    /// the latest state and emits a delta only if `content_seq` advanced.
    #[must_use]
    pub fn wait(&self, timeout: Duration) -> bool {
        matches!(self.rx.recv_timeout(timeout), Ok(()))
    }
}

impl Drop for Subscription {
    fn drop(&mut self) {
        let mut g = self.registry.lock().unwrap_or_else(|p| p.into_inner());
        for sid in &self.sessions {
            if let Some(v) = g.by_session.get_mut(sid) {
                v.retain(|h| h.token != self.token);
                if v.is_empty() {
                    g.by_session.remove(sid);
                }
            }
        }
        self.registry.refresh_any(&g);
    }
}

impl SubscriberSet {
    /// Register a subscriber watching `sessions` (their process-local ids). Returns
    /// a [`Subscription`] whose `wait` blocks until a watched session produces
    /// output; dropping it deregisters from all watched sessions. The single-slot
    /// notify is created here and its receiver handed back inside the subscription.
    #[must_use]
    pub fn register(registry: &Subscribers, sessions: &[u64]) -> Subscription {
        // capacity 1 == single-slot: at most one pending notify (coalescing).
        let (tx, rx) = sync_channel::<()>(1);
        let mut g = registry.lock().unwrap_or_else(|p| p.into_inner());
        let token = g.next_token;
        g.next_token = g.next_token.wrapping_add(1);
        for &sid in sessions {
            g.by_session.entry(sid).or_default().push(SubscriberHandle {
                token,
                notify: tx.clone(),
            });
        }
        registry.refresh_any(&g);
        drop(g);
        Subscription {
            registry: registry.clone(),
            sessions: sessions.to_vec(),
            token,
            rx,
        }
    }

    /// Notify every subscriber of session `local_id` that it produced output.
    /// NON-BLOCKING and infallible by construction: a full single-slot channel
    /// (notify already pending) or a hung-up receiver (subscriber thread gone) is
    /// silently ignored, so the producer's reader/GUI thread is NEVER stalled by a
    /// slow or dead subscriber. This is the ONLY method the producer calls.
    pub fn notify(&self, local_id: u64) {
        let Some(handles) = self.by_session.get(&local_id) else {
            return; // no subscribers for this session: cheap miss
        };
        for h in handles {
            match h.notify.try_send(()) {
                // delivered, or already pending (coalesced), or receiver gone:
                // every outcome is a no-op for the producer.
                Ok(()) | Err(TrySendError::Full(())) | Err(TrySendError::Disconnected(())) => {}
            }
        }
    }

    /// How many live subscribers are watching one session — the `watchers=` the
    /// `who` verb reports and the "eye" a presence indicator lights. Counts every
    /// stream (a `screen`+`events` subscriber counts once: one registration).
    #[must_use]
    pub fn watchers(&self, local_id: u64) -> usize {
        self.by_session.get(&local_id).map_or(0, Vec::len)
    }

    /// Number of distinct sessions with at least one subscriber (test/introspection).
    #[must_use]
    #[allow(dead_code)]
    pub fn watched_sessions(&self) -> usize {
        self.by_session.len()
    }
}

/// One watched target inside a subscription: its process-local id and the live
/// `(term, sid_string, ctx)` handle resolved ONCE at subscribe time, plus the
/// per-target send cursors (`last_sent` content seq, last block id) so the
/// coalescing compare is O(1) on each wake.
///
/// The multiplex `<local>` tag on every frame is the process-local CHANNEL id (the
/// registry key as a string). It is NOT the `s-…` sid — a compact per-connection
/// handle. The `run_subscribe` ack maps each `<local>` to its stable sid via one
/// `sub <local> <sid>` line, so a client that subscribed by `@s-…`/`@.` can
/// demultiplex frames back to the sids it knows.
struct Watch {
    /// The process-local id (the registry key + the `<local>` frame tag).
    local_id: u64,
    /// The live engine handle (cloned out of the store, clone-then-release).
    term: Arc<Mutex<Terminal>>,
    /// The last `content_seq` we emitted a DELTA for. A wake with an unchanged
    /// seq emits NOTHING (a pure viewport scroll never bumps content_seq).
    last_sent_seq: u64,
    /// The alt-screen flag as of the last content DELTA. An alt swap (1049) does
    /// NOT touch the main grid's `content_seq` (the poll path's compound cache key
    /// keys on both for exactly this reason — control_query.rs:665), so a swap
    /// whose seq happens to match `last_sent_seq` would be INVISIBLE to a pure seq
    /// compare. A flip is treated like a backward seq: GAP + full resync frame.
    last_alt: bool,
    /// The `(row, col, visible, style)` of the last cursor DELTA we emitted. The
    /// `cursor` stream is named after this datum, but a pure caret move / DECSCUSR
    /// style flip / DECTCEM visibility toggle bumps no `content_seq`, so keying
    /// cursor frames on seq alone starves the stream (a TUI moving the caret between
    /// painted fields, a vi-mode shape flip, a shell hiding the caret during
    /// completion). Compared on every wake so a cursor-only change still emits a
    /// DELTA. The `visible` bit matches the poll `cursor` verb's `<visible>` field.
    last_cursor: (u16, u16, bool, &'static str),
    /// A cheap fingerprint of the RENDER state that changes the styled output
    /// WITHOUT bumping `content_seq`: the dynamic default fg/bg, the cursor colour,
    /// DECSCNM reverse-video, cursor visibility, and the 256-entry palette. A recolor
    /// (OSC 4/10/11/12/104), a DECSCNM flip, or a DECTCEM toggle mark full damage but
    /// no content change, so a seq-gated `cells`/`screen` subscriber would show stale
    /// colours until the next glyph write. When this signature changes at an unchanged
    /// seq we re-emit the content frame (no GAP — the delta cursor is still valid).
    /// Seeded to the live signature at subscription so only later changes push.
    last_render_sig: u64,
    /// The id of the highest block we have already reported `block-complete` for,
    /// so re-scanning completed blocks on each wake never double-emits. `None`
    /// before any block has completed.
    last_block_id: Option<u64>,
    /// The session's TURN LEDGER (the `events` digest scans it for new turns).
    turns: Arc<Mutex<TurnLedger>>,
    /// The highest turn id already reported, mirroring `last_block_id` — a live
    /// stream, so it is seeded to the current high at subscription.
    last_turn_id: Option<u64>,
    /// The session's EVENT TIMELINE (session-metadata stage 1): the `events`
    /// digest scans it for `meta-change` records and pushes each as
    /// `EVENT <sid> meta <payload>` — the push face of the `meta` verb.
    timeline: Arc<Mutex<crate::session_timeline::SessionTimeline>>,
    /// The highest timeline event id already scanned, mirroring `last_turn_id`
    /// — a live stream, seeded to the current high at subscription.
    last_timeline_id: Option<u64>,
    /// The window title as of the last `EVENT … title` we emitted. A title change
    /// (OSC 0/2 — often mirroring the cwd or running command via shell integration)
    /// is a fleet-supervision signal an orchestrator should get on the `events`
    /// stream, not have to poll `title` for. Seeded to the live title at subscription
    /// so only CHANGES after that are pushed. `None` until the events stream first runs.
    last_title: Option<String>,
    /// The `bell_total` (monotonic fired-bell count) as of the last `EVENT … bell`.
    /// A bell (BEL / OSC 777 alert) is a supervision signal a fleet orchestrator
    /// should get on the `events` stream. Seeded to the live total at subscription
    /// so only bells AFTER that are pushed.
    last_bell: u64,
    /// The live byte-stream subscription for the `bytes` stream (Item 2), or `None`
    /// when `bytes` was not requested. Drained every wake into `BYTES`/`GAP` frames.
    byte_sub: Option<ByteSubscription>,
    /// `every-frame` mode: re-emit the `cells` frame on EVERY wake even when
    /// `content_seq` is unchanged (animation fidelity), instead of only on advance.
    non_coalesced: bool,
}

/// The wire `<local>` channel tag for a target: its process-local id as a string.
/// One connection watching multiple sessions demultiplexes frames by this leading
/// token, resolving it to a stable sid via the ack's `sub <local> <sid>` map.
fn sid_tag(local_id: u64) -> String {
    local_id.to_string()
}

/// Read the CURRENT screen as `(seq, rows)` where each row is the trimmed visible
/// text — via [`crate::control::visible_row`], the SAME single source the
/// `text`/`text --json` verbs use, so a pushed DELTA row is byte-identical to a
/// Format a full screen DELTA for `sid` at `seq`: a header line followed by one
/// row per screen line (CHANGED set is the whole screen here — a coalesced wake
/// re-reads the latest grid rather than a diff, the backpressure-safe choice). The
/// row count is on the header so a client can frame the body without guessing.
///
/// `DELTA <sid> seq=<n> screen <nrows>\n` then `<nrows>` trimmed rows.
fn frame_screen(sid: &str, seq: u64, rows: &[String]) -> String {
    let mut out = format!("DELTA {sid} seq={seq} screen {}\n", rows.len());
    for r in rows {
        out.push_str(r);
        out.push('\n');
    }
    out
}

/// Format a cursor DELTA for `sid` at `seq`:
/// `DELTA <sid> seq=<n> cursor <row> <col> <visible> <style>\n`. The `<visible>`
/// bit (`0|1`) matches the poll `cursor` verb's field, so a DECTCEM hide/show is
/// visible on the push stream too (it moves no caret and bumps no content_seq).
fn frame_cursor(sid: &str, seq: u64, row: u16, col: u16, visible: bool, style: &str) -> String {
    format!(
        "DELTA {sid} seq={seq} cursor {row} {col} {} {style}\n",
        u8::from(visible)
    )
}

/// A cheap fingerprint of the RENDER state that changes the styled output without
/// advancing `content_seq` (see [`Watch::last_render_sig`]): DECTCEM visibility,
/// DECSCNM reverse-video, the dynamic default fg/bg + cursor colour, and the full
/// 256-entry palette. Bounded (256 palette reads + a few dynamics per wake), and
/// only computed for a `screen`/`cells` subscriber — negligible next to the grid
/// gather this lets us SKIP when nothing changed. (Note: `palette_color` scans the
/// override list, so a heavy live OSC-4 theme makes each read O(overrides), not
/// strictly O(1) — still a small constant, well under the styled gather it avoids.)
fn render_sig(t: &Terminal) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    let mut mix = |b: u8| {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    };
    mix(u8::from(t.cursor_visible()));
    mix(u8::from(t.reverse_video()));
    for c in [t.default_foreground(), t.default_background()] {
        mix(c.r);
        mix(c.g);
        mix(c.b);
    }
    match t.cursor_color() {
        Some(c) => {
            mix(1);
            mix(c.r);
            mix(c.g);
            mix(c.b);
        }
        None => mix(0),
    }
    for i in 0..=255u8 {
        let c = t.palette_color(i);
        mix(c.r);
        mix(c.g);
        mix(c.b);
    }
    h
}

/// Format a styled CELLS DELTA — the lossless styled-screen frame pushed live.
/// LENGTH-PREFIXED so the (large, single-line) JSON body frames cleanly on the
/// line-based socket: `DELTA <sid> seq=<n> cells <nbytes>\n<json>\n`. The body is
/// exactly Item 1's `styled_frame_payload` (one physical line); the trailing `\n`
/// is a non-counted terminator the client discards after reading `<nbytes>`.
fn frame_cells(sid: &str, seq: u64, payload: &str) -> String {
    format!("DELTA {sid} seq={seq} cells {}\n{payload}\n", payload.len())
}

/// Drain the `bytes` stream into `BYTES`/`GAP` frames as RAW bytes (binary-safe —
/// no escaping, no UTF-8 decode). A counted `GAP <sid> bytes-dropped=<n>` precedes
/// the bursts whenever the per-subscriber queue overflowed since the last drain.
/// Canonical binary framing: `BYTES <sid> <len>\n<len bytes>\n`.
fn drain_bytes_frames(watch: &mut Watch) -> Vec<u8> {
    let sid = sid_tag(watch.local_id);
    let mut out: Vec<u8> = Vec::new();
    let Some(bs) = &watch.byte_sub else {
        return out;
    };
    let (bursts, dropped) = bs.drain();
    if dropped > 0 {
        out.extend_from_slice(format!("GAP {sid} bytes-dropped={dropped}\n").as_bytes());
    }
    for burst in bursts {
        out.extend_from_slice(format!("BYTES {sid} {}\n", burst.len()).as_bytes());
        out.extend_from_slice(&burst);
        out.push(b'\n');
    }
    out
}

/// Format a block-complete EVENT for `sid`:
/// `EVENT <sid> block-complete <id> exit=<code|->\n`.
fn frame_block_complete(sid: &str, id: u64, exit: Option<i32>) -> String {
    let exit = exit.map_or_else(|| "-".to_string(), |c| c.to_string());
    format!("EVENT {sid} block-complete {id} exit={exit}\n")
}

/// Emit `EVENT <sid> title <pct-encoded>` when the window title CHANGED since the
/// last drain (OSC 0/2, often the cwd or running command via shell integration) —
/// a fleet-supervision signal on the `events` stream. Returns the new watermark.
/// The title is pct-encoded so a title with spaces/newlines stays one line.
fn drain_title_event(
    term: &Arc<Mutex<Terminal>>,
    sid: &str,
    last_title: Option<String>,
    out: &mut String,
) -> Option<String> {
    let title = crate::term_lock(term).title().to_string();
    if last_title.as_deref() != Some(title.as_str()) {
        out.push_str(&format!(
            "EVENT {sid} title {}\n",
            crate::control::pct_encode(&title)
        ));
    }
    Some(title)
}

/// Scan the target's EVENT TIMELINE and emit `EVENT <sid> meta <payload>` for
/// every `meta-change` record with id strictly greater than `last_id` — the push
/// face of the `meta` verb, next to the title emitter (a fleet supervisor learns
/// a sibling was renamed/annotated without polling `meta`). The payload is the
/// record's own pre-pct-encoded `field=<f> value=<pct|->` tail. The returned
/// watermark advances to the timeline HIGH (not just the last meta record), so
/// non-meta lifecycle records are scanned once, never re-walked every wake. Small
/// payload strings are cloned OUT under the lock; the lock is never held across
/// a write.
fn drain_meta_events(
    timeline: &Arc<Mutex<crate::session_timeline::SessionTimeline>>,
    sid: &str,
    last_id: Option<u64>,
    out: &mut String,
) -> Option<u64> {
    let (fresh, high) = {
        let tl = timeline.lock().unwrap_or_else(|p| p.into_inner());
        let fresh: Vec<String> = tl
            .since(last_id)
            .filter(|e| e.kind == "meta-change")
            .map(|e| e.payload.clone())
            .collect();
        (fresh, tl.high_id().or(last_id))
    };
    for payload in fresh {
        out.push_str(&format!("EVENT {sid} meta {payload}\n"));
    }
    high
}

/// Emit `EVENT <sid> bell total=<n>` when the monotonic fired-bell count advanced
/// since the last drain (BEL / OSC 777 alert) — a supervision signal on the
/// `events` stream. Returns the new watermark. One event per drain even if several
/// bells fired between wakes (the total tells how many); the throttle already
/// collapses bursts, so this cannot flood.
fn drain_bell_event(
    term: &Arc<Mutex<Terminal>>,
    sid: &str,
    last_bell: u64,
    out: &mut String,
) -> u64 {
    let total = crate::term_lock(term).bell_total();
    if total > last_bell {
        out.push_str(&format!("EVENT {sid} bell total={total}\n"));
    }
    total
}

/// The instance's live session ids (stable sids). Cheap: one read-lock + clone of
/// the small id list; never held across a Terminal lock or a socket write.
fn store_live_sids(store: &Store) -> std::collections::HashSet<String> {
    // `live_sids` clones only the sid strings (no whole-handle clone, no sort) —
    // this runs every wake for a `sessions` subscriber, incl. idle 250ms ticks.
    store.read().unwrap_or_else(|p| p.into_inner()).live_sids()
}

/// Emit `EVENT * session-created <sid>` / `EVENT * session-exited <sid>` for the
/// delta between the last-known live-session set and the current one — the
/// INSTANCE lifecycle stream (`*` = instance-level, not a per-channel event).
/// Returns the new watermark. Surfaces a SIBLING spawn/exit a fleet supervisor is
/// not watching, so it need not poll `ls`.
fn drain_session_events(
    store: &Store,
    known: &std::collections::HashSet<String>,
    out: &mut String,
) -> std::collections::HashSet<String> {
    let live = store_live_sids(store);
    diff_session_events(&live, known, out);
    live
}

/// The pure set-diff half of [`drain_session_events`] (store-free, unit-testable):
/// `session-created` for sids newly live, `session-exited` for sids gone.
fn diff_session_events(
    live: &std::collections::HashSet<String>,
    known: &std::collections::HashSet<String>,
    out: &mut String,
) {
    for sid in live.difference(known) {
        out.push_str(&format!("EVENT * session-created {sid}\n"));
    }
    for sid in known.difference(live) {
        out.push_str(&format!("EVENT * session-exited {sid}\n"));
    }
}

/// Scan the target's TURN LEDGER and emit a turn EVENT for every record whose id
/// is strictly greater than `last_turn_id`, advancing it — the turn-lifecycle
/// twin of `drain_block_events`. The small `(id, submitted, status, dur_ms)`
/// tuples are cloned OUT under the lock; the lock is never held across a write.
/// Digest form (no text/hash — that is what the `turns` verb is for):
/// `EVENT <sid> turn <id> submitted=<0|1> status=<..> dur_ms=<..>\n`.
fn drain_turn_events(
    turns: &Arc<Mutex<TurnLedger>>,
    sid: &str,
    last_turn_id: Option<u64>,
    out: &mut String,
) -> Option<u64> {
    let fresh: Vec<(u64, bool, &'static str, u64)> = {
        let l = turns.lock().unwrap_or_else(|p| p.into_inner());
        l.since(last_turn_id)
            .map(|r| (r.id, r.submitted, r.status, r.dur_ms))
            .collect()
    };
    let mut high = last_turn_id;
    for (id, submitted, status, dur_ms) in fresh {
        out.push_str(&format!(
            "EVENT {sid} turn {id} submitted={} status={status} dur_ms={dur_ms}\n",
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
/// engine's `content_seq` moved BACKWARD relative to our last-sent (a reset /
/// alt-screen swap / engine rebuild made the prior delta cursor meaningless), so
/// the client knows to treat the next DELTA as a fresh full snapshot.
fn frame_gap(sid: &str, seq: u64) -> String {
    format!("GAP {sid} resync={seq}\n")
}

/// A `GAP <sid> events-resync=<floor>` frame: a `since-turn=` resume anchor sat below
/// the ledger's retained low-water, so turn records in `(anchor, floor)` were
/// drop-oldest evicted and are gone — the resumed subscriber missed them (re-read
/// `history` if it needs the content). The events-stream twin of `frame_gap`.
fn frame_gap_events(sid: &str, floor: u64) -> String {
    format!("GAP {sid} events-resync={floor}\n")
}

/// Scan the target's completed blocks and emit a `block-complete` EVENT for every
/// block whose id is strictly greater than `last_block_id`, advancing it. The scan
/// clones the small `(id, exit)` tuples OUT under the lock and releases BEFORE any
/// socket write (the lock is never held across a write). Returns the new
/// `last_block_id` watermark.
fn drain_block_events(
    term: &Arc<Mutex<Terminal>>,
    sid: &str,
    last_block_id: Option<u64>,
    out: &mut String,
) -> Option<u64> {
    // Apply the watermark UNDER the lock (block ids are monotonic), so an idle wake
    // on a near-cap shell session collects an EMPTY Vec instead of cloning the whole
    // ~1000-entry completed-block history under the session's Terminal lock 4×/sec.
    let completed: Vec<(u64, Option<i32>)> = {
        let t = crate::term_lock(term);
        t.all_blocks()
            .filter(|b| b.is_complete() && last_block_id.is_none_or(|h| b.id > h))
            .map(|b| (b.id, b.exit_code))
            .collect()
    };
    let mut high = last_block_id;
    for (id, exit) in completed {
        out.push_str(&frame_block_complete(sid, id, exit));
        high = Some(high.map_or(id, |h| h.max(id)));
    }
    high
}

/// Build the frames a single wake produces for one watched target, mutating its
/// send cursors. Returns the (possibly empty) byte string to write to the
/// subscriber socket. PURE w.r.t. the socket — the caller does the write — so this
/// is unit-testable headlessly with no real connection.
///
/// COALESCING: a screen/cursor DELTA is emitted ONLY when `content_seq` ADVANCED
/// past `last_sent_seq`; an unchanged seq (e.g. a pure viewport scroll, which never
/// bumps `content_seq`) emits nothing. A wake always re-reads the LATEST state, so
/// N coalesced producer wakes collapse into ONE delta carrying the newest grid.
fn frames_for_watch(watch: &mut Watch, streams: Streams, woke: bool) -> String {
    let sid = sid_tag(watch.local_id);
    let mut out = String::new();

    if streams.wants_content() {
        // ONE lock hold gathers everything this wake needs. Cheap fields (seq, alt,
        // cursor) are read unconditionally; the EXPENSIVE ones (the full grid rows,
        // the styled snapshot) are cloned ONLY when this wake will actually emit a
        // content frame. Two payoffs:
        //   • Atomicity (R6): seq, cursor, rows and cells all come from a single
        //     consistent engine snapshot — a DELTA's seq can never pair with a grid
        //     or caret sampled a lock-acquisition later.
        //   • Efficiency (R5): an idle wake (unchanged seq, not an every-frame
        //     re-emit) pays only for the cheap reads — no full-grid clone, no styled
        //     gather. A 250ms liveness TIMEOUT (`woke == false`) never re-emits.
        let last_seq = watch.last_sent_seq;
        let last_render = watch.last_render_sig;
        // The render signature only drives the `screen`/`cells` re-emit on a recolor /
        // DECSCNM flip; the `cursor` stream sources everything it emits from `cur`
        // (incl. the DECTCEM visible bit), so a CURSOR-ONLY subscriber must NOT compute
        // it — else it pays the palette hash under the lock every wake AND spuriously
        // re-emits an unchanged caret on any recolor. Gate on screen||cells.
        let wants_render = streams.screen || streams.cells;
        let alt_flip_before = |alt: bool| alt != watch.last_alt;
        let (seq, alt, cur, sig, rows, styled) = {
            let t = crate::term_lock(&watch.term);
            let seq = t.content_seq();
            let alt = t.is_alternate_screen();
            // Cursor-only: reuse the last signature so `render_changed` is always false.
            let sig = if wants_render {
                render_sig(&t)
            } else {
                last_render
            };
            let cur = streams.cursor.then(|| {
                let c = t.cursor();
                (
                    c.row,
                    c.col,
                    t.cursor_visible(),
                    crate::control::cursor_style_name(t.cursor_style()),
                )
            });
            // A render-state change (recolor / DECSCNM / DECTCEM) mutates the styled
            // output at an UNCHANGED seq, so fold it into content_change alongside the
            // alt-swap resync so `cells`/`screen` re-gather and push it.
            let render_changed = sig != last_render;
            let content_change = seq != last_seq || alt_flip_before(alt) || render_changed;
            // `every-frame` re-emits `cells` on an UNCHANGED seq for animation
            // fidelity — but ONLY on a genuine wake, never a bare liveness timeout.
            let every_frame = watch.non_coalesced && streams.cells && woke && !content_change;
            // Rows only for a full/gap frame (content changed); styled whenever we
            // will send cells (content changed OR an every-frame re-emit).
            let rows: Option<Vec<String>> = (content_change && streams.screen).then(|| {
                let n = t.rows() as usize;
                (0..n).map(|r| crate::control::visible_row(&t, r)).collect()
            });
            let styled = ((content_change || every_frame) && streams.cells)
                .then(|| crate::control::gather_styled_frame(&t));
            (seq, alt, cur, sig, rows, styled)
        };
        // Serialize the styled snapshot with the lock RELEASED (expensive per-cell
        // JSON/base64) so it never blocks keystroke encode or a frame snapshot.
        let cells_payload = styled.as_ref().map(crate::control::serialize_styled_frame);

        // Emit screen + cursor + cells from the single snapshot above (used by the
        // GAP-resync and forward-advance branches). Each stream is gated by whether
        // its payload was gathered, so this respects the requested stream set.
        let emit = |out: &mut String, seq: u64| {
            if let Some(r) = rows.as_ref() {
                out.push_str(&frame_screen(&sid, seq, r));
            }
            if let Some((cr, cc, cv, cs)) = cur {
                out.push_str(&frame_cursor(&sid, seq, cr, cc, cv, cs));
            }
            if let Some(p) = cells_payload.as_ref() {
                out.push_str(&frame_cells(&sid, seq, p));
            }
        };
        // An alt-screen swap (1049) does not move the main grid's content_seq, so a
        // swap landing on the SAME seq is a full-screen change a seq compare misses.
        // Treat a flag flip like a backward seq: resync (GAP + full frame).
        let alt_flip = alt != watch.last_alt;
        let render_changed = sig != watch.last_render_sig;
        let mut content_framed = true;
        if seq < watch.last_sent_seq || alt_flip {
            // The engine's content seq moved BACKWARD (reset / engine rebuild) OR the
            // alt buffer swapped: the client's prior delta cursor is meaningless —
            // signal a resync and send a full frame.
            out.push_str(&frame_gap(&sid, seq));
            watch.last_sent_seq = seq;
            watch.last_alt = alt;
            emit(&mut out, seq);
        } else if seq > watch.last_sent_seq || render_changed {
            // Forward content advance, OR a same-seq RENDER change (recolor / DECSCNM /
            // DECTCEM) that mutated the styled output: emit a fresh full frame. No GAP
            // for a render-only change — the client's seq cursor is still valid, only
            // the resolved colours/visibility moved.
            watch.last_sent_seq = seq;
            watch.last_alt = alt;
            emit(&mut out, seq);
        } else if let Some(p) = cells_payload.as_ref() {
            // `every-frame` mode: re-emit the styled frame on an unchanged seq so a
            // fast-repainting TUI's transient states are observable (animation
            // fidelity). Only `cells` re-emits; screen/cursor stay coalesced.
            out.push_str(&frame_cells(&sid, seq, p));
            content_framed = false;
        } else {
            content_framed = false;
        }
        // Sync the render watermark after handling, so the NEXT wake only re-emits on
        // a further render change.
        watch.last_render_sig = sig;

        // CURSOR: a content frame above already carried the current cursor, so sync
        // the watermark. Otherwise a pure caret reposition / DECSCUSR style flip /
        // DECTCEM visibility toggle — which bumps no content_seq — would starve the
        // `cursor` stream: emit a DELTA (stamped at the current seq) when the caret
        // datum changed with no content frame this wake.
        if let Some(cur) = cur {
            if content_framed {
                watch.last_cursor = cur;
            } else if cur != watch.last_cursor {
                let (cr, cc, cv, cs) = cur;
                out.push_str(&frame_cursor(&sid, seq, cr, cc, cv, cs));
                watch.last_cursor = cur;
            }
        }
    }

    if streams.events {
        watch.last_block_id = drain_block_events(&watch.term, &sid, watch.last_block_id, &mut out);
        watch.last_turn_id = drain_turn_events(&watch.turns, &sid, watch.last_turn_id, &mut out);
        watch.last_timeline_id =
            drain_meta_events(&watch.timeline, &sid, watch.last_timeline_id, &mut out);
        watch.last_title = drain_title_event(&watch.term, &sid, watch.last_title.take(), &mut out);
        watch.last_bell = drain_bell_event(&watch.term, &sid, watch.last_bell, &mut out);
    }

    // TIMESTAMPS (opt-in): prefix this wake's frames for the target with a
    // `T <sid> <t_us>` line stamping the instant (video's `now_us` clock). One line
    // per wake — every frame below it shares the instant (a wake is one read) — so
    // the stream becomes a timed frame source without touching any frame's grammar.
    if streams.timestamps && !out.is_empty() {
        out = format!("T {sid} {}\n{out}", crate::metrics::now_us());
    }

    out
}

/// A resolved subscribe target tuple, cloned OUT of the store at subscribe time:
/// the process-local id, the live engine handle, the session's live byte
/// fan-out (for the `bytes` stream — a `subscribe` registers on it lazily), its
/// turn ledger, and its event timeline (the `events` digest's meta-push source).
pub type ResolvedTarget = (
    u64,
    Arc<Mutex<Terminal>>,
    Arc<ByteFanout>,
    Arc<Mutex<TurnLedger>>,
    Arc<Mutex<crate::session_timeline::SessionTimeline>>,
);

/// The PUSH LOOP for a `subscribe` connection. The connection has already
/// AUTHORIZED every target via the control gate; here we just register for wakes,
/// emit an immediate catch-up (so a fresh subscriber sees the current screen) and
/// optionally honor `since=<seq>`, then block on the registry notify and push a
/// coalesced frame on each wake until the client disconnects (write fails) or the
/// loop is asked to stop.
///
/// PUSH-ONLY: once here, the connection never reads another request line. The
/// writer is the ONLY thing this loop touches on the socket. A write failure
/// (broken pipe / slow-then-dead client) ends the loop and drops the
/// [`Subscription`] (deregistering), so the producer never pays for a dead
/// subscriber.
///
/// `since` (optional, applied per target): the client's last-seen `content_seq`.
/// If the live content has advanced past it, the first wake's compare already
/// emits a catch-up DELTA; we seed each watch's `last_sent_seq` to `since` so the
/// immediate catch-up fires exactly when content moved past `since`.
#[allow(clippy::too_many_arguments)]
pub fn push_loop<W: Write>(
    registry: &Subscribers,
    store: &Store,
    targets: &[ResolvedTarget],
    streams: Streams,
    since: Option<u64>,
    since_turn: Option<u64>,
    since_block: Option<u64>,
    non_coalesced: bool,
    writer: &mut W,
) {
    let local_ids: Vec<u64> = targets.iter().map(|(id, _, _, _, _)| *id).collect();
    let sub = SubscriberSet::register(registry, &local_ids);

    // INSTANCE lifecycle watermark: seed to the CURRENT live set so only
    // spawns/exits AFTER subscription are pushed (a fresh subscriber `ls`s for the
    // baseline). The 250ms bounded wait below already re-polls, so a sibling
    // spawn surfaces within one tick — no separate notify wiring needed.
    let mut known_sids = streams.sessions.then(|| store_live_sids(store));

    // Build the per-target send cursors. `since` seeds `last_sent_seq` so the
    // IMMEDIATE catch-up below fires exactly when the live content advanced past
    // the client's last-seen seq; otherwise we start at 0 (a brand-new subscriber
    // gets a full snapshot on the first wake / immediate pass).
    let mut watches: Vec<Watch> = targets
        .iter()
        .map(|(id, term, fanout, turns, timeline)| Watch {
            local_id: *id,
            term: term.clone(),
            turns: turns.clone(),
            // `since-turn=<id>` resumes the turn stream from that id (push turns
            // with id > since_turn); absent it, seed to the live high so only
            // post-subscription turns push. Only meaningful with the events stream.
            last_turn_id: since_turn.or_else(|| initial_turn_watermark(turns, streams)),
            timeline: timeline.clone(),
            // Seed to the live timeline high so only meta changes AFTER
            // subscription push (a live stream, like turns/blocks/title).
            last_timeline_id: initial_timeline_watermark(timeline, streams),
            // Seed to the live title so a fresh `events` subscriber gets title
            // CHANGES from here, not a spurious event for the title already showing.
            last_title: streams
                .events
                .then(|| crate::term_lock(term).title().to_string()),
            // Seed to the live bell total so a fresh subscriber gets bells from here.
            last_bell: if streams.events {
                crate::term_lock(term).bell_total()
            } else {
                0
            },
            last_sent_seq: since.unwrap_or(0),
            // Seed to the live alt-screen state so a subscriber that connects while
            // a TUI is already on the alt buffer does not spuriously resync on its
            // first wake; a genuine swap after this flips it.
            last_alt: if streams.wants_content() {
                crate::term_lock(term).is_alternate_screen()
            } else {
                false
            },
            // Seed to a sentinel so the FIRST content/cursor wake always emits the
            // caret (a fresh subscriber has no prior cursor); real positions differ.
            last_cursor: (u16::MAX, u16::MAX, false, ""),
            // Seed the render signature to the LIVE value so a subscriber does not
            // spuriously resync on its first wake; a genuine recolor/DECSCNM/DECTCEM
            // after this flips it. Only meaningful for a content-bearing subscription.
            last_render_sig: if streams.wants_content() {
                render_sig(&crate::term_lock(term))
            } else {
                0
            },
            // Seed the block watermark to the CURRENT high so we only push blocks
            // that COMPLETE after subscription, never the historical backlog —
            // `events` is a live stream, not a replay (matches `since` for screen).
            last_block_id: since_block.or_else(|| initial_block_watermark(term, streams)),
            // Register on the byte fan-out ONLY when `bytes` is requested, so an
            // idle/unsubscribed session pays nothing for the live byte channel.
            byte_sub: if streams.bytes {
                Some(fanout.subscribe())
            } else {
                None
            },
            non_coalesced,
        })
        .collect();

    // EVENTS-RESUME GAP: a `since-turn=<n>` anchor BELOW the ledger's retained
    // low-water means turn records were drop-oldest evicted between the anchor and the
    // window — signal it so the resumed subscriber knows it missed some (turn ids are
    // process-global, so a per-session id gap can't reveal the loss). Resume anchors
    // are single-target (R4), so at most one watch matches.
    if streams.events
        && let Some(anchor) = since_turn
    {
        for w in &mut watches {
            let low = w.turns.lock().unwrap_or_else(|p| p.into_inner()).low_id();
            if let Some(low) = low
                && anchor + 1 < low
            {
                let gap = frame_gap_events(&sid_tag(w.local_id), low);
                if writer.write_all(gap.as_bytes()).is_err() {
                    return;
                }
            }
        }
    }

    // IMMEDIATE catch-up: emit the current state once so a fresh subscriber is not
    // blind until the next output burst. With `since`, this fires a DELTA only if
    // content already advanced past `since`; without it, it sends the full screen.
    // (The `bytes` stream has no backlog to replay — it is live from this point.)
    for w in &mut watches {
        // `woke = true`: the immediate catch-up is a genuine first emit, not a
        // liveness timeout, so an every-frame subscriber gets its opening cells frame.
        let frame = frames_for_watch(w, streams, true);
        if !frame.is_empty() && writer.write_all(frame.as_bytes()).is_err() {
            return; // client already gone
        }
    }
    // Surface a client that closed DURING catch-up immediately (same as the loop's
    // post-write flush at the bottom). Ignoring this let a dead subscriber linger —
    // registered, never reaped — until the watched session next produced output (a
    // silent session = effectively never), wasting a registry slot + notify channel.
    if writer.flush().is_err() {
        return;
    }

    // The push loop proper. Block on a notify (bounded so a never-producing set of
    // sessions still lets the loop notice a dropped client on the next write), then
    // re-read the LATEST state of every watched target and push a coalesced frame.
    loop {
        // A bounded wait: on a real wake we push immediately; on a timeout we still
        // loop (a no-op pass that costs one cheap content_seq compare per target and
        // lets a half-closed socket surface via the next write). The producer never
        // waits on us regardless (single-slot notify), so this interval only bounds
        // OUR own liveness, not the producer's.
        let woke = sub.wait(Duration::from_millis(250));

        // Re-resolve liveness: a target deregistered from the store (its pane
        // closed) is dropped from our watch set so we stop reading a dead engine.
        // On the `events` stream a departing session first emits `EVENT <sid>
        // exited` so a fleet controller sees the death without a separate probe.
        let goodbye = prune_closed(store, &mut watches, streams);
        if !goodbye.is_empty() && writer.write_all(goodbye.as_bytes()).is_err() {
            return;
        }
        if watches.is_empty() {
            let _ = writer.flush();
            return; // every watched session closed
        }

        // Per watch: the UTF-8 text/cells frames, then the RAW binary byte frames.
        // Writing per-watch keeps each session's frames contiguous; the byte frames
        // are length-prefixed so a client demuxes text vs binary unambiguously.
        let mut wrote = false;

        // INSTANCE lifecycle (connection-level, once per wake): a sibling spawn/exit
        // the subscriber is not watching, so a fleet supervisor need not poll `ls`.
        if let Some(known) = known_sids.as_mut() {
            let mut se = String::new();
            *known = drain_session_events(store, known, &mut se);
            if !se.is_empty() {
                if writer.write_all(se.as_bytes()).is_err() {
                    return;
                }
                wrote = true;
            }
        }
        for w in &mut watches {
            let text = frames_for_watch(w, streams, woke);
            if !text.is_empty() {
                if writer.write_all(text.as_bytes()).is_err() {
                    return; // dead client: end loop, drop Subscription (deregister)
                }
                wrote = true;
            }
            if streams.bytes {
                let bytes = drain_bytes_frames(w);
                if !bytes.is_empty() {
                    if writer.write_all(&bytes).is_err() {
                        return;
                    }
                    wrote = true;
                }
            }
        }
        if wrote && writer.flush().is_err() {
            return;
        }
    }
}

/// The block-id watermark to start a fresh `events` subscription at: the current
/// highest completed block id (so only blocks completing AFTER subscription are
/// pushed). `None` when the `events` stream is not requested or no block has
/// completed yet.
fn initial_block_watermark(term: &Arc<Mutex<Terminal>>, streams: Streams) -> Option<u64> {
    if !streams.events {
        return None;
    }
    let t = crate::term_lock(term);
    t.all_blocks()
        .filter(|b| b.is_complete())
        .map(|b| b.id)
        .max()
}

/// Seed the turn watermark to the ledger's current high so the `events` digest
/// streams only turns that COMPLETE after subscription (live, never the backlog —
/// mirrors `initial_block_watermark`). `None` when `events` was not requested.
fn initial_turn_watermark(turns: &Arc<Mutex<TurnLedger>>, streams: Streams) -> Option<u64> {
    if !streams.events {
        return None;
    }
    turns.lock().unwrap_or_else(|p| p.into_inner()).high_id()
}

/// The timeline twin of [`initial_turn_watermark`]: seed the meta-event scan to
/// the CURRENT timeline high so only post-subscription changes push.
fn initial_timeline_watermark(
    timeline: &Arc<Mutex<crate::session_timeline::SessionTimeline>>,
    streams: Streams,
) -> Option<u64> {
    if !streams.events {
        return None;
    }
    timeline.lock().unwrap_or_else(|p| p.into_inner()).high_id()
}

/// Drop any watched target whose session has been DEREGISTERED from the store
/// (its pane closed). Keeps the watch set tracking only live engines. A closed
/// session simply stops producing frames; the registry notify for it is already a
/// cheap miss after deregistration.
fn prune_closed(store: &Store, watches: &mut Vec<Watch>, streams: Streams) -> String {
    let g = store.read().unwrap_or_else(|p| p.into_inner());
    let mut goodbye = String::new();
    watches.retain(|w| {
        let alive = g.by_local(w.local_id).is_some();
        if !alive && streams.events {
            goodbye.push_str(&frame_exited(&sid_tag(w.local_id)));
        }
        alive
    });
    goodbye
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    /// A subscriber registered for a session is woken by a notify on that session,
    /// and NOT by a notify on an unrelated session.
    #[test]
    fn notify_wakes_only_subscribed_sessions() {
        let reg = new_registry();
        let sub = SubscriberSet::register(&reg, &[7]);

        // A notify on an unrelated session does not wake us.
        reg.lock().unwrap().notify(99);
        assert!(
            !sub.wait(Duration::from_millis(20)),
            "unrelated notify must not wake"
        );

        // A notify on our session wakes us.
        reg.lock().unwrap().notify(7);
        assert!(sub.wait(Duration::from_millis(200)), "our notify must wake");
    }

    /// COALESCING: a flood of notifies between two `wait`s collapses to a single
    /// pending wake (single-slot). The producer's `notify` never blocks regardless
    /// of how far behind the subscriber is.
    #[test]
    fn notify_coalesces_and_never_blocks_producer() {
        let reg = new_registry();
        let sub = SubscriberSet::register(&reg, &[1]);

        // 1000 notifies with NO intervening read: every one is O(1) and non-blocking
        // even though the slot fills after the first. (If notify ever blocked, this
        // would deadlock on the same thread.)
        for _ in 0..1000 {
            reg.lock().unwrap().notify(1);
        }
        // Exactly one wake is pending (coalesced); the second wait times out.
        assert!(sub.wait(Duration::from_millis(200)), "first wake delivered");
        assert!(
            !sub.wait(Duration::from_millis(20)),
            "flood coalesced to one wake"
        );
    }

    /// A notify to a session with a DROPPED subscriber is a no-op and the registry
    /// self-cleans: the dropped subscription deregisters, so the producer pays
    /// nothing for a dead subscriber.
    #[test]
    fn dropped_subscriber_deregisters_and_notify_is_noop() {
        let reg = new_registry();
        {
            let _sub = SubscriberSet::register(&reg, &[5]);
            assert_eq!(reg.lock().unwrap().watched_sessions(), 1);
        } // _sub dropped here
        assert_eq!(
            reg.lock().unwrap().watched_sessions(),
            0,
            "deregistered on drop"
        );
        // Still a safe no-op.
        reg.lock().unwrap().notify(5);
    }

    /// A STALLED subscriber (never calls `wait`) cannot block or backpressure the
    /// producer: 100k notifies complete instantly while the slot stays full.
    #[test]
    fn stalled_subscriber_never_blocks_producer() {
        let reg = new_registry();
        let _sub = SubscriberSet::register(&reg, &[3]); // never wait()ed: wedged
        let start = std::time::Instant::now();
        for _ in 0..100_000 {
            reg.lock().unwrap().notify(3);
        }
        // If notify blocked on a full slot this would never finish; assert it is fast.
        assert!(
            start.elapsed() < Duration::from_secs(5),
            "producer not blocked by stall"
        );
    }

    /// Multiplex: one subscription watching two sessions is woken by EITHER.
    #[test]
    fn one_subscription_watches_multiple_sessions() {
        let reg = new_registry();
        let sub = SubscriberSet::register(&reg, &[10, 20]);
        reg.lock().unwrap().notify(20);
        assert!(
            sub.wait(Duration::from_millis(200)),
            "woken by either watched session"
        );
    }

    /// Stream parsing: a subset of the known streams parses; an empty list or a
    /// typo fails closed (so a bad request never silently subscribes to nothing).
    #[test]
    fn streams_parse_subset_and_fail_closed() {
        assert_eq!(
            Streams::parse("screen"),
            Some(Streams {
                screen: true,
                ..Default::default()
            })
        );
        assert_eq!(
            Streams::parse("screen,cursor,events"),
            Some(Streams {
                screen: true,
                cursor: true,
                events: true,
                ..Default::default()
            }),
        );
        assert_eq!(
            Streams::parse("cursor screen"),
            Some(Streams {
                screen: true,
                cursor: true,
                ..Default::default()
            }),
        );
        assert_eq!(Streams::parse(""), None, "empty fails closed");
        assert_eq!(Streams::parse("bogus"), None, "unknown stream fails closed");
        assert_eq!(
            Streams::parse("screen,bogus"),
            None,
            "one bad token fails the whole list"
        );
    }

    /// CORE coalescing claim at the FRAME level: a screen DELTA is emitted only when
    /// the engine's `content_seq` ADVANCES; a wake with unchanged content (a pure
    /// viewport scroll never bumps `content_seq`) emits NOTHING. Each emitted frame
    /// is `<sid>`-tagged so a multiplexed client can demultiplex it.
    #[test]
    fn screen_delta_on_content_change_none_on_viewport_scroll() {
        let term = Arc::new(Mutex::new(Terminal::new(24, 80)));
        let streams = Streams {
            screen: true,
            ..Default::default()
        };
        let mut w = Watch {
            local_id: 4,
            term: term.clone(),
            last_sent_seq: 0,
            last_alt: false,
            last_cursor: (u16::MAX, u16::MAX, false, ""),
            last_render_sig: 0,
            last_block_id: None,
            turns: std::sync::Arc::new(std::sync::Mutex::new(TurnLedger::default())),
            timeline: std::sync::Arc::new(std::sync::Mutex::new(
                crate::session_timeline::SessionTimeline::default(),
            )),
            last_timeline_id: None,
            last_turn_id: None,
            last_title: None,
            last_bell: 0,
            byte_sub: None,
            non_coalesced: false,
        };

        // First wake on a fresh engine: content_seq is already > 0 (the engine
        // initialized its grid), so an immediate catch-up DELTA is produced, tagged
        // with our sid (4).
        crate::term_lock(&term).process(b"hello");
        let f1 = frames_for_watch(&mut w, streams, true);
        assert!(
            f1.starts_with("DELTA 4 seq="),
            "sid-tagged screen delta: {f1:?}"
        );
        assert!(f1.contains("hello"), "delta carries the live screen text");
        let seq_after_write = w.last_sent_seq;
        assert!(seq_after_write > 0);

        // A wake with NO content change (we only move the viewport — a pure scroll
        // does not bump content_seq) emits NOTHING.
        crate::term_lock(&term).scroll_display(1);
        let f2 = frames_for_watch(&mut w, streams, true);
        assert!(f2.is_empty(), "viewport scroll produces no delta: {f2:?}");
        assert_eq!(
            w.last_sent_seq, seq_after_write,
            "seq unchanged by a scroll"
        );

        // A real content change DOES advance the seq and re-emits a delta.
        crate::term_lock(&term).process(b" world");
        let f3 = frames_for_watch(&mut w, streams, true);
        assert!(
            f3.starts_with("DELTA 4 seq="),
            "content change re-emits: {f3:?}"
        );
        assert!(
            w.last_sent_seq > seq_after_write,
            "seq advanced on real content"
        );
    }

    /// MULTIPLEX: two distinct watches produce frames tagged with their OWN sid, so
    /// a single connection watching both can demultiplex by the leading `<sid>`.
    #[test]
    fn multiplex_two_sids_tag_their_own_deltas() {
        let term_a = Arc::new(Mutex::new(Terminal::new(24, 80)));
        let term_b = Arc::new(Mutex::new(Terminal::new(24, 80)));
        crate::term_lock(&term_a).process(b"alpha");
        crate::term_lock(&term_b).process(b"bravo");
        let streams = Streams {
            screen: true,
            ..Default::default()
        };
        let mut wa = Watch {
            local_id: 1,
            term: term_a,
            last_sent_seq: 0,
            last_alt: false,
            last_cursor: (u16::MAX, u16::MAX, false, ""),
            last_render_sig: 0,
            last_block_id: None,
            turns: std::sync::Arc::new(std::sync::Mutex::new(TurnLedger::default())),
            timeline: std::sync::Arc::new(std::sync::Mutex::new(
                crate::session_timeline::SessionTimeline::default(),
            )),
            last_timeline_id: None,
            last_turn_id: None,
            last_title: None,
            last_bell: 0,
            byte_sub: None,
            non_coalesced: false,
        };
        let mut wb = Watch {
            local_id: 2,
            term: term_b,
            last_sent_seq: 0,
            last_alt: false,
            last_cursor: (u16::MAX, u16::MAX, false, ""),
            last_render_sig: 0,
            last_block_id: None,
            turns: std::sync::Arc::new(std::sync::Mutex::new(TurnLedger::default())),
            timeline: std::sync::Arc::new(std::sync::Mutex::new(
                crate::session_timeline::SessionTimeline::default(),
            )),
            last_timeline_id: None,
            last_turn_id: None,
            last_title: None,
            last_bell: 0,
            byte_sub: None,
            non_coalesced: false,
        };

        let fa = frames_for_watch(&mut wa, streams, true);
        let fb = frames_for_watch(&mut wb, streams, true);
        assert!(fa.starts_with("DELTA 1 "), "watch A tags sid 1: {fa:?}");
        assert!(fa.contains("alpha"));
        assert!(fb.starts_with("DELTA 2 "), "watch B tags sid 2: {fb:?}");
        assert!(fb.contains("bravo"));
    }

    /// A cursor DELTA carries the deterministic cursor state in the SAME wire shape
    /// the `cursor` verb reports, tagged with the sid.
    #[test]
    fn cursor_delta_reports_position_and_style() {
        let term = Arc::new(Mutex::new(Terminal::new(24, 80)));
        crate::term_lock(&term).process(b"abc");
        let streams = Streams {
            cursor: true,
            ..Default::default()
        };
        let mut w = Watch {
            local_id: 9,
            term,
            last_sent_seq: 0,
            last_alt: false,
            last_cursor: (u16::MAX, u16::MAX, false, ""),
            last_render_sig: 0,
            last_block_id: None,
            turns: std::sync::Arc::new(std::sync::Mutex::new(TurnLedger::default())),
            timeline: std::sync::Arc::new(std::sync::Mutex::new(
                crate::session_timeline::SessionTimeline::default(),
            )),
            last_timeline_id: None,
            last_turn_id: None,
            last_title: None,
            last_bell: 0,
            byte_sub: None,
            non_coalesced: false,
        };
        let f = frames_for_watch(&mut w, streams, true);
        // "abc" advances the cursor to col 3 on row 0.
        assert!(f.contains("DELTA 9 seq="), "sid-tagged cursor delta: {f:?}");
        assert!(f.contains("cursor 0 3 "), "cursor row/col reported: {f:?}");
    }

    /// `since=<seq>` SEMANTICS at the frame level: seeding `last_sent_seq` to the
    /// CURRENT content seq means a fresh wake emits NOTHING (the client is already
    /// caught up); seeding it BELOW the current content seq emits an immediate
    /// catch-up DELTA.
    #[test]
    fn since_seeds_catch_up_only_when_content_advanced() {
        let term = Arc::new(Mutex::new(Terminal::new(24, 80)));
        crate::term_lock(&term).process(b"state");
        let cur = crate::term_lock(&term).content_seq();
        let streams = Streams {
            screen: true,
            ..Default::default()
        };

        // since == current seq: caught up, no catch-up frame.
        let mut caught_up = Watch {
            local_id: 1,
            term: term.clone(),
            last_sent_seq: cur,
            last_alt: false,
            last_cursor: (u16::MAX, u16::MAX, false, ""),
            // Seed to the LIVE render signature (as production does), so an unchanged
            // render does not spuriously trip the recolor/DECSCNM/DECTCEM re-emit path.
            last_render_sig: render_sig(&crate::term_lock(&term)),
            last_block_id: None,
            turns: std::sync::Arc::new(std::sync::Mutex::new(TurnLedger::default())),
            timeline: std::sync::Arc::new(std::sync::Mutex::new(
                crate::session_timeline::SessionTimeline::default(),
            )),
            last_timeline_id: None,
            last_turn_id: None,
            last_title: None,
            last_bell: 0,
            byte_sub: None,
            non_coalesced: false,
        };
        assert!(
            frames_for_watch(&mut caught_up, streams, true).is_empty(),
            "no frame when caught up"
        );

        // since below current seq: an immediate catch-up DELTA fires.
        let mut behind = Watch {
            local_id: 1,
            term,
            last_sent_seq: cur - 1,
            last_alt: false,
            last_cursor: (u16::MAX, u16::MAX, false, ""),
            last_render_sig: 0,
            last_block_id: None,
            turns: std::sync::Arc::new(std::sync::Mutex::new(TurnLedger::default())),
            timeline: std::sync::Arc::new(std::sync::Mutex::new(
                crate::session_timeline::SessionTimeline::default(),
            )),
            last_timeline_id: None,
            last_turn_id: None,
            last_title: None,
            last_bell: 0,
            byte_sub: None,
            non_coalesced: false,
        };
        assert!(
            frames_for_watch(&mut behind, streams, true).starts_with("DELTA 1 "),
            "catch-up delta when content advanced past since",
        );
    }

    /// CURSOR STREAM completeness: a pure caret move (CSI H) bumps no `content_seq`,
    /// so a seq-gated stream would never push it. The `cursor` stream must emit a
    /// DELTA on the change anyway (its whole purpose), matching the poll `cursor` verb.
    #[test]
    fn cursor_only_move_emits_delta_on_unchanged_seq() {
        let term = Arc::new(Mutex::new(Terminal::new(24, 80)));
        crate::term_lock(&term).process(b"abc"); // content advances; cursor at (0,3)
        let streams = Streams {
            cursor: true,
            ..Default::default()
        };
        let mut w = Watch {
            local_id: 5,
            term: term.clone(),
            last_sent_seq: 0,
            last_alt: false,
            last_cursor: (u16::MAX, u16::MAX, false, ""),
            last_render_sig: 0,
            last_block_id: None,
            turns: std::sync::Arc::new(std::sync::Mutex::new(TurnLedger::default())),
            timeline: std::sync::Arc::new(std::sync::Mutex::new(
                crate::session_timeline::SessionTimeline::default(),
            )),
            last_timeline_id: None,
            last_turn_id: None,
            last_title: None,
            last_bell: 0,
            byte_sub: None,
            non_coalesced: false,
        };
        // First wake syncs the cursor watermark off the content advance.
        assert!(frames_for_watch(&mut w, streams, true).contains("cursor 0 3 "));
        let seq_before = crate::term_lock(&term).content_seq();
        // CSI H moves the caret WITHOUT a cell write — content_seq stays put.
        crate::term_lock(&term).process(b"\x1b[10;5H");
        assert_eq!(
            crate::term_lock(&term).content_seq(),
            seq_before,
            "a pure cursor move must not bump content_seq (else this tests nothing)"
        );
        // The seq is unchanged, but the caret moved to (9,4): a cursor DELTA still fires.
        let f = frames_for_watch(&mut w, streams, true);
        assert!(
            f.contains("cursor 9 4 "),
            "cursor-only move emits a delta on an unchanged seq: {f:?}"
        );
    }

    /// CURSOR VISIBILITY (DECTCEM ?25l/?25h) bumps no `content_seq`, but the `cursor`
    /// stream carries the `<visible>` bit (matching the poll `cursor` verb), so a pure
    /// hide/show still emits a DELTA reflecting the flip.
    #[test]
    fn cursor_visibility_toggle_emits_delta_with_visible_bit() {
        let term = Arc::new(Mutex::new(Terminal::new(24, 80)));
        crate::term_lock(&term).process(b"hi"); // caret at (0,2), cursor visible
        let streams = Streams {
            cursor: true,
            ..Default::default()
        };
        let mut w = Watch {
            local_id: 1,
            term: term.clone(),
            last_sent_seq: 0,
            last_alt: false,
            last_cursor: (u16::MAX, u16::MAX, false, ""),
            last_render_sig: 0,
            last_block_id: None,
            turns: std::sync::Arc::new(std::sync::Mutex::new(TurnLedger::default())),
            timeline: std::sync::Arc::new(std::sync::Mutex::new(
                crate::session_timeline::SessionTimeline::default(),
            )),
            last_timeline_id: None,
            last_turn_id: None,
            last_title: None,
            last_bell: 0,
            byte_sub: None,
            non_coalesced: false,
        };
        // First wake syncs; the caret is visible (bit 1).
        assert!(
            frames_for_watch(&mut w, streams, true).contains("cursor 0 2 1 "),
            "initial cursor delta carries visible=1"
        );
        let seq_before = crate::term_lock(&term).content_seq();
        crate::term_lock(&term).process(b"\x1b[?25l"); // DECTCEM hide — no cell write
        assert_eq!(
            crate::term_lock(&term).content_seq(),
            seq_before,
            "a DECTCEM toggle must not bump content_seq (else this tests nothing)"
        );
        let f = frames_for_watch(&mut w, streams, true);
        assert!(
            f.contains("cursor 0 2 0 "),
            "a pure visibility hide emits a cursor delta with visible=0: {f:?}"
        );
    }

    /// A RECOLOR (OSC 4 palette / OSC 10-12 dynamic colour) or DECSCNM reverse-video
    /// mutates the styled output WITHOUT bumping `content_seq`, so a seq-gated `cells`
    /// subscriber would show stale colours. The per-wake render signature notices it
    /// and re-emits a `cells` DELTA at the unchanged seq (no GAP).
    #[test]
    fn recolor_reemits_cells_on_unchanged_seq() {
        let term = Arc::new(Mutex::new(Terminal::new(4, 8)));
        crate::term_lock(&term).process(b"x");
        let streams = Streams {
            cells: true,
            ..Default::default()
        };
        let mut w = Watch {
            local_id: 1,
            term: term.clone(),
            last_sent_seq: 0,
            last_alt: false,
            last_cursor: (u16::MAX, u16::MAX, false, ""),
            last_render_sig: 0,
            last_block_id: None,
            turns: std::sync::Arc::new(std::sync::Mutex::new(TurnLedger::default())),
            timeline: std::sync::Arc::new(std::sync::Mutex::new(
                crate::session_timeline::SessionTimeline::default(),
            )),
            last_timeline_id: None,
            last_turn_id: None,
            last_title: None,
            last_bell: 0,
            byte_sub: None,
            non_coalesced: false,
        };
        // First wake emits cells and syncs the render signature.
        assert!(frames_for_watch(&mut w, streams, true).contains(" cells "));
        let seq_before = crate::term_lock(&term).content_seq();
        // OSC 11: set the default BACKGROUND (the dark/light-toggle path) — a render
        // change with no cell write. (OSC 4 palette set is gated by
        // `allow_palette_reconfigure`; OSC 10/11/12 defaults are not.)
        crate::term_lock(&term).process(b"\x1b]11;rgb:00/00/80\x07");
        assert_eq!(
            crate::term_lock(&term).content_seq(),
            seq_before,
            "a recolor must not bump content_seq (else this tests nothing)"
        );
        let f = frames_for_watch(&mut w, streams, true);
        assert!(
            f.contains("cells ") && f.contains(&format!("seq={seq_before} ")),
            "recolor re-emits a cells DELTA at the unchanged seq: {f:?}"
        );
        assert!(
            !f.contains("GAP"),
            "a render-only change needs no GAP resync: {f:?}"
        );

        // DECSCNM reverse-video (a distinct render path via the reverse_video accessor)
        // likewise re-emits at an unchanged seq.
        let seq2 = crate::term_lock(&term).content_seq();
        crate::term_lock(&term).process(b"\x1b[?5h"); // DECSCNM on
        assert_eq!(
            crate::term_lock(&term).content_seq(),
            seq2,
            "DECSCNM bumps no content_seq"
        );
        assert!(
            frames_for_watch(&mut w, streams, true).contains("cells "),
            "a DECSCNM flip re-emits a cells DELTA at the unchanged seq"
        );
    }

    /// ALT-SCREEN swap (1049) does not move the MAIN grid's `content_seq`, so a swap
    /// landing on a seq the client already saw would be invisible to a pure seq
    /// compare. A flag flip must force a resync (GAP + full frame) even at equal seq.
    #[test]
    fn alt_screen_flip_forces_resync_even_at_equal_seq() {
        let term = Arc::new(Mutex::new(Terminal::new(24, 80)));
        crate::term_lock(&term).process(b"\x1b[?1049h"); // enter the alternate screen
        let (seq, alt) = {
            let t = crate::term_lock(&term);
            (t.content_seq(), t.is_alternate_screen())
        };
        assert!(alt, "1049h enters the alternate screen");
        let streams = Streams {
            screen: true,
            ..Default::default()
        };
        // The watch believes it is caught up at this seq but on the MAIN buffer
        // (last_alt=false): the alt flip is the ONLY thing that changed.
        let mut w = Watch {
            local_id: 1,
            term: term.clone(),
            last_sent_seq: seq,
            last_alt: false,
            last_cursor: (u16::MAX, u16::MAX, false, ""),
            last_render_sig: 0,
            last_block_id: None,
            turns: std::sync::Arc::new(std::sync::Mutex::new(TurnLedger::default())),
            timeline: std::sync::Arc::new(std::sync::Mutex::new(
                crate::session_timeline::SessionTimeline::default(),
            )),
            last_timeline_id: None,
            last_turn_id: None,
            last_title: None,
            last_bell: 0,
            byte_sub: None,
            non_coalesced: false,
        };
        let f = frames_for_watch(&mut w, streams, true);
        assert!(
            f.contains("GAP 1 ") && f.contains("DELTA 1 "),
            "an alt-screen flip at equal seq resyncs (GAP + full frame): {f:?}"
        );
        assert!(w.last_alt, "the alt watermark is updated after the resync");
    }

    /// ITEM 2: the `cells` and `bytes` stream tokens parse (additively with the
    /// existing screen/cursor/events).
    #[test]
    fn streams_parse_accepts_cells_and_bytes() {
        assert_eq!(
            Streams::parse("cells"),
            Some(Streams {
                cells: true,
                ..Default::default()
            })
        );
        assert_eq!(
            Streams::parse("bytes"),
            Some(Streams {
                bytes: true,
                ..Default::default()
            })
        );
        assert_eq!(
            Streams::parse("cells,bytes,screen"),
            Some(Streams {
                cells: true,
                bytes: true,
                screen: true,
                ..Default::default()
            }),
        );
        // `timestamps` / `ts` are MODIFIER tokens (no own frames).
        assert_eq!(
            Streams::parse("screen,timestamps"),
            Some(Streams {
                screen: true,
                timestamps: true,
                ..Default::default()
            })
        );
        assert_eq!(
            Streams::parse("cursor ts"),
            Some(Streams {
                cursor: true,
                timestamps: true,
                ..Default::default()
            })
        );
    }

    /// The `timestamps` modifier prefixes a wake's frames with a `T <sid> <t_us>`
    /// line (one per wake, video's `now_us` clock), turning the stream into a timed
    /// frame source. Off by default (byte-identical un-timestamped stream).
    #[test]
    fn timestamps_prefixes_a_wake_with_a_t_line() {
        let term = Arc::new(Mutex::new(Terminal::new(24, 80)));
        crate::term_lock(&term).process(b"hello");
        let streams_ts = Streams {
            screen: true,
            timestamps: true,
            ..Default::default()
        };
        let mut w = Watch {
            local_id: 7,
            term: term.clone(),
            last_sent_seq: 0,
            last_alt: false,
            last_cursor: (u16::MAX, u16::MAX, false, ""),
            last_render_sig: 0,
            last_block_id: None,
            turns: std::sync::Arc::new(std::sync::Mutex::new(TurnLedger::default())),
            timeline: std::sync::Arc::new(std::sync::Mutex::new(
                crate::session_timeline::SessionTimeline::default(),
            )),
            last_timeline_id: None,
            last_turn_id: None,
            last_title: None,
            last_bell: 0,
            byte_sub: None,
            non_coalesced: false,
        };
        let f = frames_for_watch(&mut w, streams_ts, true);
        assert!(f.starts_with("T 7 "), "wake prefixed with a T line: {f:?}");
        assert!(
            f.contains("\nDELTA 7 "),
            "the frames follow the T line: {f:?}"
        );
        // Without the modifier, no T line.
        let mut w2 = Watch {
            local_id: 7,
            term,
            last_sent_seq: 0,
            last_alt: false,
            last_cursor: (u16::MAX, u16::MAX, false, ""),
            last_render_sig: 0,
            last_block_id: None,
            turns: std::sync::Arc::new(std::sync::Mutex::new(TurnLedger::default())),
            timeline: std::sync::Arc::new(std::sync::Mutex::new(
                crate::session_timeline::SessionTimeline::default(),
            )),
            last_timeline_id: None,
            last_turn_id: None,
            last_title: None,
            last_bell: 0,
            byte_sub: None,
            non_coalesced: false,
        };
        let f2 = frames_for_watch(
            &mut w2,
            Streams {
                screen: true,
                ..Default::default()
            },
            true,
        );
        assert!(
            f2.starts_with("DELTA 7 "),
            "no T line without the modifier: {f2:?}"
        );
    }

    /// A `cells` DELTA carries the LOSSLESS styled-screen JSON payload (Item 1),
    /// length-prefixed, on content advance.
    #[test]
    fn cells_delta_carries_styled_payload() {
        let term = Arc::new(Mutex::new(Terminal::new(2, 4)));
        crate::term_lock(&term).process(b"\x1b[1mhi");
        let streams = Streams {
            cells: true,
            ..Default::default()
        };
        let mut w = Watch {
            local_id: 5,
            term,
            last_sent_seq: 0,
            last_alt: false,
            last_cursor: (u16::MAX, u16::MAX, false, ""),
            last_render_sig: 0,
            last_block_id: None,
            turns: std::sync::Arc::new(std::sync::Mutex::new(TurnLedger::default())),
            timeline: std::sync::Arc::new(std::sync::Mutex::new(
                crate::session_timeline::SessionTimeline::default(),
            )),
            last_timeline_id: None,
            last_turn_id: None,
            last_title: None,
            last_bell: 0,
            byte_sub: None,
            non_coalesced: false,
        };
        let f = frames_for_watch(&mut w, streams, true);
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

    /// SESSION-METADATA stage 1 — the events digest pushes `EVENT <sid> meta
    /// <payload>` for each `meta-change` timeline record past the watermark,
    /// SKIPS non-meta lifecycle records, and advances the watermark to the
    /// timeline HIGH so nothing double-emits and non-meta records are never
    /// re-walked. A seeded watermark (subscription-time high) means only
    /// post-subscription changes push — a live stream, not a replay.
    #[test]
    fn meta_change_drains_as_a_meta_event_and_watermark_advances() {
        let timeline = std::sync::Arc::new(std::sync::Mutex::new(
            crate::session_timeline::SessionTimeline::default(),
        ));
        // Pre-subscription history: a spawn + an OLD meta change.
        timeline
            .lock()
            .unwrap()
            .record("spawned", "state=alive".to_string());
        timeline
            .lock()
            .unwrap()
            .record("meta-change", "field=title value=old".to_string());
        let seeded = initial_timeline_watermark(
            &timeline,
            Streams {
                events: true,
                ..Default::default()
            },
        );
        assert_eq!(seeded, Some(2), "seeded to the live high");

        // Nothing new: no frames, watermark stays.
        let mut out = String::new();
        let wm = drain_meta_events(&timeline, "7", seeded, &mut out);
        assert!(
            out.is_empty(),
            "no replay of pre-subscription history: {out}"
        );
        assert_eq!(wm, Some(2));

        // A post-subscription meta change + an interleaved non-meta record: only
        // the meta record pushes; the watermark passes BOTH.
        timeline
            .lock()
            .unwrap()
            .record("meta-change", "field=title value=build%20agent".to_string());
        timeline
            .lock()
            .unwrap()
            .record("state-change", "state=exited".to_string());
        let mut out = String::new();
        let wm = drain_meta_events(&timeline, "7", wm, &mut out);
        assert_eq!(
            out, "EVENT 7 meta field=title value=build%20agent\n",
            "exactly the meta record, verbatim payload"
        );
        assert_eq!(wm, Some(4), "watermark passes the non-meta record too");
        // Drained again: silence (no double emit).
        let mut out = String::new();
        let wm2 = drain_meta_events(&timeline, "7", wm, &mut out);
        assert!(out.is_empty());
        assert_eq!(wm2, Some(4));
    }

    /// The `bytes` stream drains EVERY burst byte-exactly (incl. non-UTF-8) as
    /// length-prefixed `BYTES` frames — live and every-frame, no coalescing.
    #[test]
    fn bytes_drain_is_byte_lossless_and_every_frame() {
        let fan = Arc::new(ByteFanout::new());
        let term = Arc::new(Mutex::new(Terminal::new(2, 4)));
        let bs = fan.subscribe();
        fan.tee(&Arc::from(&b"\x1b[31m"[..]));
        fan.tee(&Arc::from(&[0x80u8, 0x00][..])); // non-UTF-8 + NUL
        let mut w = Watch {
            local_id: 7,
            term,
            last_sent_seq: 0,
            last_alt: false,
            last_cursor: (u16::MAX, u16::MAX, false, ""),
            last_render_sig: 0,
            last_block_id: None,
            turns: std::sync::Arc::new(std::sync::Mutex::new(TurnLedger::default())),
            timeline: std::sync::Arc::new(std::sync::Mutex::new(
                crate::session_timeline::SessionTimeline::default(),
            )),
            last_timeline_id: None,
            last_turn_id: None,
            last_title: None,
            last_bell: 0,
            byte_sub: Some(bs),
            non_coalesced: false,
        };
        let out = drain_bytes_frames(&mut w);
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
        for _ in 0..10 {
            fan.tee(&Arc::from(&b"abcd"[..]));
        }
        let mut w = Watch {
            local_id: 7,
            term,
            last_sent_seq: 0,
            last_alt: false,
            last_cursor: (u16::MAX, u16::MAX, false, ""),
            last_render_sig: 0,
            last_block_id: None,
            turns: std::sync::Arc::new(std::sync::Mutex::new(TurnLedger::default())),
            timeline: std::sync::Arc::new(std::sync::Mutex::new(
                crate::session_timeline::SessionTimeline::default(),
            )),
            last_timeline_id: None,
            last_turn_id: None,
            last_title: None,
            last_bell: 0,
            byte_sub: Some(bs),
            non_coalesced: false,
        };
        let out = String::from_utf8_lossy(&drain_bytes_frames(&mut w)).into_owned();
        assert!(
            out.starts_with("GAP 7 bytes-dropped="),
            "gap precedes bursts: {out}"
        );
        assert!(out.contains("BYTES 7 4\n"), "newest burst survives: {out}");
    }

    /// The `events` digest emits `EVENT <sid> title <pct>` on a title change and
    /// NOT on an unchanged re-scan (watermark), with the title pct-encoded so a
    /// space/newline in it stays on one line.
    #[test]
    fn drain_title_event_emits_once_per_change_pct_encoded() {
        let term = Arc::new(Mutex::new(Terminal::new(24, 80)));
        // OSC 2 sets the window title to "my dir" (a space -> pct-encoded).
        crate::term_lock(&term).process(b"\x1b]2;my dir\x07");
        let mut out = String::new();
        // First drain with no watermark emits the current title.
        let wm = drain_title_event(&term, "3", None, &mut out);
        assert!(
            out.contains("EVENT 3 title my%20dir\n"),
            "title emitted + pct-encoded: {out:?}"
        );
        // A re-scan with the same title emits nothing.
        out.clear();
        let wm = drain_title_event(&term, "3", wm, &mut out);
        assert!(out.is_empty(), "unchanged title emits nothing: {out:?}");
        // A new title emits again.
        crate::term_lock(&term).process(b"\x1b]2;other\x07");
        out.clear();
        let _ = drain_title_event(&term, "3", wm, &mut out);
        assert!(
            out.contains("EVENT 3 title other\n"),
            "change re-emits: {out:?}"
        );
    }

    /// The `sessions` stream emits `EVENT * session-created <sid>` for a sid newly
    /// live and `EVENT * session-exited <sid>` for one gone, and NOTHING for an
    /// unchanged set (watermark) — so a supervisor learns of a SIBLING spawn/exit
    /// without polling `ls`.
    #[test]
    fn diff_session_events_emits_created_and_exited() {
        use std::collections::HashSet;
        let known: HashSet<String> = ["s-aaa".to_string(), "s-bbb".to_string()].into();
        // A new session (s-ccc) appears; s-bbb is gone.
        let live: HashSet<String> = ["s-aaa".to_string(), "s-ccc".to_string()].into();
        let mut out = String::new();
        diff_session_events(&live, &known, &mut out);
        assert!(
            out.contains("EVENT * session-created s-ccc\n"),
            "created: {out:?}"
        );
        assert!(
            out.contains("EVENT * session-exited s-bbb\n"),
            "exited: {out:?}"
        );
        assert!(
            !out.contains("s-aaa"),
            "unchanged session emits nothing: {out:?}"
        );
        // No change -> nothing.
        let mut out2 = String::new();
        diff_session_events(&live, &live, &mut out2);
        assert!(out2.is_empty(), "unchanged set emits nothing: {out2:?}");
    }

    /// The `events` digest emits `EVENT <sid> bell total=<n>` when the monotonic
    /// fired-bell count advances, and NOT on an unchanged re-scan (watermark).
    #[test]
    fn drain_bell_event_emits_on_new_bells() {
        let term = Arc::new(Mutex::new(Terminal::new(24, 80)));
        let mut out = String::new();
        // No bells yet: baseline 0, nothing emitted.
        let wm = drain_bell_event(&term, "4", 0, &mut out);
        assert!(out.is_empty() && wm == 0, "no bell yet: {out:?}");
        // A BEL fires: total advances to 1, event emitted.
        crate::term_lock(&term).process(b"\x07");
        out.clear();
        let wm = drain_bell_event(&term, "4", wm, &mut out);
        assert!(
            out.contains("EVENT 4 bell total=1\n"),
            "bell emitted: {out:?}"
        );
        // A re-scan with no new bell emits nothing.
        out.clear();
        let _ = drain_bell_event(&term, "4", wm, &mut out);
        assert!(
            out.is_empty(),
            "unchanged bell total emits nothing: {out:?}"
        );
    }

    /// The `events` digest surfaces new TURN records as `EVENT <sid> turn …`
    /// lines, scanning the ledger by id watermark exactly like block-complete:
    /// a record past the watermark emits once and advances it; a re-scan with no
    /// new record emits nothing (no double-report).
    #[test]
    fn drain_turn_events_emits_once_per_new_record() {
        use crate::turn_ledger::{TurnLedger, TurnRecord};
        let ledger = Arc::new(Mutex::new(TurnLedger::default()));
        let push = |id: u64, submitted: bool, status: &'static str| {
            ledger.lock().unwrap().push(TurnRecord {
                id,
                started_ms: id,
                dur_ms: 7,
                submitted,
                status,
                text: format!("m{id}"),
                screen_hash: id,
                seq: id,
            });
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

        // A NEW turn emits exactly one digest line and advances the watermark.
        push(2, false, "timeout");
        out.clear();
        wm = drain_turn_events(&ledger, "3", wm, &mut out);
        assert_eq!(out, "EVENT 3 turn 2 submitted=0 status=timeout dur_ms=7\n");
        assert_eq!(wm, Some(2));

        // Re-scan with nothing new: silent.
        out.clear();
        let wm2 = drain_turn_events(&ledger, "3", wm, &mut out);
        assert!(out.is_empty() && wm2 == Some(2), "no double-report");
    }

    /// `every-frame` mode re-emits the `cells` frame even when `content_seq` is
    /// unchanged (animation fidelity); coalesced mode emits nothing on no change.
    #[test]
    fn every_frame_reemits_cells_on_unchanged_seq() {
        let term = Arc::new(Mutex::new(Terminal::new(2, 4)));
        crate::term_lock(&term).process(b"x");
        let streams = Streams {
            cells: true,
            ..Default::default()
        };
        // Coalesced: second call on unchanged seq emits nothing.
        let mut w = Watch {
            local_id: 1,
            term: term.clone(),
            last_sent_seq: 0,
            last_alt: false,
            last_cursor: (u16::MAX, u16::MAX, false, ""),
            last_render_sig: 0,
            last_block_id: None,
            turns: std::sync::Arc::new(std::sync::Mutex::new(TurnLedger::default())),
            timeline: std::sync::Arc::new(std::sync::Mutex::new(
                crate::session_timeline::SessionTimeline::default(),
            )),
            last_timeline_id: None,
            last_turn_id: None,
            last_title: None,
            last_bell: 0,
            byte_sub: None,
            non_coalesced: false,
        };
        assert!(frames_for_watch(&mut w, streams, true).starts_with("DELTA 1 "));
        assert!(
            frames_for_watch(&mut w, streams, true).is_empty(),
            "coalesced: no re-emit on unchanged seq"
        );
        // every-frame: re-emits cells on unchanged seq.
        let mut w2 = Watch {
            local_id: 1,
            term,
            last_sent_seq: 0,
            last_alt: false,
            last_cursor: (u16::MAX, u16::MAX, false, ""),
            last_render_sig: 0,
            last_block_id: None,
            turns: std::sync::Arc::new(std::sync::Mutex::new(TurnLedger::default())),
            timeline: std::sync::Arc::new(std::sync::Mutex::new(
                crate::session_timeline::SessionTimeline::default(),
            )),
            last_timeline_id: None,
            last_turn_id: None,
            last_title: None,
            last_bell: 0,
            byte_sub: None,
            non_coalesced: true,
        };
        assert!(frames_for_watch(&mut w2, streams, true).starts_with("DELTA 1 "));
        assert!(
            frames_for_watch(&mut w2, streams, true).contains(" cells "),
            "every-frame re-emits cells on unchanged seq",
        );
    }

    /// R5 efficiency: an every-frame subscriber does NOT re-emit on a bare liveness
    /// TIMEOUT (`woke == false`) — only a genuine wake re-serializes the grid. This
    /// is what keeps an idle animation-mode subscriber from burning a full styled
    /// gather every 250ms tick.
    #[test]
    fn every_frame_does_not_reemit_on_timeout() {
        let term = Arc::new(Mutex::new(Terminal::new(2, 4)));
        crate::term_lock(&term).process(b"x");
        let streams = Streams {
            cells: true,
            ..Default::default()
        };
        let mut w = Watch {
            local_id: 1,
            term,
            last_sent_seq: 0,
            last_alt: false,
            last_cursor: (u16::MAX, u16::MAX, false, ""),
            last_render_sig: 0,
            last_block_id: None,
            turns: std::sync::Arc::new(std::sync::Mutex::new(TurnLedger::default())),
            timeline: std::sync::Arc::new(std::sync::Mutex::new(
                crate::session_timeline::SessionTimeline::default(),
            )),
            last_timeline_id: None,
            last_turn_id: None,
            last_title: None,
            last_bell: 0,
            byte_sub: None,
            non_coalesced: true,
        };
        // First (genuine) wake emits the frame and catches the watermark up.
        assert!(frames_for_watch(&mut w, streams, true).starts_with("DELTA 1 "));
        // A TIMEOUT tick on the now-unchanged seq re-emits NOTHING (the efficiency
        // win); a genuine wake still would (proven by the test above).
        assert!(
            frames_for_watch(&mut w, streams, false).is_empty(),
            "every-frame must not re-emit on a bare liveness timeout",
        );
    }
}
