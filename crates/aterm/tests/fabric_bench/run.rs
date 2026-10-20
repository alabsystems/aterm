// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The measured phases, over two worlds booted exactly as the fabric suite
//! boots them (`harness::World`: a guarded — so Strict — broker in this
//! process, a headless `aterm`, and the real `aterm link serve` child it
//! launched):
//!
//! 1. MESSAGING, on one node. [`latency`]: session A posts a `note` to session
//!    B, one at a time on an idle bridge, and the clock runs from the `post`
//!    reply to B's `await inbox` answering. [`burst`]: N posts back to back
//!    from sender sessions to B (within B's allowance, [`BURST_WINDOW`]), with
//!    B consuming as a real reader does. Then the census of the records each
//!    message cost.
//! 2. PING-PONG, on a node of its own ([`pingpong`]): A posts, B replies the
//!    moment it sees the message, back to back, with the bridge's wait tally
//!    on — the reply's wait on the delivery's inbox commit and the bridge's one
//!    loop, which the latency phase waits out on purpose.
//! 3. ATTACH, on a third node: a bridge that starts with N sessions, until
//!    its node row and every session's presence row are on the broker again,
//!    for each population in the spec; the census of what it wrote, the
//!    killed bridge's fired will counted apart.
//!
//! Each world also runs the [`calibrate`] yardstick — bare Strict publishes,
//! no aterm — before and after its phases, and every headline is reported in
//! yardstick units beside its milliseconds: a correction for the disk, not
//! for a busy CPU.
//!
//! Separate worlds, not one, so the attach populations are exactly what the
//! spec names (sessions are never closed here), the messaging numbers are not
//! taken beside thirty sessions' roster traffic, and only the ping-pong
//! world's bridge runs with a development seam set.
//!
//! THE POST IS THE DEFAULT ONE: `kind=note` without `--wait`, which answers
//! `OK <id>` once aterm has queued it in the session's outbox. So "post ->
//! visible" spans everything the fabric does for a message: the bridge's
//! outbox drain, the publish and its fsync, the bridge reading its own inbox
//! subscription, the `deliver` into B's ring, and B's parked `await` waking.
//! The broker-side landing time ([`crate::clock`]) splits it in two.
//!
//! THE RECIPIENT READS LIKE A READER. aterm's ring holds at most 64 UNLISTED
//! rows from one peer node (`aterm-gui` `SENDER_QUOTA`), and every sender here
//! is the same node, so a recipient that never listed would refuse the 65th
//! message and the bench would measure refusals. After every wake B lists what
//! arrived (`inbox since=<id>`, not `--peek`), which is what releases the
//! quota — and a listing records nothing the bridge acts on, so it wakes no
//! bridge work into the next measured message. The burst's sender keeps inside
//! that allowance too ([`BURST_WINDOW`]): a reader can always be descheduled
//! for longer than a fast bridge takes to deliver 64 rows.
//!
//! EVERY ROW IS ACCOUNTED FOR. A phase hands the next the newest row id it
//! listed, and every listing must hold only the rows that phase sent, each
//! once: a message delivered twice, or a late repeat of an earlier phase's,
//! fails the run by name instead of being timed as a message.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::Path;
use std::sync::atomic::AtomicBool;
use std::sync::{Condvar, Mutex, PoisonError};
use std::time::{Duration, Instant};

use astream_broker::{Broker, BrokerHandle, Durability, Subscription};
use aterm_json::{Value, json};
use aterm_link::ctl::{Ctl, Reply};

use crate::census::{self, Rec};
use crate::clock::{self, Clock, StopOnDrop, Timeline, ms};
use crate::harness::{self, DEADLINE, FLEET, World, kv, until};
use crate::profile::{self, Profile};
use crate::spec::{Spec, Tier};
use crate::stats;

#[path = "run/pingpong.rs"]
mod pingpong;

/// What a run hands back.
pub struct Report {
    pub json: Value,
    pub markdown: String,
}

/// How long the bench lets a bridge run before it kills it again.
///
/// aterm's bridge supervisor (`aterm-gui` `fabric_launch.rs`) doubles its
/// relaunch back-off after a child that ran less than `RELAUNCH_HEALTHY` (5 s)
/// and resets it to its 200 ms floor after one that ran longer. The attach
/// rounds kill the bridge on purpose, so each waits out the healthy mark first
/// (with a margin for the bench's own view of the start lagging the bridge's)
/// — otherwise round nine would wait 25 s for its relaunch. NOT a
/// synchronisation sleep: nothing it precedes depends on the time passing,
/// only the run's length does, and the measured attach starts at the new
/// bridge's own pid file, after the back-off.
const SUPERVISOR_HEALTHY: Duration = Duration::from_millis(5_500);

/// How often the pid file is read while a killed bridge comes back: it marks
/// the start of an attach, so it is read finely. A file read, so it puts no
/// load on the broker being measured.
const PID_POLL: Duration = Duration::from_millis(1);

// ---------------------------------------------------------------------------
// the control-socket verbs the phases speak
// ---------------------------------------------------------------------------

fn request(ctl: &mut Ctl, line: &str) -> Reply {
    ctl.request(line).unwrap_or_else(|e| panic!("{line}: {e}"))
}

/// `from` posts a note to `to`: `(stamp before the request, stamp after the
/// reply)`.
fn post(ctl: &mut Ctl, clock: Clock, from: &str, to: &str, text: &str) -> (u64, u64) {
    let line = format!("@{from} post to=@{to} kind=note -- {text}");
    let t_call = clock.us();
    let reply = request(ctl, &line);
    let t_ret = clock.us();
    assert!(reply.ok(), "{line}: {}", reply.header());
    (t_call, t_ret)
}

/// Park on `sid`'s inbox until a note row newer than `since` is there: `(the
/// newest such row's id, the stamp when the answer arrived)`.
///
/// # Errors
///
/// Any answer but a row — `OK timeout` after the hang detector — as the
/// request and its reply.
fn await_note(ctl: &mut Ctl, clock: Clock, sid: &str, since: u64) -> Result<(u64, u64), String> {
    // A minute: the hang detector (`harness::DEADLINE`), never a budget.
    let line = format!(
        "@{sid} await inbox since={since} kinds=note timeout={}",
        DEADLINE.as_millis()
    );
    let reply = request(ctl, &line);
    let t = clock.us();
    let header = reply.header();
    header
        .strip_prefix("OK inbox ")
        .and_then(|rest| rest.split_whitespace().next())
        .and_then(|id| id.parse().ok())
        .map(|id| (id, t))
        .ok_or_else(|| format!("{line}: {header}"))
}

/// One `msg` row of an `inbox` listing.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Row {
    id: u64,
    off: u64,
    text: String,
}

/// The `msg` rows of an `inbox` reply.
fn rows_of(reply: &Reply) -> Vec<Row> {
    reply
        .rows()
        .iter()
        .filter(|r| r.starts_with("msg "))
        .filter_map(|r| {
            Some(Row {
                id: r.split_whitespace().nth(1)?.parse().ok()?,
                off: kv(r, "off")?.parse().ok()?,
                text: aterm_link::pct::decode(kv(r, "text").unwrap_or_default()),
            })
        })
        .collect()
}

/// LIST `sid`'s rows newer than `since` — not `--peek`: listing is what
/// releases the sender's quota (see the module note).
fn list_since(ctl: &mut Ctl, sid: &str, since: u64) -> Vec<Row> {
    let line = format!("@{sid} inbox since={since}");
    let reply = request(ctl, &line);
    assert!(reply.ok(), "{line}: {}", reply.header());
    rows_of(&reply)
}

/// The newest row id `sid` holds, or 0.
fn newest_id(ctl: &mut Ctl, sid: &str) -> u64 {
    let reply = request(ctl, &format!("@{sid} inbox --peek --meta"));
    rows_of(&reply).iter().map(|r| r.id).max().unwrap_or(0)
}

/// Spawn one more session: its sid.
fn spawn(w: &World) -> String {
    let reply = w.verb("spawn");
    assert!(reply.ok(), "spawn: {}", reply.header());
    reply
        .header()
        .split_whitespace()
        .nth(1)
        .expect("spawn answers OK <sid>")
        .to_string()
}

/// Every presence row under this world's node, by subject: `(offset, body)`.
fn presence_rows(w: &World) -> HashMap<String, (u64, String)> {
    let filter = format!("/f/{FLEET}/pub/{}/*/presence", w.node);
    w.read(|c| c.last_all(&filter))
        .map(|(rows, _)| {
            rows.into_iter()
                .map(|(off, subject, body)| {
                    (subject, (off, String::from_utf8_lossy(&body).into_owned()))
                })
                .collect()
        })
        .unwrap_or_default()
}

fn presence_subject(w: &World, owner: &str) -> String {
    format!("/f/{FLEET}/pub/{}/{owner}/presence", w.node)
}

/// Wait until the node row and every one of `sids`' presence rows say
/// `state=live` at an offset at or past `from`.
fn wait_live(w: &World, sids: &[String], from: u64, what: &str) {
    let mut owners: Vec<&str> = sids.iter().map(String::as_str).collect();
    owners.push("node");
    until(what, || {
        let rows = presence_rows(w);
        owners
            .iter()
            .all(|o| {
                rows.get(&presence_subject(w, o))
                    .is_some_and(|(off, body)| *off >= from && body.contains(" state=live "))
            })
            .then_some(())
    });
}

/// One row a consumer saw: which message (its index), at which broker offset,
/// stamped when.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Seen {
    j: usize,
    off: u64,
    t: u64,
}

/// What a reader saw.
struct Consumed {
    /// Every row it listed, in arrival order, A REPEAT INCLUDED: a message
    /// delivered twice is two entries here, and [`tally`] refuses it.
    seen: Vec<Seen>,
    /// The newest row id it listed: where the next phase's reading starts.
    since: u64,
}

/// Read `sid`'s inbox like a reader until `want` DISTINCT messages whose text
/// is `<prefix><j>` have arrived: each wake is followed by a listing, and a
/// row is stamped VISIBLE at the wake that covered it (or at the listing, for
/// one that arrived between the two). After each listing it tells `credit`
/// how many it has listed.
///
/// # Errors
///
/// A wait that timed out, or a row whose text is not `<prefix><j>` — a stray,
/// or a late repeat of the previous phase's message, which this phase must not
/// time as one of its own.
fn consume(
    ctl: &mut Ctl,
    clock: Clock,
    sid: &str,
    mut since: u64,
    prefix: &str,
    want: usize,
    credit: Option<&Credit>,
) -> Result<Consumed, String> {
    let mut seen = Vec::with_capacity(want);
    let mut distinct = HashSet::with_capacity(want);
    while distinct.len() < want {
        let (woke_at, t_wake) = await_note(ctl, clock, sid, since)?;
        let rows = list_since(ctl, sid, since);
        let t_list = clock.us();
        for row in rows {
            let j = row
                .text
                .strip_prefix(prefix)
                .and_then(|j| j.parse::<usize>().ok())
                .ok_or_else(|| {
                    format!(
                        "@{sid}'s inbox holds a row this phase did not send while it waited for \
                         `{prefix}*`: {row:?} (a late repeat of an earlier message, or a stray)"
                    )
                })?;
            let t = if row.id <= woke_at { t_wake } else { t_list };
            seen.push(Seen { j, off: row.off, t });
            distinct.insert(j);
            since = since.max(row.id);
        }
        if let Some(credit) = credit {
            credit.listed(distinct.len());
        }
    }
    Ok(Consumed { seen, since })
}

/// Why a phase's rows are not each of its messages exactly once.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Miscount {
    /// Message `j` arrived as two rows: at one offset (the endpoint's `off=`
    /// dedup failed) or at two (the bus holds the record twice).
    Twice { j: usize, first: u64, again: u64 },
    /// Message `j` never arrived.
    Never(usize),
    /// A row names message `j`, past the `n` that were sent.
    Unsent(usize),
}

impl std::fmt::Display for Miscount {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Twice { j, first, again } => write!(
                f,
                "message {j} was delivered twice: rows at off={first} and off={again}"
            ),
            Self::Never(j) => write!(f, "message {j} was never seen"),
            Self::Unsent(j) => write!(f, "message {j} was seen, but was never sent"),
        }
    }
}

/// Each of `n` messages' row, by index. Pure (unit-tested).
///
/// # Errors
///
/// The first [`Miscount`]: a message seen twice, one never seen, or an index
/// past `n`.
fn tally(seen: &[Seen], n: usize) -> Result<Vec<Seen>, Miscount> {
    let mut by_j: Vec<Option<Seen>> = vec![None; n];
    for s in seen {
        let slot = by_j.get_mut(s.j).ok_or(Miscount::Unsent(s.j))?;
        if let Some(first) = slot {
            return Err(Miscount::Twice {
                j: s.j,
                first: first.off,
                again: s.off,
            });
        }
        *slot = Some(*s);
    }
    by_j.into_iter()
        .enumerate()
        .map(|(j, s)| s.ok_or(Miscount::Never(j)))
        .collect()
}

/// How many burst posts may be in flight — posted, not yet LISTED by the
/// reader — at once.
///
/// aterm refuses a delivery while 64 rows from the sending peer sit unlisted in
/// the recipient's ring (`aterm-gui` `SENDER_QUOTA`; every sender here is one
/// node, so one allowance), and the bridge books that refusal as final: an
/// `undeliverable` on the sender's lane, the record committed past, never
/// retried. A burst sent with no bound therefore holds only while the reader
/// keeps within 64 rows of the bridge — true while one fsync'd commit per
/// delivery paces the bridge at ~100 messages/s, and false the moment a faster
/// bridge (the commit batching this bench exists to measure) can deliver 64
/// rows during one scheduling stall of the reader. So the sender keeps the
/// allowance itself: unlisted rows from this node never exceed the posts in
/// flight, and this keeps those under 64 with a margin. The burst is still
/// back to back wherever the endpoint permits it — exactly what any sender
/// that must not be refused can do.
const BURST_WINDOW: usize = 56;

/// aterm's per-peer allowance of unlisted rows (`aterm-gui` `SENDER_QUOTA`,
/// crate-private there), which [`BURST_WINDOW`] stays under.
const RECIPIENT_ALLOWANCE: usize = 64;
const _: () = assert!(BURST_WINDOW < RECIPIENT_ALLOWANCE);

/// Whether post `posted` (0-based) may go out with `listed` messages listed.
/// Pure (unit-tested).
fn may_post(posted: usize, listed: usize, window: usize) -> bool {
    posted < listed + window
}

/// The burst's flow control: the reader says how many messages it has listed,
/// and the sender waits for room in [`BURST_WINDOW`] before each post.
struct Credit {
    window: usize,
    /// `(listed, the reader has stopped)`.
    state: Mutex<(usize, bool)>,
    moved: Condvar,
}

impl Credit {
    fn new(window: usize) -> Self {
        Self {
            window,
            state: Mutex::new((0, false)),
            moved: Condvar::new(),
        }
    }

    /// The reader has listed `n` distinct messages.
    fn listed(&self, n: usize) {
        let mut s = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        s.0 = s.0.max(n);
        self.moved.notify_all();
    }

    /// The reader has stopped, done or failed: it will list nothing more.
    fn close(&self) {
        self.state.lock().unwrap_or_else(PoisonError::into_inner).1 = true;
        self.moved.notify_all();
    }

    /// Wait until post `posted` may go out. `false` when the reader stopped
    /// first: its own answer says why.
    ///
    /// # Panics
    ///
    /// After [`DEADLINE`] with the reader still running and no room — the hang
    /// detector.
    fn acquire(&self, posted: usize) -> bool {
        let s = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        let (s, _) = self
            .moved
            .wait_timeout_while(s, DEADLINE, |(listed, stopped)| {
                !*stopped && !may_post(posted, *listed, self.window)
            })
            .unwrap_or_else(PoisonError::into_inner);
        if may_post(posted, s.0, self.window) {
            return true;
        }
        assert!(
            s.1,
            "burst post {posted}: the reader listed {} and made no room in {DEADLINE:?}",
            s.0
        );
        false
    }
}

/// Closes its [`Credit`] when dropped — on the reader's unwind too, so a
/// failed reader never leaves the sender waiting out the hang detector.
struct CloseOnDrop<'a>(&'a Credit);

impl Drop for CloseOnDrop<'_> {
    fn drop(&mut self) {
        self.0.close();
    }
}

/// Every `undeliverable` verdict in `sids`' inboxes, read with `--peek` (so
/// nothing is released): what a refused delivery leaves on its sender's lane,
/// for a failure message.
fn refusals(w: &World, sids: &[String]) -> String {
    let mut ctl = w.ctl();
    let found: Vec<String> = sids
        .iter()
        .flat_map(|sid| {
            let reply = request(&mut ctl, &format!("@{sid} inbox --peek"));
            reply
                .rows()
                .iter()
                .filter(|r| r.contains(" kind=undeliverable "))
                .map(|r| format!("@{sid}: {r}"))
                .collect::<Vec<_>>()
        })
        .collect();
    if found.is_empty() {
        "no sender holds an `undeliverable` verdict".to_string()
    } else {
        format!(
            "refused deliveries on the senders' lanes:\n{}",
            found.join("\n")
        )
    }
}

/// Stop the world's broker and read back every record it wrote.
fn stop_and_read(w: &mut World) -> Vec<Rec> {
    if let Some(mut handle) = w.handle.take() {
        handle.shutdown();
    }
    let recs = census::read_log(std::path::Path::new(&w.broker_log));
    w.broker.take();
    recs
}

/// Run `phases` beside a head sampler on `w`'s broker: what they answered, and
/// the timeline.
fn sampled<T: Send>(w: &World, clock: Clock, phases: impl FnOnce() -> T + Send) -> (T, Timeline) {
    let broker = w.broker.as_ref().expect("the world serves its own broker");
    let stop = AtomicBool::new(false);
    std::thread::scope(|s| {
        let sampler = s.spawn(|| clock::sample_heads(clock, || broker.visible_head(), &stop));
        let answer = {
            let _stop = StopOnDrop(&stop);
            phases()
        };
        (answer, sampler.join().expect("the head sampler"))
    })
}

/// `ms(a, b)` for a landing the sampler may not have seen.
fn ms_opt(a: Option<u64>, b: Option<u64>) -> Option<f64> {
    Some(ms(a?, b?))
}

// ---------------------------------------------------------------------------
// the yardstick: the disk, measured beside the stack
// ---------------------------------------------------------------------------

/// Publishes in one yardstick block.
const CALIBRATION_PUBLISHES: usize = 50;

/// The producer id the yardstick publishes under. No bridge's (a bridge's is
/// its own, bound to its node's cap), so its records never meet a bridge's
/// dedup; its subject (`/f/<F>/bench/…`) is under no filter a bridge holds.
const CALIBRATION_PRODUCER: u64 = 0x0fab_0be7_0000_0001;

/// THE YARDSTICK: `CALIBRATION_PUBLISHES` bare Strict publishes, one at a time,
/// from a client in this process to the world's own broker — one round trip
/// and one fsync each, no aterm, no bridge. Every number above it is
/// "some fsyncs plus aterm's work", and the fsync is the machine's: on a
/// shared host another job's writes stretch every `F_FULLFSYNC` (each one
/// flushes the drive's whole cache). So each world runs a block before its
/// phases and one after, and the report divides by the yardstick's p50 as
/// well as quoting milliseconds — a ratio taken in one run on one disk.
///
/// WHAT IT DOES NOT CORRECT FOR: CPU contention. It times one thread's round
/// trip and one fsync; the phases above it hop through three processes and
/// several threads, and on a loaded host each hop waits for a core the
/// yardstick never asked for. The baseline's own runs show it (2026-10-16:
/// 2.40 / 1.65 / 1.55 yardstick units for one build at load 15-28 and 12-13),
/// so the ratio narrows the spread between runs and does not remove it; the
/// ledger entry's load average is the only CPU measure. Reported, never
/// judged.
///
/// ALWAYS STRICT. Under the ceiling arm (`durability=relaxed`) the world's
/// broker no longer fsyncs, so its publishes would time the page cache, not
/// the disk; the yardstick then publishes to a Strict broker of its own
/// ([`Ruler`]), so every run's unit is the same fsync and the arms compare in
/// it.
fn calibrate(ruler: &Ruler, w: &World, clock: Clock, seq: &mut u64) -> Vec<f64> {
    let mut god = ruler.client(w);
    let subject = format!("/f/{FLEET}/bench/strict-publish");
    (0..CALIBRATION_PUBLISHES)
        .map(|_| {
            *seq += 1;
            let t0 = clock.us();
            god.publish(CALIBRATION_PRODUCER, *seq, &subject, b"yardstick")
                .unwrap_or_else(|e| panic!("yardstick publish {seq} on {subject}: {e}"));
            ms(t0, clock.us())
        })
        .collect()
}

/// The broker the yardstick publishes to: the world's own when the world is
/// Strict (what every run before the tier knob measured), else a Strict broker
/// opened for it in the world's scratch directory, served on its own socket
/// and dropped with this. One fsync either way, on the same disk.
struct Ruler {
    own: Option<(Broker, BrokerHandle)>,
    sock: String,
}

impl Ruler {
    fn for_world(w: &World) -> Ruler {
        if w.durability == Durability::Strict {
            return Ruler {
                own: None,
                sock: w.broker_sock.clone(),
            };
        }
        let sock = w.tmp.join("y.sock").to_string_lossy().into_owned();
        let broker = Broker::open_guarded_with(
            w.tmp.join("y.log"),
            harness::SECRET.to_vec(),
            Durability::Strict,
        )
        .expect("the yardstick's Strict broker");
        let handle = broker.serve(&sock).expect("serve the yardstick's broker");
        Ruler {
            own: Some((broker, handle)),
            sock,
        }
    }

    /// A client holding the whole fleet's cap, as [`World::god`] is.
    fn client(&self, w: &World) -> astream_broker::Client {
        if self.own.is_none() {
            return w.god();
        }
        let mut c = harness::broker_client(&self.sock);
        let cap = astream_cap::mint(harness::SECRET, &format!("/f/{FLEET}/>"))
            .expect("mint the yardstick's cap");
        c.attach(&cap.filter, &cap.tag)
            .expect("attach the yardstick's cap");
        c
    }
}

impl Drop for Ruler {
    fn drop(&mut self) {
        if let Some((_, handle)) = &mut self.own {
            handle.shutdown();
        }
    }
}

/// The astream tier a [`Tier`] names.
fn durability(tier: Tier) -> Durability {
    match tier {
        Tier::Strict => Durability::Strict,
        Tier::Relaxed => Durability::Relaxed,
    }
}

/// The yardstick's p50 over both blocks.
fn yardstick(before: &[f64], after: &[f64]) -> Option<f64> {
    let pooled: Vec<f64> = before.iter().chain(after).copied().collect();
    stats::summarize(&pooled).map(|s| s.p50)
}

/// `v / unit`, rounded, for a ratio to the yardstick.
fn per_unit(v: Option<f64>, unit: Option<f64>) -> Option<f64> {
    let (v, unit) = (v?, unit?);
    (unit > 0.0).then(|| stats::round3(v / unit))
}

/// The yardstick's table rows.
fn yardstick_rows(before: &[f64], after: &[f64]) -> Vec<String> {
    vec![
        "| yardstick: one bare Strict publish, client -> broker (ms) | n | mean | p50 | p95 | p99 |"
            .to_string(),
        "|---|---:|---:|---:|---:|---:|".to_string(),
        stats::row("before the phases", before),
        stats::row("after the phases", after),
    ]
}

fn fmt_x(v: Option<f64>) -> String {
    v.map_or("–".to_string(), |v| format!("{v:.2}"))
}

// ---------------------------------------------------------------------------
// 1. messaging
// ---------------------------------------------------------------------------

/// One measured round trip.
struct LatSample {
    text: String,
    t_call: u64,
    t_ret: u64,
    t_seen: u64,
}

/// How finely [`latency`] watches for the previous message's commit to land:
/// an atomic load of the in-process broker's head, so it costs the stack
/// nothing.
const IDLE_POLL: Duration = Duration::from_micros(100);

/// What the latency phase measured.
struct LatRaw {
    /// The measured round trips.
    samples: Vec<LatSample>,
    /// Every round trip's text, warm-up included: each one's idle wait is
    /// checked against the log after the run.
    texts: Vec<String>,
    /// The newest row id it listed.
    since: u64,
}

/// The latency phase: `warmup + samples` round trips, one at a time.
///
/// ONE MESSAGE AT A TIME MEANS ON AN IDLE BRIDGE. The bridge delivers a record
/// and commits its inbox cursor past it at the top of its next round, once no
/// inbox record is queued behind it (`Bridge::round`; on an idle bridge, right
/// after the delivery) — one more Strict append, after the row is already
/// visible. A next post issued
/// the moment the row shows up queues behind that commit, and its sample would
/// carry the tail of the message before it (measured: about one fsync). So
/// each round trip waits for the head to pass the record after the message's
/// own before the next post. That record being the commit is an ASSUMPTION
/// about today's bridge (the census shows the pair), and one no in-run read can
/// check without asking the broker for it — so [`messaging`] checks it for
/// every round against the log after the run ([`census::next_is_its_commit`]),
/// and a bridge that writes anything between a delivery and its commit fails
/// the run by name rather than leaving a fsync of tail in every sample.
/// Back-to-back traffic is what [`burst`] measures.
fn latency(w: &World, broker: &Broker, spec: &Spec, clock: Clock, a: &str, b: &str) -> LatRaw {
    let mut ctl = w.ctl();
    let mut since = newest_id(&mut ctl, b);
    let mut out = Vec::with_capacity(spec.samples);
    let mut texts = Vec::with_capacity(spec.warmup + spec.samples);
    for i in 0..spec.warmup + spec.samples {
        let text = format!("lat-{i}");
        let (t_call, t_ret) = post(&mut ctl, clock, a, b, &text);
        let (woke_at, t_seen) =
            await_note(&mut ctl, clock, b, since).unwrap_or_else(|e| panic!("round trip {i}: {e}"));
        // WHAT WOKE IT IS THIS POST, and nothing else arrived: checked, not
        // assumed, so a stray row can never be timed as the message.
        let rows = list_since(&mut ctl, b, since);
        assert!(
            rows.len() == 1 && rows[0].text == text && rows[0].id == woke_at,
            "round trip {i}: woke at row {woke_at}, listed {rows:?}"
        );
        let off = rows[0].off;
        aterm_spec::wait::within(DEADLINE, IDLE_POLL, || {
            (broker.visible_head() > off + 1).then_some(())
        })
        .unwrap_or_else(|| {
            panic!("round trip {i}: nothing landed after offset {off} within {DEADLINE:?}")
        });
        since = woke_at;
        texts.push(text.clone());
        if i >= spec.warmup {
            out.push(LatSample {
                text,
                t_call,
                t_ret,
                t_seen,
            });
        }
    }
    LatRaw {
        samples: out,
        texts,
        since,
    }
}

/// What the burst phase measured.
struct BurstRaw {
    senders: usize,
    t0: u64,
    /// Each post's reply stamp, by message index.
    rets: Vec<u64>,
    /// Each message's row, by index ([`tally`]: each seen exactly once).
    seen: Vec<Seen>,
    /// The broker's FLUSHES over the burst: writer batches that appended a
    /// record, from just before the first post to the record past the last
    /// message (the burst's commit) — on the Strict log one `fsync` each, the
    /// cost pipelining folds ([`burst`]).
    flushes: u64,
}

/// The burst phase, reading `b`'s inbox on from row `since` (the latency
/// phase's last).
fn burst(
    w: &World,
    broker: &Broker,
    spec: &Spec,
    clock: Clock,
    a: &str,
    b: &str,
    since: u64,
) -> BurstRaw {
    let k = spec.burst_senders();
    let senders: Vec<String> = (0..k).map(|_| spawn(w)).collect();
    wait_live(
        w,
        &senders,
        0,
        "every burst sender's presence row to say live",
    );
    let mut ctl = w.ctl();
    // ONE WARM-UP POST PER SENDER, consumed before the clock starts: a
    // session's first post creates its per-session state on the bridge.
    for (i, s) in senders.iter().enumerate() {
        post(&mut ctl, clock, s, b, &format!("warm-{i}"));
    }
    let since = consume(&mut ctl, clock, b, since, "warm-", k, None)
        .unwrap_or_else(|e| panic!("burst warm-up: {e}"))
        .since;

    let n = spec.burst;
    let credit = Credit::new(BURST_WINDOW);
    // Read under the log lock (`Broker::stats`), so each count is between two
    // whole batches; outside the timed window at both ends.
    let flushed = || {
        let stats = broker.stats();
        stats.writer_batches - stats.empty_batches
    };
    let flushed_before = flushed();
    let (t0, rets, consumed) = std::thread::scope(|scope| {
        let consumer = scope.spawn(|| {
            let _close = CloseOnDrop(&credit);
            let mut reader = w.ctl();
            consume(&mut reader, clock, b, since, "burst-", n, Some(&credit))
        });
        let t0 = clock.us();
        let mut rets = Vec::with_capacity(n);
        for j in 0..n {
            // A reader that stopped early has failed; its answer says why.
            if !credit.acquire(j) {
                break;
            }
            rets.push(post(&mut ctl, clock, &senders[j % k], b, &format!("burst-{j}")).1);
        }
        let consumed = consumer
            .join()
            .unwrap_or_else(|e| std::panic::resume_unwind(e));
        (t0, rets, consumed)
    });
    let consumed = consumed.unwrap_or_else(|e| panic!("burst: {e}\n({})", refusals(w, &senders)));
    let seen = tally(&consumed.seen, n)
        .unwrap_or_else(|e| panic!("burst: every message is seen exactly once, but {e}"));

    // THE FENCE bounds the burst's window by the commit covering its last
    // message. The bridge commits its inbox cursor at the top of a round once
    // no inbox record is queued (`Bridge::round`; within a burst, also every
    // `INBOX_COMMIT_EVERY` records), so that commit is the bridge's next write
    // after the last delivery, and the fence is posted only once the log holds
    // a record past the burst's last one (the latency phase's idle wait). That
    // is not proof: a round that drains aterm's outbox ahead of its flush (a
    // roster round, `Bridge::roster_backstop`) could still publish the fence
    // first and fold it into the burst's commit, which would put the fence's
    // round trip in the burst's numbers. So [`messaging`] checks against the
    // log after the run that the commit landed before the fence's record
    // ([`census::lands_before`]), and fails the run by name if not. Read on
    // from the burst reader's last row, so a late repeat of a burst message is
    // a stray here.
    let b_last = consumed.seen.iter().map(|s| s.off).max().unwrap_or(0);
    aterm_spec::wait::within(DEADLINE, IDLE_POLL, || {
        (broker.visible_head() > b_last + 1).then_some(())
    })
    .unwrap_or_else(|| {
        panic!("burst: nothing landed after the last message's offset {b_last} within {DEADLINE:?}")
    });
    let flushes = flushed() - flushed_before;
    post(&mut ctl, clock, a, b, "fence-0");
    consume(&mut ctl, clock, b, consumed.since, "fence-", 1, None)
        .unwrap_or_else(|e| panic!("burst fence: {e}"));
    BurstRaw {
        senders: k,
        t0,
        rets,
        seen,
        flushes,
    }
}

/// The messaging world: boot, measure, census.
fn messaging(spec: &Spec, clock: Clock) -> (Value, Vec<String>) {
    eprintln!("fabric bench: booting the messaging world");
    let mut w = World::boot_durable("fbench-msg", durability(spec.durability));
    w.wait_ready();
    let (a, b) = w.two_sessions();
    wait_live(
        &w,
        &[a.clone(), b.clone()],
        0,
        "both sessions' presence rows to say live",
    );
    let ruler = Ruler::for_world(&w);
    let mut cal_seq = 0;
    let cal_before = calibrate(&ruler, &w, clock, &mut cal_seq);
    eprintln!(
        "fabric bench: latency, {} round trips ({} warm-up)",
        spec.samples, spec.warmup
    );
    let ((lat_raw, bur), timeline) = sampled(&w, clock, || {
        let broker = w.broker.as_ref().expect("the world serves its own broker");
        let lat = latency(&w, broker, spec, clock, &a, &b);
        eprintln!(
            "fabric bench: burst, {} posts from {} senders, at most {BURST_WINDOW} in flight",
            spec.burst,
            spec.burst_senders()
        );
        let bur = burst(&w, broker, spec, clock, &a, &b, lat.since);
        (lat, bur)
    });
    let cal_after = calibrate(&ruler, &w, clock, &mut cal_seq);
    let unit = yardstick(&cal_before, &cal_after);
    let recs = stop_and_read(&mut w);
    let lane = format!("/f/{FLEET}/in/{}/{b}/{}/note", w.node, w.node);
    let group = aterm_link::subject::inbox_group(FLEET, &w.node);
    // Every text this run sent is unique, so ONE record per text: a second is
    // a message the bus holds twice, named here rather than letting one record
    // shadow the other in the lookup.
    let mut on_lane: HashMap<&str, &Rec> = HashMap::new();
    for r in recs.iter().filter(|r| r.subject == lane) {
        if let Some(first) = on_lane.insert(r.text.as_str(), r) {
            panic!(
                "two records on {lane} carry `{}`, at offsets {} and {}: the bus holds one \
                 message twice",
                r.text, first.seq, r.seq
            );
        }
    }
    let seq_of = |text: &str| -> u64 {
        on_lane
            .get(text)
            .unwrap_or_else(|| panic!("no record of `{text}` on {lane}"))
            .seq
    };
    // THE IDLE-BRIDGE WAIT HELD: every round trip's next record was its commit.
    for text in &lat_raw.texts {
        census::next_is_its_commit(&recs, &group, seq_of(text)).unwrap_or_else(|e| {
            panic!(
                "`{text}`: the latency phase's idle wait passed on the record after the \
                 delivery, and {e} — so the next sample may carry this commit's tail. Make \
                 `latency` wait for the commit itself."
            )
        });
    }
    let lat = lat_raw.samples;

    // --- latency
    let mut post_call = Vec::new();
    let mut deliver = Vec::new();
    let mut end_to_end = Vec::new();
    let mut to_landed = Vec::new();
    let mut landed_to_visible = Vec::new();
    let mut visible_to_committed = Vec::new();
    for s in &lat {
        let seq = seq_of(&s.text);
        let landed = timeline.landed(seq);
        let committed =
            census::covering_commit(&recs, &group, seq, seq).and_then(|r| timeline.landed(r.seq));
        post_call.push(ms(s.t_call, s.t_ret));
        deliver.push(ms(s.t_ret, s.t_seen));
        end_to_end.push(ms(s.t_call, s.t_seen));
        to_landed.extend(ms_opt(Some(s.t_ret), landed));
        landed_to_visible.extend(ms_opt(landed, Some(s.t_seen)));
        visible_to_committed.extend(ms_opt(Some(s.t_seen), committed));
    }
    let lat_lo = seq_of(&lat.first().expect("at least one sample").text);
    let lat_last = seq_of(&lat.last().expect("at least one sample").text);
    let lat_hi =
        census::covering_commit(&recs, &group, lat_last, lat_last).map_or(lat_last, |r| r.seq);
    let lat_census = census::census(census::window(&recs, lat_lo, lat_hi));

    // --- burst
    let n = bur.rets.len();
    let mut b_deliver = Vec::new();
    let mut b_to_landed = Vec::new();
    let mut b_landed_to_visible = Vec::new();
    let mut seqs = Vec::with_capacity(n);
    let mut landed_all = Vec::with_capacity(n);
    for (j, &t_ret) in bur.rets.iter().enumerate() {
        let seen = bur.seen[j];
        let seq = seq_of(&format!("burst-{j}"));
        assert_eq!(
            seq, seen.off,
            "burst-{j}: the row's off= is the record's offset"
        );
        seqs.push(seq);
        let landed = timeline.landed(seq);
        landed_all.push(landed);
        b_deliver.push(ms(t_ret, seen.t));
        b_to_landed.extend(ms_opt(Some(t_ret), landed));
        b_landed_to_visible.extend(ms_opt(landed, Some(seen.t)));
    }
    let t_sent = bur.rets.iter().copied().max().unwrap_or(bur.t0);
    let t_all_visible = bur.seen.iter().map(|s| s.t).max().unwrap_or(bur.t0);
    let t_all_landed = landed_all
        .iter()
        .copied()
        .collect::<Option<Vec<u64>>>()
        .and_then(|v| v.into_iter().max());
    let b_lo = seqs.iter().copied().min().unwrap_or(0);
    let b_last = seqs.iter().copied().max().unwrap_or(0);
    // THE FENCE HELD: the burst's commit landed before the fence's record, so
    // the window it bounds holds no part of the fence's round trip.
    census::lands_before(&recs, &group, b_last, seq_of("fence-0")).unwrap_or_else(|e| {
        panic!(
            "the burst's window ends at the commit covering its last message, and {e} — so the \
             burst's numbers would carry the fence's round trip. Make `burst` wait for that \
             commit itself before it posts the fence."
        )
    });
    let cover = census::covering_commit(&recs, &group, b_last, b_last);
    let t_covered = cover.and_then(|r| timeline.landed(r.seq));
    let b_hi = cover.map_or(b_last, |r| r.seq);
    let b_census = census::census(census::window(&recs, b_lo, b_hi));
    let after = |t: Option<u64>| t.map(|t| stats::round3(ms(bur.t0, t)));
    let rate = |t: Option<u64>| t.map(|t| stats::round3(n as f64 / (ms(bur.t0, t) / 1000.0)));
    let p50 = |v: &[f64]| stats::summarize(v).map(|s| s.p50);
    let deliver_x = per_unit(p50(&deliver), unit);
    let burst_ms_per_message = after(Some(t_all_visible)).map(|t| t / n as f64);
    let burst_x = per_unit(burst_ms_per_message, unit);

    let json = json!({
        "node": w.node.clone(),
        "head_changes_sampled": timeline.len(),
        "yardstick": {
            "what": "one bare Strict publish round trip, a client in the bench process to the world's broker, serial",
            "before_ms": stats::to_json(&cal_before),
            "after_ms": stats::to_json(&cal_after),
            "p50_ms": unit.map(stats::round3),
        },
        "normalized": {
            "what": "the measure divided by the yardstick's p50: how many bare Strict publishes it costs. Takes the disk's speed out, not CPU contention: compare runs at similar load",
            "deliver_p50_in_publishes": deliver_x,
            "burst_ms_per_message": burst_ms_per_message.map(stats::round3),
            "burst_publishes_per_message": burst_x,
        },
        "latency": {
            "samples": lat.len(),
            "warmup": spec.warmup,
            "post_call_ms": stats::to_json(&post_call),
            "deliver_ms": stats::to_json(&deliver),
            "end_to_end_ms": stats::to_json(&end_to_end),
            "post_to_durable_ms": stats::to_json(&to_landed),
            "durable_to_visible_ms": stats::to_json(&landed_to_visible),
            "visible_to_commit_durable_ms": stats::to_json(&visible_to_committed),
            "census": census::to_json(&lat_census, lat.len(), (lat_lo, lat_hi)),
        },
        "burst": {
            "messages": n,
            "senders": bur.senders,
            "in_flight_max": BURST_WINDOW,
            "all_accepted_ms": after(Some(t_sent)),
            "all_durable_ms": after(t_all_landed),
            "all_visible_ms": after(Some(t_all_visible)),
            "commit_covered_ms": after(t_covered),
            "durable_rate_per_s": rate(t_all_landed),
            "visible_rate_per_s": rate(Some(t_all_visible)),
            "broker_flushes": bur.flushes,
            "broker_flushes_per_message": (n > 0).then(|| stats::round3(bur.flushes as f64 / n as f64)),
            "deliver_ms": stats::to_json(&b_deliver),
            "post_to_durable_ms": stats::to_json(&b_to_landed),
            "durable_to_visible_ms": stats::to_json(&b_landed_to_visible),
            "census": census::to_json(&b_census, n, (b_lo, b_hi)),
        },
    });

    let fmt_ms = |v: Option<f64>| v.map_or("not seen".to_string(), |v| format!("{v:.1}"));
    let fmt_rate = |v: Option<f64>| v.map_or("–".to_string(), |v| format!("{v:.1}"));
    let mut md = vec![
        format!(
            "## Messaging: post -> deliver on one node (node `{}`)\n",
            w.node
        ),
        format!(
            "{} round trips after {} warm-up, one at a time on an idle bridge: session A posts \
             `kind=note` (no `--wait`) to session B, and B's `await inbox` is answered. Durable = \
             the record's offset first seen under the broker's visible head; the inbox commit \
             follows the delivery, after the row is visible.\n",
            lat.len(),
            spec.warmup
        ),
        "| measure (ms) | n | mean | p50 | p95 | p99 |".to_string(),
        "|---|---:|---:|---:|---:|---:|".to_string(),
        stats::row("post call (request -> `OK`)", &post_call),
        stats::row("**post -> visible in B's inbox**", &deliver),
        stats::row("post call -> visible (end to end)", &end_to_end),
        stats::row("post -> durable on the broker", &to_landed),
        stats::row("durable -> visible", &landed_to_visible),
        stats::row("visible -> the inbox commit durable", &visible_to_committed),
        String::new(),
        format!(
            "Records the broker wrote per delivered message (offsets {lat_lo}..={lat_hi}, through \
             the commit covering the last one):\n"
        ),
    ];
    md.extend(census::rows(&lat_census, lat.len()));
    md.extend([
        String::new(),
        format!(
            "## Burst: {n} posts back to back to one recipient ({} sender sessions)\n",
            bur.senders
        ),
        format!(
            "Back to back within the recipient's allowance: at most {BURST_WINDOW} posts in \
             flight (posted, not yet listed by the reader), because aterm refuses a 65th unlisted \
             row from one peer for good. So \"every post accepted\" trails the reader by at most \
             that many messages, and a message's own post -> visible is mostly its wait in that \
             queue.\n"
        ),
        "| milestone | ms after the first post | messages/s |".to_string(),
        "|---|---:|---:|".to_string(),
        format!(
            "| every post accepted (`OK`) | {} | – |",
            fmt_ms(after(Some(t_sent)))
        ),
        format!(
            "| every record durable on the broker | {} | {} |",
            fmt_ms(after(t_all_landed)),
            fmt_rate(rate(t_all_landed))
        ),
        format!(
            "| **every message visible in the recipient's inbox** | {} | {} |",
            fmt_ms(after(Some(t_all_visible))),
            fmt_rate(rate(Some(t_all_visible)))
        ),
        format!(
            "| the recipient bridge's commit covers them | {} | – |",
            fmt_ms(after(t_covered))
        ),
        String::new(),
        "| per message (ms) | n | mean | p50 | p95 | p99 |".to_string(),
        "|---|---:|---:|---:|---:|---:|".to_string(),
        stats::row("post -> visible", &b_deliver),
        stats::row("post -> durable", &b_to_landed),
        stats::row("durable -> visible", &b_landed_to_visible),
        String::new(),
        format!(
            "Broker flushes over the burst (writer batches that appended a record, through the \
             record past the last message): {} — {} per message.\n",
            bur.flushes,
            if n == 0 {
                "–".to_string()
            } else {
                format!("{:.3}", bur.flushes as f64 / n as f64)
            }
        ),
        format!("Records per delivered message (offsets {b_lo}..={b_hi}):\n"),
    ]);
    md.extend(census::rows(&b_census, n));
    md.push(String::new());
    md.extend(yardstick_rows(&cal_before, &cal_after));
    md.extend([
        String::new(),
        format!(
            "Normalized to the yardstick's p50 ({} ms): post -> visible p50 = **{} publishes**; \
             the burst = {} ms per message = **{} publishes per message**. The ratio takes the \
             disk's speed out of a comparison, not CPU contention (the yardstick is one thread \
             and one fsync; the fabric is three processes): compare runs taken at similar load \
             averages.\n",
            fmt_x(unit),
            fmt_x(deliver_x),
            fmt_x(burst_ms_per_message),
            fmt_x(burst_x)
        ),
    ]);
    (json, md)
}

// ---------------------------------------------------------------------------
// 2. attach
// ---------------------------------------------------------------------------

/// One bridge restart.
struct Round {
    sessions: usize,
    /// The visible head just before the kill: every record the new bridge
    /// writes is at or past it.
    mark: u64,
    /// The restarted bridge's pid: its node row names it.
    pid: i32,
    t_kill: u64,
    t_pid: u64,
    sids: Vec<String>,
}

/// The bridge's pid from its state dir, if it has written one.
fn read_pid(w: &World) -> Option<i32> {
    std::fs::read_to_string(w.state.join("pid"))
        .ok()?
        .trim()
        .parse()
        .ok()
}

/// Where one bridge's attach landed: its node row, and each session's row in
/// the order the sessions were named.
#[derive(Debug, Clone, PartialEq, Eq)]
struct AttachRows {
    node: u64,
    sessions: Vec<u64>,
}

impl AttachRows {
    /// The last of them, the node row included: when the attach is done. (A
    /// bridge that wrote its node row after a session's would otherwise be
    /// "done" before its node said live.)
    fn last(&self) -> u64 {
        self.sessions
            .iter()
            .copied()
            .chain([self.node])
            .max()
            .unwrap_or(self.node)
    }

    /// The last session row, for the per-session part.
    fn last_session(&self) -> Option<u64> {
        self.sessions.iter().copied().max()
    }
}

/// Where the bridge `pid` published its attach among `rows` — `(offset,
/// subject, body head line)`, all at or past the round's mark: its node row
/// (`state=live … pid=<pid>`) and, under that row's `inc=`, each of
/// `session_subjects`' first live row. `None` until every one is there. Pure
/// (unit-tested).
///
/// KEYED ON THE NEW BRIDGE'S OWN MARKS, not on "the first live row past the
/// mark": a row the killed bridge wrote just before it died (a roster round's
/// republish) lands past the mark too, and it is not this attach. The node row
/// names the bridge's pid, and every session row the same bridge writes carries
/// the node row's incarnation (`bring_presence_up`, `publish_session_presence`).
/// No order is assumed between the rows: a bridge that publishes a session's
/// row before its node's is read the same way.
fn attach_rows(
    rows: &[(u64, &str, &str)],
    node_subject: &str,
    session_subjects: &[String],
    pid: i32,
) -> Option<AttachRows> {
    let pid = pid.to_string();
    let live = |head: &str| census::token(head, "state") == Some("live");
    let (node, inc) = rows.iter().find_map(|&(off, subject, head)| {
        (subject == node_subject && live(head) && census::token(head, "pid") == Some(&*pid))
            .then(|| census::token(head, "inc").map(|inc| (off, inc)))
            .flatten()
    })?;
    let sessions = session_subjects
        .iter()
        .map(|want| {
            rows.iter().find_map(|&(off, subject, head)| {
                (subject == want && live(head) && census::token(head, "inc") == Some(inc))
                    .then_some(off)
            })
        })
        .collect::<Option<Vec<u64>>>()?;
    Some(AttachRows { node, sessions })
}

/// THE ATTACH'S STOP SIGNAL, with nothing asked of the broker inside the timed
/// window: read `watch` — a subscription to the node's presence rows from the
/// round's mark, opened BEFORE the kill — until the bridge `pid` has published
/// its whole attach ([`attach_rows`]).
///
/// WHY A SUBSCRIPTION AND NOT A POLL. The stop TIME is read off the log after
/// the run; this only has to know the attach is over before the next round. A
/// `last_all` poll would answer that by scanning the subject index under the
/// broker's log lock every 10 ms — the lock the writer takes for the very
/// Strict appends being timed. A subscription costs the broker what each of the
/// bridge's own subscribers costs it: a short lock per commit to copy the new
/// records out. Its setup (the `Subscribe` request and its catch-up) happens
/// before the clock starts.
fn await_attach(
    watch: &mut Subscription,
    node_subject: &str,
    session_subjects: &[String],
    pid: i32,
) {
    let deadline = Instant::now() + DEADLINE;
    let mut received: Vec<(u64, String, String)> = Vec::new();
    loop {
        let rows: Vec<(u64, &str, &str)> = received
            .iter()
            .map(|(off, subject, head)| (*off, subject.as_str(), head.as_str()))
            .collect();
        if attach_rows(&rows, node_subject, session_subjects, pid).is_some() {
            return;
        }
        // Marks keep arriving on an idle subscription, so the read timeout
        // alone would never fire: the hang detector is this deadline.
        assert!(
            Instant::now() < deadline,
            "the restarted bridge (pid {pid}) did not publish its attach within {DEADLINE:?}; \
             the presence rows that arrived: {received:?}"
        );
        match watch.recv() {
            Ok(Some((off, subject, body))) => {
                received.push((off, subject, census::head_line(&body)));
            }
            Ok(None) => panic!("the broker closed the presence subscription"),
            Err(e) => panic!("the presence subscription: {e}; received {received:?}"),
        }
    }
}

/// What one population's restarts wrote, beside the times.
#[derive(Default)]
struct AttachRecords {
    /// Each round's census of the attach, the killed bridge's fired will left
    /// out.
    censuses: Vec<census::Census>,
    /// Each round's fired wills: records the KILL caused (the broker publishing
    /// the dead bridge's `state=gone` node row), landing inside the window.
    fired: Vec<usize>,
}

impl AttachRecords {
    fn mean(values: impl Iterator<Item = usize>, rounds: usize) -> f64 {
        values.sum::<usize>() as f64 / rounds.max(1) as f64
    }

    /// The mean count of each kind of record per attach.
    fn by_kind(&self) -> BTreeMap<String, f64> {
        let mut kinds: BTreeMap<String, usize> = BTreeMap::new();
        for c in &self.censuses {
            for (class, n) in &c.by_class {
                *kinds.entry(class.clone()).or_default() += n;
            }
        }
        kinds
            .into_iter()
            .map(|(class, n)| (class, n as f64 / self.censuses.len().max(1) as f64))
            .collect()
    }
}

/// The attach world: grow the population, restart the bridge `rounds` times
/// at each size, census.
fn attach(spec: &Spec, clock: Clock) -> (Value, Vec<String>) {
    eprintln!("fabric bench: booting the attach world");
    let mut w = World::boot_durable("fbench-att", durability(spec.durability));
    w.wait_ready();
    let first = until("aterm's boot session", || {
        w.sessions().first().map(|(_, sid, _)| sid.clone())
    });
    let node_subject = presence_subject(&w, "node");
    let presence_filter = format!("/f/{FLEET}/pub/{}/*/presence", w.node);
    let ruler = Ruler::for_world(&w);
    let mut cal_seq = 0;
    let cal_before = calibrate(&ruler, &w, clock, &mut cal_seq);
    let (rounds, timeline) = sampled(&w, clock, || {
        let broker = w.broker.as_ref().expect("the world serves its own broker");
        let mut sids = vec![first];
        let mut pid = w.bridge_pid();
        let mut up_since = Instant::now();
        let mut rounds = Vec::new();
        for &size in &spec.attach {
            while sids.len() < size {
                sids.push(spawn(&w));
            }
            wait_live(&w, &sids, 0, "every session's presence row to say live");
            let session_subjects: Vec<String> =
                sids.iter().map(|sid| presence_subject(&w, sid)).collect();
            eprintln!(
                "fabric bench: attach with {size} session(s), {} restart(s)",
                spec.rounds
            );
            for _ in 0..spec.rounds {
                if let Some(rest) = SUPERVISOR_HEALTHY.checked_sub(up_since.elapsed()) {
                    std::thread::sleep(rest);
                }
                let mark = broker.visible_head();
                let mut watch = w
                    .god()
                    .subscribe(mark, &presence_filter)
                    .unwrap_or_else(|e| panic!("subscribe to {presence_filter} from {mark}: {e}"));
                watch
                    .set_read_timeout(Some(DEADLINE))
                    .expect("bound the presence subscription");
                let t_kill = clock.us();
                harness::kill(pid, 9);
                let new_pid = aterm_spec::wait::within(DEADLINE, PID_POLL, || {
                    read_pid(&w).filter(|p| *p != pid)
                })
                .unwrap_or_else(|| panic!("the bridge did not come back within {DEADLINE:?}"));
                let t_pid = clock.us();
                pid = new_pid;
                up_since = Instant::now();
                await_attach(&mut watch, &node_subject, &session_subjects, new_pid);
                drop(watch);
                rounds.push(Round {
                    sessions: size,
                    mark,
                    pid: new_pid,
                    t_kill,
                    t_pid,
                    sids: sids.clone(),
                });
            }
        }
        rounds
    });
    let cal_after = calibrate(&ruler, &w, clock, &mut cal_seq);
    let unit = yardstick(&cal_before, &cal_after);
    let recs = stop_and_read(&mut w);
    let fired = census::will_firings(&recs);

    // Each round, read off the log: where the new bridge's rows landed.
    let mut by_size: BTreeMap<usize, Vec<Value>> = BTreeMap::new();
    let mut series: BTreeMap<usize, [Vec<f64>; 5]> = BTreeMap::new();
    let mut records: BTreeMap<usize, AttachRecords> = BTreeMap::new();
    for r in &rounds {
        let rows: Vec<(u64, &str, &str)> = recs
            .iter()
            .filter(|rec| rec.seq >= r.mark)
            .map(|rec| (rec.seq, rec.subject.as_str(), rec.head.as_str()))
            .collect();
        let session_subjects: Vec<String> =
            r.sids.iter().map(|sid| presence_subject(&w, sid)).collect();
        let found =
            attach_rows(&rows, &node_subject, &session_subjects, r.pid).unwrap_or_else(|| {
                panic!(
                    "the log holds no complete attach by pid {} past offset {}",
                    r.pid, r.mark
                )
            });
        let last = found.last();
        let t_node = timeline.landed(found.node);
        let t_all = timeline.landed(last);
        let t_last_session = found.last_session().and_then(|s| timeline.landed(s));
        let in_window = || census::window(&recs, r.mark, last);
        let c = census::census(in_window().filter(|rec| !fired.contains(&rec.seq)));
        let fired_here = in_window().filter(|rec| fired.contains(&rec.seq)).count();
        let s = series.entry(r.sessions).or_default();
        s[0].extend(ms_opt(Some(r.t_pid), t_all));
        s[1].extend(ms_opt(Some(r.t_pid), t_node));
        s[2].extend(ms_opt(t_node, t_last_session));
        s[3].push(ms(r.t_kill, r.t_pid));
        s[4].extend(ms_opt(Some(r.t_kill), t_all));
        by_size.entry(r.sessions).or_default().push(json!({
            "mark": r.mark,
            "pid": r.pid,
            "node_offset": found.node,
            "last_offset": last,
            "records": c.total,
            "fired_wills": fired_here,
            "kill_to_pid_ms": stats::round3(ms(r.t_kill, r.t_pid)),
            "pid_to_all_ms": ms_opt(Some(r.t_pid), t_all).map(stats::round3),
        }));
        let rec = records.entry(r.sessions).or_default();
        rec.censuses.push(c);
        rec.fired.push(fired_here);
    }
    let mut sizes = Vec::new();
    let mut md = vec![
        format!(
            "## Attach: a bridge starting with N sessions (node `{}`)\n",
            w.node
        ),
        format!(
            "The bridge is SIGKILLed and aterm's supervisor relaunches it; the clock starts when \
             the new bridge writes its pid (its aterm connection is up) and stops when its node \
             row and every session's presence row — the rows under its own pid and incarnation, \
             whichever lands last — are durable again. Inside that window the bench asks the \
             broker nothing: it holds one subscription to the node's presence rows, opened before \
             the kill, and reads the times off the log afterwards. \"node row -> last session \
             row\" is negative for a bridge that publishes a session's row before its node's. {} \
             restart(s) per size.\n",
            spec.rounds
        ),
        "| sessions | measure (ms) | n | mean | p50 | p95 | p99 |".to_string(),
        "|---:|---|---:|---:|---:|---:|---:|".to_string(),
    ];
    let mut normalized = vec![
        format!(
            "Normalized to the yardstick's p50 ({} ms; the disk's speed, not CPU contention); per \
             session = (node row -> last session row) p50 / sessions:\n",
            fmt_x(unit)
        ),
        "| sessions | attach p50 (ms) | in publishes | per session (ms) | in publishes |"
            .to_string(),
        "|---:|---:|---:|---:|---:|".to_string(),
    ];
    let names = [
        "**pid -> every row durable (attach)**",
        "pid -> node row durable",
        "node row -> last session row",
        "kill -> new pid (supervisor relaunch)",
        "kill -> every row durable",
    ];
    for (size, s) in &series {
        for (name, values) in names.iter().zip(s.iter()) {
            md.push(format!("| {size} |{}", &stats::row(name, values)[1..]));
        }
        let rec = &records[size];
        let recs_per: Vec<f64> = rec.censuses.iter().map(|c| c.total as f64).collect();
        let commits_per: Vec<f64> = rec.censuses.iter().map(|c| c.commits as f64).collect();
        let fired_per: Vec<f64> = rec.fired.iter().map(|n| *n as f64).collect();
        let by_kind: aterm_json::Map = rec
            .by_kind()
            .into_iter()
            .map(|(class, mean)| (class, json!(stats::round3(mean))))
            .collect();
        let attach_p50 = stats::summarize(&s[0]).map(|x| x.p50);
        let per_session = stats::summarize(&s[2]).map(|x| x.p50 / *size as f64);
        normalized.push(format!(
            "| {size} | {} | {} | {} | {} |",
            fmt_x(attach_p50),
            fmt_x(per_unit(attach_p50, unit)),
            fmt_x(per_session),
            fmt_x(per_unit(per_session, unit)),
        ));
        sizes.push(json!({
            "sessions": size,
            "rounds": s[3].len(),
            "attach_p50_in_publishes": per_unit(attach_p50, unit),
            "per_session_ms": per_session.map(stats::round3),
            "per_session_in_publishes": per_unit(per_session, unit),
            "attach_ms": stats::to_json(&s[0]),
            "pid_to_node_ms": stats::to_json(&s[1]),
            "node_to_last_session_ms": stats::to_json(&s[2]),
            "kill_to_pid_ms": stats::to_json(&s[3]),
            "kill_to_all_ms": stats::to_json(&s[4]),
            "records_per_attach": stats::to_json(&recs_per),
            "commit_records_per_attach": stats::to_json(&commits_per),
            "records_by_kind_mean": Value::Object(by_kind),
            "fired_wills_not_counted": stats::to_json(&fired_per),
            "rounds_detail": Value::Array(by_size.remove(size).unwrap_or_default()),
        }));
    }
    md.push(String::new());
    md.push(
        "Records the broker wrote per attach, by kind (mean per restart), from the mark to the \
         last row. The killed bridge's will — the `state=gone` node row the broker publishes for \
         it when the kill drops its connection — lands in that window too; it is the kill's \
         record, not the attach's, so it has its own column and is not in the total.\n"
            .to_string(),
    );
    md.push(
        "| sessions | records per attach (mean) | of which commits | by kind (mean) | the killed \
         bridge's fired will (not counted) |"
            .to_string(),
    );
    md.push("|---:|---:|---:|---|---:|".to_string());
    for (size, rec) in &records {
        let rounds = rec.censuses.len();
        let kinds: Vec<String> = rec
            .by_kind()
            .iter()
            .map(|(class, mean)| format!("`{class}` {mean:.1}"))
            .collect();
        md.push(format!(
            "| {size} | {:.1} | {:.1} | {} | {:.1} |",
            AttachRecords::mean(rec.censuses.iter().map(|c| c.total), rounds),
            AttachRecords::mean(rec.censuses.iter().map(|c| c.commits), rounds),
            kinds.join(" · "),
            AttachRecords::mean(rec.fired.iter().copied(), rounds),
        ));
    }
    md.push(String::new());
    md.extend(yardstick_rows(&cal_before, &cal_after));
    md.push(String::new());
    md.extend(normalized);
    md.push(String::new());
    let json = json!({
        "node": w.node.clone(),
        "head_changes_sampled": timeline.len(),
        "yardstick": {
            "before_ms": stats::to_json(&cal_before),
            "after_ms": stats::to_json(&cal_after),
            "p50_ms": unit.map(stats::round3),
        },
        "sizes": Value::Array(sizes),
    });
    (json, md)
}

// ---------------------------------------------------------------------------
// the run
// ---------------------------------------------------------------------------

/// What the run's broker promised, for the report's head and its JSON: under
/// Relaxed, every "durable" in the tables is an ack into the page cache.
fn broker_note(tier: Tier) -> &'static str {
    match tier {
        Tier::Strict => {
            "astream Broker::open_guarded (Durability::Strict: every ack after an fsync)"
        }
        Tier::Relaxed => {
            "astream Broker::open_guarded_with(Durability::Relaxed): every ack after the \
             batch's page-cache write, NO fsync. The ceiling arm, not what aterm runs: every \
             \"durable\" below means acked into the page cache. The yardstick is still one Strict \
             publish, on a Strict broker of its own."
        }
    }
}

/// Measure everything `spec` names.
#[must_use]
pub fn measure(spec: &Spec) -> Report {
    // ONE BINARY THROUGHOUT. Read before the first launch and after the last
    // (the supervisor re-launches the bridge by path on every attach round);
    // the lane holds both to what its own build wrote.
    let binary_at_start = binary_identity();
    let clock = Clock::start();
    let started = Instant::now();
    let (messaging, messaging_md) = messaging(spec, clock);
    let (pingpong, pingpong_md) = pingpong::measure(spec, clock);
    let (attach, attach_md) = attach(spec, clock);
    let mut md = vec![
        format!("**Broker:** {}", broker_note(spec.durability)),
        String::new(),
    ];
    md.extend(messaging_md);
    md.extend(pingpong_md);
    md.extend(attach_md);
    let binary_at_end = binary_identity();
    // THE ARM IS THE BINARY'S. `harness::ATERM` is the window and, through the
    // seam, the bridge; cargo built it, and this test (which holds the broker),
    // under one profile, whose directory it sits in.
    let arm = Profile::of_binary(Path::new(harness::ATERM));
    let json = json!({
        "schema": "aterm-fabric-bench-v1",
        "spec": spec.words(),
        "profile": arm.map_or("unknown", Profile::name),
        "build": arm.map_or("a binary in neither arm's build directory", Profile::describe),
        "aterm_binary": harness::ATERM,
        "aterm_binary_identity": { "start": binary_at_start, "end": binary_at_end },
        "broker": broker_note(spec.durability),
        "clock": "one monotonic origin in the bench process; microsecond stamps",
        "elapsed_s": stats::round3(started.elapsed().as_secs_f64()),
        "messaging": messaging,
        "pingpong": pingpong,
        "attach": attach,
    });
    Report {
        json,
        markdown: md.join("\n"),
    }
}

/// [`profile::identity`] of the harness's `aterm`, or why it could not be read,
/// which the lane takes as no identity.
fn binary_identity() -> String {
    profile::identity(Path::new(harness::ATERM)).unwrap_or_else(|why| format!("unreadable: {why}"))
}

#[cfg(test)]
mod tests {
    #[test]
    fn an_inbox_listing_yields_its_message_rows() {
        use super::*;

        let reply = Reply::Lines {
            header: "OK 3 hold=0 holder=- seen=0 bus_head=9 oldest_on_bus=@4 dropped=0 pending=0"
                .to_string(),
            rows: vec![
                "msg 12 off=7 t=1 from=s-a@n-1 kind=note trust=agent len=6 text=burst-3".into(),
                "msg 13 off=9 t=2 from=s-a@n-1 kind=note trust=agent len=8 text=a%20b".into(),
                "post 4 to=@s-b kind=note off=- len=3".into(),
            ],
        };
        assert_eq!(
            rows_of(&reply),
            vec![
                Row {
                    id: 12,
                    off: 7,
                    text: "burst-3".into()
                },
                Row {
                    id: 13,
                    off: 9,
                    text: "a b".into()
                },
            ]
        );
    }

    #[test]
    fn every_message_is_seen_exactly_once_or_the_tally_says_which_was_not() {
        use super::*;

        fn seen(j: usize, off: u64) -> Seen {
            Seen { j, off, t: 0 }
        }

        assert_eq!(
            tally(&[seen(1, 11), seen(0, 10)], 2),
            Ok(vec![seen(0, 10), seen(1, 11)])
        );
        // THE REPEAT THAT ARRIVES WITH THE LAST MESSAGE: n + 1 rows holding n
        // distinct messages, which a count of distinct indices calls complete.
        assert_eq!(
            tally(&[seen(0, 10), seen(1, 11), seen(1, 11)], 2),
            Err(Miscount::Twice {
                j: 1,
                first: 11,
                again: 11
            })
        );
        // The bus holding one message twice: two offsets.
        assert_eq!(
            tally(&[seen(0, 10), seen(0, 12), seen(1, 11)], 2),
            Err(Miscount::Twice {
                j: 0,
                first: 10,
                again: 12
            })
        );
        assert_eq!(tally(&[seen(0, 10)], 2), Err(Miscount::Never(1)));
        assert_eq!(
            tally(&[seen(0, 10), seen(2, 12)], 2),
            Err(Miscount::Unsent(2))
        );
    }

    #[test]
    fn the_burst_never_has_more_in_flight_than_its_window() {
        use super::*;

        // A window of 3: posts 0, 1 and 2 go with nothing listed; post 3 waits
        // for the reader to list one.
        assert!(may_post(2, 0, 3));
        assert!(!may_post(3, 0, 3));
        assert!(may_post(3, 1, 3));
        let credit = Credit::new(2);
        assert!(credit.acquire(0) && credit.acquire(1));
        std::thread::scope(|s| {
            // The reader lists one, which is what lets post 2 out; the wait is
            // the hang detector's, never a budget.
            s.spawn(|| credit.listed(1));
            assert!(credit.acquire(2));
        });
        // A reader that stopped lets nothing more out, and says so at once.
        let closed = Credit::new(2);
        drop(CloseOnDrop(&closed));
        assert!(!closed.acquire(2));
        assert!(closed.acquire(1), "room it already had is still room");
    }

    #[test]
    fn an_attach_is_the_new_bridges_rows_in_whatever_order_they_land() {
        use super::*;

        let node = "/f/f1/pub/n-1/node/presence";
        let sa = "/f/f1/pub/n-1/s-a/presence".to_string();
        let sb = "/f/f1/pub/n-1/s-b/presence".to_string();
        let both = [sa.clone(), sb.clone()];
        let rows = vec![
            // The killed bridge's will, fired; then a republish it wrote just
            // before it died — past the mark, and not this attach.
            (10, node, "v=1 state=gone inc=4 fabric=disconnected"),
            (11, sa.as_str(), "v=1 inc=4 state=live"),
            (12, node, "v=1 state=live inc=5 pid=77"),
            (13, sb.as_str(), "v=1 inc=5 state=live"),
            (14, sa.as_str(), "v=1 inc=5 state=live"),
        ];
        let found = attach_rows(&rows, node, &both, 77).expect("complete");
        assert_eq!(
            found,
            AttachRows {
                node: 12,
                sessions: vec![14, 13]
            }
        );
        assert_eq!(found.last(), 14);
        assert_eq!(attach_rows(&rows[..4], node, &both, 77), None, "s-a owed");
        assert_eq!(attach_rows(&rows, node, &both, 78), None, "another pid's");
        // A bridge that writes its node row LAST: the attach ends there, and
        // the per-session part is negative.
        let node_last = vec![
            (20, sa.as_str(), "v=1 inc=6 state=live"),
            (21, node, "v=1 state=live inc=6 pid=80"),
        ];
        let found = attach_rows(&node_last, node, std::slice::from_ref(&sa), 80).expect("complete");
        assert_eq!((found.last(), found.last_session()), (21, Some(20)));
    }
}
