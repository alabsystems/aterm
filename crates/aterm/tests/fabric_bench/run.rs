// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The measured phases, each selected scenario over its own world, booted as the fabric suite boots it (`harness::World`: a guarded broker in this process, opened with aterm's own options for the arm's tier — ordered at aterm's horizon by default — and retained by aterm's default rules as `aterm link broker` retains it, a headless `aterm`, and the real `aterm link serve` child it launched, its bus mirror at `--log auto`; [`boot_world`] takes an arm's `retention=off`, `log=off` and tick interval). With `broker_process=background` or `interactive` each world's broker is instead `aterm link broker` in a process of its own, as its launchd job starts it ([`outside`]).

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::Path;
use std::sync::atomic::AtomicBool;
use std::sync::{Condvar, Mutex, PoisonError};
use std::time::{Duration, Instant};

use astream_broker::{Broker, BrokerHandle, Durability, FlushCounts, OpenOptions, Subscription};
use aterm_json::{Value, json};
use aterm_link::ctl::{Ctl, Reply};

use crate::census::{self, Rec};
use crate::clock::{self, Clock, Heads, StopOnDrop, ms};
use crate::harness::{self, DEADLINE, FLEET, World, kv, until};
use crate::profile::{self, Profile};
use crate::spec::{Log, NOTE_PACE, ORDERED_HORIZON_MS, Scenario, Spec, Tier};
use crate::stats;

#[path = "run/pingpong.rs"]
mod pingpong;

#[path = "run/handoff.rs"]
mod handoff;

#[path = "run/ask.rs"]
mod ask;

#[path = "run/cross.rs"]
mod cross;

#[path = "run/backlog.rs"]
mod backlog;

#[path = "run/tallies.rs"]
mod tallies;

#[path = "run/outside.rs"]
mod outside;

#[path = "run/seeded.rs"]
mod seeded;

/// What a run hands back.
pub struct Report {
    pub json: Value,
    pub markdown: String,
}

/// How long the bench lets a bridge run before it kills it again.
const SUPERVISOR_HEALTHY: Duration = Duration::from_millis(5_500);

/// How often the pid file is read while a killed bridge comes back.
const PID_POLL: Duration = Duration::from_millis(1);

fn request(ctl: &mut Ctl, line: &str) -> Reply {
    ctl.request(line).unwrap_or_else(|e| panic!("{line}: {e}"))
}

/// `from` posts a note to `to`: `(stamp before the request, stamp after the reply)`.
fn post(ctl: &mut Ctl, clock: Clock, from: &str, to: &str, text: &str) -> (u64, u64) {
    let line = format!("@{from} post to=@{to} kind=note -- {text}");
    let t_call = clock.us();
    let reply = request(ctl, &line);
    let t_ret = clock.us();
    assert!(reply.ok(), "{line}: {}", reply.header());
    (t_call, t_ret)
}

/// Park on `sid`'s inbox until a note row newer than `since` is there.
///
/// # Errors
///
/// Any answer but a row — `OK timeout` after the hang detector — as the request and its reply.
fn await_note(ctl: &mut Ctl, clock: Clock, sid: &str, since: u64) -> Result<(u64, u64), String> {
    await_kinds(ctl, clock, sid, since, "note")
}

/// [`await_note`] for the rows of `kinds` (`ask`, `ask,task`, …).
///
/// # Errors
///
/// As [`await_note`].
fn await_kinds(
    ctl: &mut Ctl,
    clock: Clock,
    sid: &str,
    since: u64,
    kinds: &str,
) -> Result<(u64, u64), String> {
    let line = format!(
        "@{sid} await inbox since={since} kinds={kinds} timeout={}",
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

/// `sid`'s `inbox` reply for its rows newer than `since` — not `--peek`.
fn listing_since(ctl: &mut Ctl, sid: &str, since: u64) -> Reply {
    let line = format!("@{sid} inbox since={since}");
    let reply = request(ctl, &line);
    assert!(reply.ok(), "{line}: {}", reply.header());
    reply
}

/// List `sid`'s rows newer than `since` — not `--peek`.
fn list_since(ctl: &mut Ctl, sid: &str, since: u64) -> Vec<Row> {
    rows_of(&listing_since(ctl, sid, since))
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

/// Spawn `n` idle bystander sessions ([`Spec::bystanders`]).
fn bystanders(w: &World, n: usize) -> Vec<String> {
    if n > 0 {
        eprintln!("fabric bench: spawning {n} idle bystander session(s)");
    }
    (0..n).map(|_| spawn(w)).collect()
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

/// Wait until the node row and every one of `sids`' presence rows say `state=live` at an offset at or past `from`.
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

/// One row a consumer saw: which message (its index), at which broker offset, stamped when.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Seen {
    j: usize,
    off: u64,
    t: u64,
}

/// What a reader saw.
struct Consumed {
    /// Every row it listed, in arrival order, a repeat included.
    seen: Vec<Seen>,
    /// The newest row id it listed: where the next phase's reading starts.
    since: u64,
}

/// Read `sid`'s inbox like a reader until `want` distinct messages whose text is `<prefix><j>` have arrived.
///
/// # Errors
///
/// A wait that timed out, or a row whose text is not `<prefix><j>` — a stray, or a late repeat of the previous phase's message, which this phase must not time as one of its own.
fn consume(
    ctl: &mut Ctl,
    clock: Clock,
    sid: &str,
    since: u64,
    prefix: &str,
    want: usize,
    credit: Option<&Credit>,
) -> Result<Consumed, String> {
    consume_kinds(ctl, clock, sid, since, (prefix, "note"), want, credit)
}

/// [`consume`] of the rows of `kinds` (`ask`, …), `(prefix, kinds)`.
///
/// # Errors
///
/// As [`consume`].
fn consume_kinds(
    ctl: &mut Ctl,
    clock: Clock,
    sid: &str,
    mut since: u64,
    (prefix, kinds): (&str, &str),
    want: usize,
    credit: Option<&Credit>,
) -> Result<Consumed, String> {
    let mut seen = Vec::with_capacity(want);
    let mut distinct = HashSet::with_capacity(want);
    while distinct.len() < want {
        let (woke_at, t_wake) = await_kinds(ctl, clock, sid, since, kinds)?;
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
    /// Message `j` arrived as two rows.
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

/// Each of `n` messages' row, by index.
///
/// # Errors
///
/// The first [`Miscount`]: a message seen twice, one never seen, or an index past `n`.
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

/// How many burst posts may be in flight — posted, not yet listed by the reader — at once.
const BURST_WINDOW: usize = 56;

/// Aterm's per-peer allowance of unlisted rows (`aterm-gui` `SENDER_QUOTA`, crate-private there), which [`BURST_WINDOW`] stays under.
const RECIPIENT_ALLOWANCE: usize = 64;
const _: () = assert!(BURST_WINDOW < RECIPIENT_ALLOWANCE);

/// Whether post `posted` (0-based) may go out with `listed` messages listed.
fn may_post(posted: usize, listed: usize, window: usize) -> bool {
    posted < listed + window
}

/// The burst's flow control.
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

    /// Wait until post `posted` may go out.
    ///
    /// # Panics
    ///
    /// After [`DEADLINE`] with the reader still running and no room — the hang detector.
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

/// Closes its [`Credit`] when dropped — on the reader's unwind too, so a failed reader never leaves the sender waiting out the hang detector.
struct CloseOnDrop<'a>(&'a Credit);

impl Drop for CloseOnDrop<'_> {
    fn drop(&mut self) {
        self.0.close();
    }
}

/// Every `undeliverable` verdict in `sids`' inboxes, read with `--peek` (so nothing is released).
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
    stop_read_then(w, |_| ()).0
}

/// [`stop_and_read`], running `stopped` on the world once its broker has let the log go.
fn stop_read_then<T>(w: &mut World, stopped: impl FnOnce(&World) -> T) -> (Vec<Rec>, T) {
    if let Some(mut handle) = w.handle.take() {
        handle.shutdown();
    }
    let recs = census::read_log(std::path::Path::new(&w.broker_log), w.opts.at_rest_key);
    w.broker.take();
    let then = stopped(w);
    w.stop().unwrap_or_else(|why| {
        panic!(
            "fabric world cleanup incomplete: {why}; kept {}",
            w.tmp.display()
        )
    });
    (recs, then)
}

/// Opens of the stopped broker's log a backlog run times.
const REOPENS: usize = 5;

/// What a broker's start costs on the stopped world's whole log.
struct Reopen {
    /// The log's length.
    bytes: u64,
    /// [`REOPENS`] opens, each what `aterm link broker` does before it binds: the open (every
    /// retained record scanned, under a key authenticated) and its retention rules.
    ms: Vec<f64>,
    /// What the last open held in memory, read before it was dropped (D10).
    held: Option<backlog::Held>,
}

/// Time [`REOPENS`] opens of `w`'s stopped log, under the options it served with.
fn reopen(w: &World) -> Reopen {
    let mut held = None;
    let ms = (0..REOPENS)
        .map(|i| {
            let start = Instant::now();
            let broker = if w.retained {
                harness::open_broker(&w.broker_log, &w.opts)
            } else {
                harness::open_broker_unretained(&w.broker_log, &w.opts)
            };
            let took = start.elapsed().as_secs_f64() * 1e3;
            if i + 1 == REOPENS {
                // Its residency alone: by now this process also holds the census's copy of
                // the whole log ([`stop_read_then`]), so its footprint is not the broker's.
                held = Some(backlog::Held {
                    footprint: None,
                    peak_footprint: None,
                    ..backlog::held(&broker)
                });
            }
            drop(broker);
            took
        })
        .collect();
    Reopen {
        bytes: std::fs::metadata(&w.broker_log).map_or(0, |m| m.len()),
        ms,
        held,
    }
}

/// Run `phases` beside a head sampler on `w`'s broker.
fn sampled<T: Send>(w: &World, clock: Clock, phases: impl FnOnce() -> T + Send) -> (T, Heads) {
    let broker = w.broker.as_ref().expect("the world serves its own broker");
    sampled_on(broker, clock, phases)
}

/// [`sampled`] on any broker this process serves (a fleet's, say).
fn sampled_on<T: Send>(
    broker: &Broker,
    clock: Clock,
    phases: impl FnOnce() -> T + Send,
) -> (T, Heads) {
    let stop = AtomicBool::new(false);
    std::thread::scope(|s| {
        let sampler = s.spawn(|| {
            clock::sample_heads(
                clock,
                || (broker.visible_head(), broker.durable_head()),
                &stop,
            )
        });
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

/// The last records, seen durable.
fn await_durable(broker: &Broker) {
    if broker.served_durability().served != astream_broker::Tier::Ordered {
        return;
    }
    let target = broker.visible_head();
    aterm_spec::wait::within(DEADLINE, IDLE_POLL, || {
        (broker.durable_head() >= target).then_some(())
    })
    .unwrap_or_else(|| {
        panic!(
            "the ordered broker's durable head never reached {target} within {DEADLINE:?}: \
             its horizon flush did not come"
        )
    });
}

/// The flushes a window cost.
fn flushes_between(before: FlushCounts, after: FlushCounts) -> FlushCounts {
    FlushCounts {
        barrier: after.barrier - before.barrier,
        full: after.full - before.full,
        skipped: after.skipped - before.skipped,
        failed: after.failed - before.failed,
    }
}

/// A window's flushes for the report's JSON: the counts and each per delivered message.
fn flushes_json(f: FlushCounts, n: usize) -> Value {
    let per = |v: u64| (n > 0).then(|| stats::round3(v as f64 / n as f64));
    json!({
        "barrier": f.barrier,
        "full": f.full,
        "skipped": f.skipped,
        "failed": f.failed,
        "barrier_per_message": per(f.barrier),
        "full_per_message": per(f.full),
        "skipped_per_message": per(f.skipped),
    })
}

/// A window's flushes for the report's tables.
fn flushes_line(f: FlushCounts, n: usize) -> String {
    let per = |v: u64| {
        if n == 0 {
            "–".to_string()
        } else {
            format!("{:.3}", v as f64 / n as f64)
        }
    };
    format!(
        "Broker flushes in the window (`Broker::flush_counts`): {} barrier ({} per message), {} \
         full ({} per message), {} full skipped as covered, {} failed.\n",
        f.barrier,
        per(f.barrier),
        f.full,
        per(f.full),
        f.skipped,
        f.failed
    )
}

/// What the broker wrote between a delivery and its inbox commit, over `of` round trips
/// (`rounds` of them saw any), for the latency section.
fn beside_line(c: &census::Census, rounds: usize, of: usize) -> String {
    if c.total == 0 {
        return format!(
            "Between each delivery and its inbox commit the broker wrote nothing else, in all \
             {of} round trips (warm-up included).\n"
        );
    }
    let kinds: Vec<String> = c
        .by_class
        .iter()
        .map(|(class, n)| format!("`{class}` {n}"))
        .collect();
    format!(
        "Between a delivery and its inbox commit the broker wrote {} other records, in {rounds} \
         of the {of} round trips (warm-up included), by kind: {}. None of them ended a round \
         trip's wait, and each round trip's commit sat below the visible head read before the \
         next post went out.\n",
        c.total,
        kinds.join(", ")
    )
}

/// The broker's retention ticks so far, and the publishes it refused for the rate
/// (`BrokerStats::rate_limited`, critic item E06).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct Ticks {
    run: u64,
    skipped: u64,
    rate_limited: u64,
}

/// The broker's ticks now: read through `Broker::stats`, under the log lock, so never inside a timed round trip.
fn ticks(broker: &Broker) -> Ticks {
    let stats = broker.stats();
    Ticks {
        run: stats.retention_ticks,
        skipped: stats.retention_skips,
        rate_limited: stats.rate_limited,
    }
}

/// The ticks a window held: `after - before`.
fn ticks_between(before: Ticks, after: Ticks) -> Ticks {
    Ticks {
        run: after.run - before.run,
        skipped: after.skipped - before.skipped,
        rate_limited: after.rate_limited - before.rate_limited,
    }
}

/// A window's ticks for the report's JSON.
fn ticks_json(t: Ticks) -> Value {
    json!({ "run": t.run, "skipped": t.skipped })
}

/// A window's ticks for the report's tables, beside its flushes, and its refusals for the rate.
fn ticks_line(t: Ticks) -> String {
    format!(
        "Retention ticks in the window (`BrokerStats::retention_ticks`, `retention_skips`): {} \
         run, {} skipped. Publishes the broker refused for the rate in the window \
         (`BrokerStats::rate_limited`): {}.\n",
        t.run, t.skipped, t.rate_limited
    )
}

/// Publishes in one yardstick block.
const CALIBRATION_PUBLISHES: usize = 50;

/// The producer id the yardstick publishes under.
const CALIBRATION_PRODUCER: u64 = 0x0fab_0be7_0000_0001;

/// The yardstick: blocks of `CALIBRATION_PUBLISHES` bare publishes, one at a time, from a client in this process — one round trip each, no aterm, no bridge — of three kinds ([`Yardstick`]).
fn calibrate(ruler: &Ruler, node: &str, clock: Clock, seq: &mut u64) -> Yardstick {
    let block = |c: &mut astream_broker::Client, subject: &str, seq: &mut u64| -> Vec<f64> {
        (0..CALIBRATION_PUBLISHES)
            .map(|_| {
                *seq += 1;
                let t0 = clock.us();
                c.publish(CALIBRATION_PRODUCER, *seq, subject, b"yardstick")
                    .unwrap_or_else(|e| panic!("yardstick publish {seq} on {subject}: {e}"));
                ms(t0, clock.us())
            })
            .collect()
    };
    let mut strict = ruler.strict_client();
    let mut ordered = ruler.ordered_client();
    let escalated = format!(
        "{}/note",
        aterm_link::subject::person_lane(FLEET, "h-bench", node)
    );
    Yardstick {
        strict: block(
            &mut strict,
            &format!("/f/{FLEET}/bench/strict-publish"),
            seq,
        ),
        ordered: block(
            &mut ordered,
            &format!("/f/{FLEET}/bench/ordered-publish"),
            seq,
        ),
        escalated: block(&mut ordered, &escalated, seq),
    }
}

/// One yardstick block of each kind ([`calibrate`]), in milliseconds.
struct Yardstick {
    /// Bare publishes to a strict broker: the unit.
    strict: Vec<f64>,
    /// Bare publishes to an ordered broker (aterm's options, the default horizon), on a subject its table does not escalate.
    ordered: Vec<f64>,
    /// The same, on a person-lane subject its table escalates.
    escalated: Vec<f64>,
}

/// The brokers the yardstick publishes to.
struct Ruler {
    own: Option<(Broker, BrokerHandle)>,
    sock: String,
    ordered: (Broker, BrokerHandle),
    ordered_sock: String,
}

impl Ruler {
    fn for_world(w: &World) -> Ruler {
        Ruler::beside(&w.tmp, w.opts.durability, &w.broker_sock)
    }

    /// The yardstick's brokers in `tmp`, beside a broker of `durability`
    /// served on `sock`, whose strict blocks take that broker when it is
    /// strict.
    fn beside(tmp: &Path, durability: Durability, sock: &str) -> Ruler {
        let serve = |log: &str, sock: &str, tier: aterm_link::durability::Durability| {
            let sock = tmp.join(sock).to_string_lossy().into_owned();
            let broker = harness::open_broker(tmp.join(log), &harness::broker_opts(tier));
            let handle = broker
                .serve(&sock)
                .unwrap_or_else(|e| panic!("serve the yardstick's broker on {sock}: {e}"));
            ((broker, handle), sock)
        };
        let (ordered, ordered_sock) = serve(
            "yo.log",
            "yo.sock",
            aterm_link::durability::Durability::Ordered,
        );
        if durability == Durability::Strict {
            return Ruler {
                own: None,
                sock: sock.to_string(),
                ordered,
                ordered_sock,
            };
        }
        let (own, sock) = serve(
            "y.log",
            "y.sock",
            aterm_link::durability::Durability::Strict,
        );
        Ruler {
            own: Some(own),
            sock,
            ordered,
            ordered_sock,
        }
    }

    /// A client of `sock` holding the whole fleet's cap, as [`World::god`] is.
    fn god_of(sock: &str) -> astream_broker::Client {
        let mut c = harness::broker_client(sock);
        let cap = astream_cap::mint(harness::SECRET, &format!("/f/{FLEET}/>"))
            .expect("mint the yardstick's cap");
        c.attach(&cap.filter, &cap.tag)
            .expect("attach the yardstick's cap");
        c
    }

    /// A god client of the strict block's broker: the world's own when it is
    /// strict ([`World::god`] is this client of its socket).
    fn strict_client(&self) -> astream_broker::Client {
        Self::god_of(&self.sock)
    }

    /// A god client of the ordered blocks' broker.
    fn ordered_client(&self) -> astream_broker::Client {
        Self::god_of(&self.ordered_sock)
    }

    /// What the ordered blocks' broker serves (astream serves ordered strict off Apple and off the boot volume).
    fn ordered_served(&self) -> astream_broker::ServedInfo {
        self.ordered.0.served_durability()
    }
}

impl Drop for Ruler {
    fn drop(&mut self) {
        if let Some((_, handle)) = &mut self.own {
            handle.shutdown();
        }
        self.ordered.1.shutdown();
    }
}

/// Boot one of the run's worlds as the arm names it.
fn boot_world(tag: &str, spec: &Spec, env: &[(&str, &str)]) -> World {
    let flags: &[&str] = match spec.log {
        Log::Auto => &[],
        Log::Off => &["--log", "off"],
    };
    let w = World::boot_arm(tag, opts_for(spec), spec.retention, flags, env);
    if !spec.rate_limit {
        w.broker
            .as_ref()
            .expect("the world serves its own broker")
            .set_rate_limit(None)
            .expect("lift the broker's rate limit");
    }
    if let Some(ms) = spec.retention_interval_ms {
        w.broker
            .as_ref()
            .expect("the world serves its own broker")
            .set_retention_interval(Duration::from_millis(ms));
    }
    w
}

/// The options a run's world opens its broker with.
fn opts_for(spec: &Spec) -> OpenOptions {
    use aterm_link::durability::Durability as Aterm;
    let mut opts = harness::broker_opts(match spec.durability {
        Tier::Strict => Aterm::Strict,
        Tier::Ordered => Aterm::Ordered,
        Tier::Relaxed => Aterm::Relaxed,
    });
    if let Some(ms) = spec.horizon_ms {
        opts.durability = Durability::Ordered {
            horizon: Some(Duration::from_millis(ms)),
        };
    }
    if spec.at_rest {
        opts = harness::sealed_opts(opts);
    }
    opts
}

/// The strict yardstick's p50 over both blocks: the run's unit.
fn yardstick(before: &Yardstick, after: &Yardstick) -> Option<f64> {
    let pooled: Vec<f64> = before.strict.iter().chain(&after.strict).copied().collect();
    stats::summarize(&pooled).map(|s| s.p50)
}

/// `v / unit`, rounded, for a ratio to the yardstick.
fn per_unit(v: Option<f64>, unit: Option<f64>) -> Option<f64> {
    let (v, unit) = (v?, unit?);
    (unit > 0.0).then(|| stats::round3(v / unit))
}

/// The yardstick's table rows.
fn yardstick_rows(ruler: &Ruler, before: &Yardstick, after: &Yardstick) -> Vec<String> {
    let served = ruler.ordered_served();
    vec![
        "| yardstick: one bare publish, client -> broker (ms) | n | mean | p50 | p95 | p99 |"
            .to_string(),
        "|---|---:|---:|---:|---:|---:|".to_string(),
        stats::row("strict, before the phases (the unit)", &before.strict),
        stats::row("strict, after the phases (the unit)", &after.strict),
        stats::row("ordered, before the phases", &before.ordered),
        stats::row("ordered, after the phases", &after.ordered),
        stats::row(
            "ordered, escalated (a person lane), before",
            &before.escalated,
        ),
        stats::row(
            "ordered, escalated (a person lane), after",
            &after.escalated,
        ),
        String::new(),
        format!(
            "The ordered yardstick's broker serves {} (asked {}, reason {:?}).\n",
            aterm_link::durability::tier_name(served.served),
            aterm_link::durability::tier_name(served.requested),
            served.reason
        ),
    ]
}

/// The yardstick blocks for the report's JSON.
fn yardstick_json(
    ruler: &Ruler,
    before: &Yardstick,
    after: &Yardstick,
    unit: Option<f64>,
) -> Value {
    json!({
        "what": "one bare publish round trip, a client in the bench process to a broker, serial: strict (the unit: one full flush), ordered (one barrier) and ordered on an escalated person-lane subject (one full flush before the ack)",
        "before_ms": stats::to_json(&before.strict),
        "after_ms": stats::to_json(&after.strict),
        "p50_ms": unit.map(stats::round3),
        "ordered_before_ms": stats::to_json(&before.ordered),
        "ordered_after_ms": stats::to_json(&after.ordered),
        "escalated_before_ms": stats::to_json(&before.escalated),
        "escalated_after_ms": stats::to_json(&after.escalated),
        "ordered_served": aterm_link::durability::tier_name(ruler.ordered_served().served),
    })
}

fn fmt_x(v: Option<f64>) -> String {
    v.map_or("–".to_string(), |v| format!("{v:.2}"))
}

/// One measured round trip.
struct LatSample {
    text: String,
    t_call: u64,
    t_ret: u64,
    t_seen: u64,
}

/// How finely [`await_commit`] watches the visible head for the previous message's commit
/// to land.
const IDLE_POLL: Duration = Duration::from_micros(100);

/// What the latency phase measured.
struct LatRaw {
    /// The measured round trips.
    samples: Vec<LatSample>,
    /// Every round trip, warm-up included: its text, and the broker's visible head read
    /// once its wait for the commit ended, before the next post went out. After the run
    /// each one's commit is held to that head ([`census::lands_before`]): a commit the next
    /// post went out ahead of sits at that head or past it.
    trips: Vec<(String, u64)>,
    /// The newest row id it listed.
    since: u64,
    /// The broker's flushes over the measured round trips.
    flushes: FlushCounts,
    /// The broker's retention ticks over the same window ([`Ticks`]).
    ticks: Ticks,
}

/// [`await_commit`]'s probe over its two reads: `head`, the lock-free visible head, on
/// every poll, and `start`, where the group resumes (it takes the log lock), only on the
/// first poll and on one that found the head moved since `start` last ran. A commit
/// counts in `start` with the batch that makes it visible, which moves the head, so a
/// poll that finds the head where it was finds `start` where it was too. That holds in
/// every tier but Replicated, which the bench never serves.
fn commit_counted(
    off: u64,
    mut head: impl FnMut() -> u64,
    mut start: impl FnMut() -> u64,
) -> impl FnMut() -> Option<()> {
    let mut read_at = None;
    move || {
        let now = head();
        if read_at == Some(now) {
            return None;
        }
        read_at = Some(now);
        (start() > off).then_some(())
    }
}

/// Wait for `group`'s commit through offset `off` to land: until the broker counts it
/// where the group resumes ([`Broker::group_start`], which counts a commit with the batch
/// that makes it visible). Whatever else lands first — the bus mirror's `ev` rows, which
/// the bridge writes beside its inbox commit — does not end the wait.
///
/// The wait runs inside a measured window: a round trip's visible -> the inbox commit
/// acked, and the burst's time to the commit covering its last message. `group_start`
/// takes the log lock, which the writer stages and promotes that commit's batch under, so
/// the wait polls the lock-free visible head every [`IDLE_POLL`], as the head poll before
/// it did, and reads `group_start` only when the head has moved ([`commit_counted`]): at
/// most once per batch the writer publishes, never on a poll that found the head where it
/// was.
///
/// # Panics
///
/// If the polling wait expires without observing the commit, naming `what`. Each
/// read waits for the broker's log lock; [`DEADLINE`] does not time that acquisition.
fn await_commit(broker: &Broker, group: &str, off: u64, what: &str) {
    aterm_spec::wait::within(
        DEADLINE,
        IDLE_POLL,
        commit_counted(off, || broker.visible_head(), || broker.group_start(group)),
    )
    .unwrap_or_else(|| {
        panic!(
            "{what}: no commit of {group} through offset {off} landed within {DEADLINE:?} (the \
             group resumes at {})",
            broker.group_start(group)
        )
    });
}

/// The latency phase: `warmup + samples` round trips, one at a time, each after the
/// one before it has landed its inbox commit ([`await_commit`]).
fn latency(w: &World, broker: &Broker, spec: &Spec, clock: Clock, a: &str, b: &str) -> LatRaw {
    let group = aterm_link::subject::inbox_group(FLEET, &w.node);
    let mut ctl = w.ctl();
    let mut since = newest_id(&mut ctl, b);
    let mut out = Vec::with_capacity(spec.samples);
    let mut trips = Vec::with_capacity(spec.warmup + spec.samples);
    let mut flushes_before = broker.flush_counts();
    let mut ticks_before = ticks(broker);
    for i in 0..spec.warmup + spec.samples {
        if i == spec.warmup {
            flushes_before = broker.flush_counts();
            ticks_before = ticks(broker);
        }
        let text = format!("lat-{i}");
        let (t_call, t_ret) = post(&mut ctl, clock, a, b, &text);
        let (woke_at, t_seen) =
            await_note(&mut ctl, clock, b, since).unwrap_or_else(|e| panic!("round trip {i}: {e}"));
        let rows = list_since(&mut ctl, b, since);
        assert!(
            rows.len() == 1 && rows[0].text == text && rows[0].id == woke_at,
            "round trip {i}: woke at row {woke_at}, listed {rows:?}"
        );
        await_commit(broker, &group, rows[0].off, &format!("round trip {i}"));
        trips.push((text.clone(), broker.visible_head()));
        since = woke_at;
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
        trips,
        since,
        flushes: flushes_between(flushes_before, broker.flush_counts()),
        ticks: ticks_between(ticks_before, ticks(broker)),
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
    /// The broker's batches over the burst.
    batches: u64,
    /// The flushes those batches cost, by kind, over the same window ([`Broker::flush_counts`]).
    flushes: FlushCounts,
    /// The broker's retention ticks over the same window ([`Ticks`]).
    ticks: Ticks,
    /// The broker's visible head once the wait for the commit covering the last message
    /// ended, read before the fence's post went out: after the run the commit is held to it
    /// ([`census::lands_before`]).
    fence_head: u64,
}

/// The burst phase, reading `b`'s inbox on from row `since` (the latency phase's last).
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
    for (i, s) in senders.iter().enumerate() {
        post(&mut ctl, clock, s, b, &format!("warm-{i}"));
    }
    let since = consume(&mut ctl, clock, b, since, "warm-", k, None)
        .unwrap_or_else(|e| panic!("burst warm-up: {e}"))
        .since;

    let n = spec.burst;
    let credit = Credit::new(BURST_WINDOW);
    let flushed = || {
        let stats = broker.stats();
        stats.writer_batches - stats.empty_batches
    };
    let flushed_before = flushed();
    let flush_counts_before = broker.flush_counts();
    let ticks_before = ticks(broker);
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

    let b_last = consumed.seen.iter().map(|s| s.off).max().unwrap_or(0);
    await_commit(
        broker,
        &aterm_link::subject::inbox_group(FLEET, &w.node),
        b_last,
        "burst",
    );
    let fence_head = broker.visible_head();
    let batches = flushed() - flushed_before;
    let flushes = flushes_between(flush_counts_before, broker.flush_counts());
    let ticks = ticks_between(ticks_before, ticks(broker));
    post(&mut ctl, clock, a, b, "fence-0");
    consume(&mut ctl, clock, b, consumed.since, "fence-", 1, None)
        .unwrap_or_else(|e| panic!("burst fence: {e}"));
    BurstRaw {
        senders: k,
        t0,
        rets,
        seen,
        batches,
        flushes,
        ticks,
        fence_head,
    }
}

/// One paced note ([`paced_notes`]).
struct PacedNote {
    text: String,
    /// When A's post call went out.
    t_post: u64,
    /// When its row was visible in B's inbox.
    t_seen: u64,
    /// The writer batches that appended a record from just before its post to just before
    /// the next note's (the last note's: once its pace was out).
    batches: u64,
}

/// What the paced-notes phase measured.
struct PacedRaw {
    /// The sending and the receiving session.
    sids: (String, String),
    notes: Vec<PacedNote>,
    /// The broker's flushes over the phase, from the first note's post to the last one's pace.
    flushes: FlushCounts,
    /// The broker's retention ticks over the same window ([`Ticks`]).
    ticks: Ticks,
}

/// The paced-notes phase (C13): `spec.paced_notes` one-way notes from A to B, [`NOTE_PACE`]
/// apart, none answered. Each delivery moves B's mail count, and the notes go slower than
/// the mirror's one snapshot of a moved count a second, so each owes the bus mirror B's
/// snapshot, as an agent's delivery does. Each note waits for its delivery's inbox commit
/// ([`await_commit`]) and then the rest of its pace, so its window holds what that delivery
/// cost and nothing of the next. It starts one pace after the burst's fence, so the burst's
/// own snapshots are out of its first window.
fn paced_notes(
    w: &World,
    broker: &Broker,
    spec: &Spec,
    clock: Clock,
    a: &str,
    b: &str,
) -> PacedRaw {
    let group = aterm_link::subject::inbox_group(FLEET, &w.node);
    let mut ctl = w.ctl();
    let batches = || {
        let stats = broker.stats();
        stats.writer_batches - stats.empty_batches
    };
    std::thread::sleep(NOTE_PACE);
    let mut since = newest_id(&mut ctl, b);
    let mut notes = Vec::with_capacity(spec.paced_notes);
    let flushes_before = broker.flush_counts();
    let ticks_before = ticks(broker);
    let mut before = batches();
    for i in 0..spec.paced_notes {
        let paced = Instant::now() + NOTE_PACE;
        let text = format!("paced-{i}");
        let t_post = clock.us();
        post(&mut ctl, clock, a, b, &text);
        let (woke_at, t_seen) =
            await_note(&mut ctl, clock, b, since).unwrap_or_else(|e| panic!("paced note {i}: {e}"));
        let rows = list_since(&mut ctl, b, since);
        assert!(
            rows.len() == 1 && rows[0].text == text && rows[0].id == woke_at,
            "paced note {i}: woke at row {woke_at}, listed {rows:?}"
        );
        await_commit(broker, &group, rows[0].off, &format!("paced note {i}"));
        since = woke_at;
        std::thread::sleep(paced.saturating_duration_since(Instant::now()));
        let after = batches();
        notes.push(PacedNote {
            text,
            t_post,
            t_seen,
            batches: after - before,
        });
        before = after;
    }
    PacedRaw {
        sids: (a.to_string(), b.to_string()),
        notes,
        flushes: flushes_between(flushes_before, broker.flush_counts()),
        ticks: ticks_between(ticks_before, ticks(broker)),
    }
}

/// One paced note's records, as the visible head took them ([`paced_report`]): each step of
/// the head (one writer batch, as far as the 100 µs sampling tells two apart) in log order,
/// `|` between steps and `+` within one; `note` is the delivery, `commit` the inbox commit
/// covering it, `evA` and `evB` the sender's and the recipient's mirror rows, anything else
/// its kind's last segment. A record before the delivery belongs to the note when it landed
/// after the post call; after `||`, what landed past the commit's step before the next
/// note's post call (the last note's: within its pace). `landed` is when the visible head
/// first passed an offset ([`clock::Timeline::landed`]).
fn paced_shape(
    recs: &[Rec],
    landed: &dyn Fn(u64) -> Option<u64>,
    sids: (&str, &str),
    note: (u64, u64, u64),
    from: u64,
    tail_before: u64,
) -> String {
    let (seq, end, t_post) = note;
    let label = |r: &Rec| -> String {
        if r.seq == seq {
            "note".to_string()
        } else if r.commit.is_some() {
            "commit".to_string()
        } else if r.subject.ends_with("/ev") && r.subject.contains(&format!("/{}/", sids.0)) {
            "evA".to_string()
        } else if r.subject.ends_with("/ev") && r.subject.contains(&format!("/{}/", sids.1)) {
            "evB".to_string()
        } else {
            census::class_of(&r.subject)
                .rsplit('/')
                .next()
                .unwrap_or_default()
                .to_string()
        }
    };
    let steps = |rows: Vec<&Rec>| -> String {
        let mut out = String::new();
        let mut last: Option<Option<u64>> = None;
        for r in rows {
            let at = landed(r.seq);
            match last {
                None => {}
                Some(prev) if prev == at && at.is_some() => out.push('+'),
                Some(_) => out.push_str(" | "),
            }
            out.push_str(&label(r));
            last = Some(at);
        }
        out
    };
    let own: Vec<&Rec> = census::window(recs, from, end)
        .filter(|r| r.seq >= seq || landed(r.seq).is_some_and(|t| t >= t_post))
        .collect();
    let tail: Vec<&Rec> = recs
        .iter()
        .filter(|r| r.seq > end)
        .take_while(|r| landed(r.seq).is_some_and(|t| t < tail_before))
        .collect();
    let mut shape = steps(own);
    if !tail.is_empty() {
        shape.push_str(" || ");
        shape.push_str(&steps(tail));
    }
    shape
}

/// The paced-notes phase's section ([`paced_notes`]), read off the stopped log: per note, its
/// commit, the `ev` rows that landed between the delivery and that commit, and the `ev` rows
/// that landed in the commit's own step of the visible head (one batch with it), past it.
fn paced_report(
    raw: &PacedRaw,
    recs: &[Rec],
    group: &str,
    heads: &Heads,
    seq_of: impl Fn(&str) -> u64,
) -> (Value, Vec<String>) {
    let n = raw.notes.len();
    let ev = |r: &Rec| r.subject.ends_with("/ev");
    let mut visible_to_committed = Vec::new();
    let (mut before, mut notes_before, mut with, mut notes_with) = (0usize, 0usize, 0usize, 0usize);
    let mut by_batches: BTreeMap<u64, usize> = BTreeMap::new();
    let mut spans: Vec<(u64, u64, u64)> = Vec::with_capacity(n);
    for note in &raw.notes {
        let seq = seq_of(&note.text);
        let commit = census::covering_commit(recs, group, seq, seq)
            .unwrap_or_else(|| panic!("`{}`: no commit of {group} covers offset {seq}", note.text));
        let landed = heads.acked.landed(commit.seq);
        visible_to_committed.extend(ms_opt(Some(note.t_seen), landed));
        let owed = census::before_its_commit(recs, group, seq)
            .expect("the commit covers it")
            .into_iter()
            .filter(|r| ev(r))
            .count();
        before += owed;
        notes_before += usize::from(owed > 0);
        let step: Vec<&Rec> = recs
            .iter()
            .filter(|r| r.seq > commit.seq)
            .take_while(|r| landed.is_some() && heads.acked.landed(r.seq) == landed)
            .collect();
        spans.push((seq, step.last().map_or(commit.seq, |r| r.seq), note.t_post));
        let rode = step.into_iter().filter(|r| ev(r)).count();
        with += rode;
        notes_with += usize::from(rode > 0);
        *by_batches.entry(note.batches).or_default() += 1;
    }
    let pace_us = u64::try_from(NOTE_PACE.as_micros()).unwrap_or(u64::MAX);
    let mut shapes: BTreeMap<String, usize> = BTreeMap::new();
    for (i, span) in spans.iter().enumerate() {
        let from = if i == 0 { 0 } else { spans[i - 1].1 + 1 };
        let tail_before = spans
            .get(i + 1)
            .map_or(span.2.saturating_add(pace_us), |next| next.2);
        let shape = paced_shape(
            recs,
            &|seq| heads.acked.landed(seq),
            (&raw.sids.0, &raw.sids.1),
            *span,
            from,
            tail_before,
        );
        *shapes.entry(shape).or_default() += 1;
    }
    let mut shapes: Vec<(String, usize)> = shapes.into_iter().collect();
    shapes.sort_by(|x, y| y.1.cmp(&x.1).then_with(|| x.0.cmp(&y.0)));
    let first = raw.notes.first().map(|note| seq_of(&note.text));
    let last = raw.notes.last().map(|note| seq_of(&note.text));
    let window = first.zip(last).map(|(lo, last)| {
        let hi = census::covering_commit(recs, group, last, last).map_or(last, |r| r.seq);
        (lo, hi)
    });
    let phase = window.map(|(lo, hi)| census::census(census::window(recs, lo, hi)));
    let batches: u64 = raw.notes.iter().map(|note| note.batches).sum();
    let per = |v: usize| (n > 0).then(|| stats::round3(v as f64 / n as f64));
    let json = json!({
        "notes": n,
        "pace_ms": u64::try_from(NOTE_PACE.as_millis()).unwrap_or(u64::MAX),
        "visible_to_commit_acked_ms": stats::to_json(&visible_to_committed),
        "broker_batches": batches,
        "broker_batches_per_note": (n > 0).then(|| stats::round3(batches as f64 / n as f64)),
        "notes_by_batches": by_batches
            .iter()
            .map(|(k, v)| (k.to_string(), json!(v)))
            .collect::<aterm_json::Map>(),
        "flushes": flushes_json(raw.flushes, n),
        "retention_ticks": ticks_json(raw.ticks),
        "rate_limited": raw.ticks.rate_limited,
        "ev_between_delivery_and_commit": {"records": before, "notes_with_any": notes_before, "per_note": per(before)},
        "ev_in_the_commits_batch": {"records": with, "notes_with_any": notes_with, "per_note": per(with)},
        "shapes": shapes
            .iter()
            .map(|(shape, count)| json!({"shape": shape, "notes": count}))
            .collect::<Vec<_>>(),
        "census": match (&phase, window) {
            (Some(c), Some(w)) => census::to_json(c, n, w),
            _ => Value::Null,
        },
    });
    let mut md = vec![
        format!(
            "## Paced notes: {n} one-way notes, {} ms apart (C13)\n",
            NOTE_PACE.as_millis()
        ),
        format!(
            "After the burst, A posts `kind=note` to B {n} times, {} ms apart, and B never \
             answers: each delivery moves B's mail count past the bus mirror's one snapshot a \
             second, so each owes the mirror B's snapshot, as an agent's delivery does. Each \
             note waits for its inbox commit, then the rest of its pace. A note's batches are \
             the writer batches that appended a record from just before its post to just \
             before the next one's; an `ev` row in the commit's batch is one past the commit \
             that the visible head passed in the same step as the commit.\n",
            NOTE_PACE.as_millis()
        ),
        "| measure (ms) | n | mean | p50 | p95 | p99 |".to_string(),
        "|---|---:|---:|---:|---:|---:|".to_string(),
        stats::row("visible -> the inbox commit acked", &visible_to_committed),
        String::new(),
        format!(
            "Broker batches per note: {} ({batches} over {n}); notes by their batches: {}.\n",
            per(usize::try_from(batches).unwrap_or(usize::MAX))
                .map_or("–".to_string(), |v| format!("{v:.3}")),
            by_batches
                .iter()
                .map(|(k, v)| format!("{v} with {k}"))
                .collect::<Vec<_>>()
                .join(", ")
        ),
        flushes_line(raw.flushes, n),
        ticks_line(raw.ticks),
        format!(
            "`ev` rows between a delivery and its inbox commit: {before}, in {notes_before} of \
             {n} notes. `ev` rows in the commit's own batch, past it: {with}, in {notes_with} \
             of {n} notes.\n"
        ),
        format!(
            "Each note's records as the visible head took them (`|` between its steps, `+` \
             within one, `note` the delivery, `evA`/`evB` the sender's and the recipient's \
             mirror rows; after `||`, what landed past the commit's step before the next \
             post): {}.\n",
            shapes
                .iter()
                .map(|(shape, count)| format!("`{shape}` {count}"))
                .collect::<Vec<_>>()
                .join(", ")
        ),
    ];
    if let (Some(c), Some((lo, hi))) = (&phase, window) {
        md.push(format!(
            "Records the broker wrote per note (offsets {lo}..={hi}, through the commit covering \
             the last one):\n"
        ));
        md.extend(census::rows(c, n));
        md.push(String::new());
    }
    (json, md)
}

/// What a world's broker serves, in one line for its section.
fn served_line(w: &World, spec: &Spec) -> String {
    served_line_of(
        w.broker.as_ref().expect("the world serves its own broker"),
        spec,
    )
}

/// [`served_line`] for any broker this process serves (a fleet's, say).
fn served_line_of(broker: &Broker, spec: &Spec) -> String {
    let s = broker.served_durability();
    let horizon = s
        .horizon_ms
        .map_or_else(|| "none".to_string(), |ms| format!("{ms} ms"));
    let retains = match broker.retention() {
        Some(rules) => format!(
            "retains its log by {} rules (aterm's defaults, `broker_rules::retain`), its \
             retention tick every {} ms",
            rules.rules().len() + 1,
            broker.retention_interval().as_millis()
        ),
        None => {
            "retains nothing (no retention rules: a broker from before astream's R4)".to_string()
        }
    };
    let limits = match broker.rate_limit() {
        Some(limit) if limit == aterm_link::broker_rules::rate_limit() => format!(
            "limits each producer by aterm's default ({}, `broker_rules::limit`)",
            aterm_link::broker_rules::rate_summary()
        ),
        Some(limit) => format!("limits each producer by {limit:?}"),
        None => "limits no producer's rate".to_string(),
    };
    format!(
        "The world's broker serves **{}** (asked {}, reason {:?}; horizon {horizon}; {} \
         escalation rules), {retains} and {limits}; the bridge runs `--log {}`{}.\n",
        aterm_link::durability::tier_name(s.served),
        aterm_link::durability::tier_name(s.requested),
        s.reason,
        s.rules,
        spec.log.word(),
        match (spec.log, broker.retention().is_some()) {
            (Log::Auto, true) => " (the mirror is on: the broker retains the `ev` faces)",
            (Log::Auto, false) => " (the mirror stays off: the broker retains nothing)",
            (Log::Off, _) => " (no mirror)",
        }
    )
}

/// The messaging world: boot, measure, census.
fn messaging(spec: &Spec, clock: Clock) -> (Value, Vec<String>) {
    eprintln!("fabric bench: booting the messaging world");
    let mut w = boot_world("fbench-msg", spec, &tallies::world_env(spec, &[]));
    w.wait_ready();
    let served = served_line(&w, spec);
    let (a, b) = w.two_sessions();
    let mut live = vec![a.clone(), b.clone()];
    live.extend(bystanders(&w, spec.bystanders));
    wait_live(
        &w,
        &live,
        0,
        "both sessions' presence rows, and every bystander's, to say live",
    );
    let ruler = Ruler::for_world(&w);
    let mut cal_seq = 0;
    let cal_before = calibrate(&ruler, &w.node, clock, &mut cal_seq);
    eprintln!(
        "fabric bench: latency, {} round trips ({} warm-up)",
        spec.samples, spec.warmup
    );
    let ((lat_raw, bur, paced_raw, counted), heads) = sampled(&w, clock, || {
        let broker = w.broker.as_ref().expect("the world serves its own broker");
        // The rounds are the phases; A, which only sends, holds the fence.
        let mut probe = spec.tally.then(|| tallies::Probe::of(&w, broker, &a));
        let group = "messaging";
        if let Some(p) = probe.as_mut() {
            p.start();
        }
        let lat = latency(&w, broker, spec, clock, &a, &b);
        if let Some(p) = probe.as_mut() {
            p.round(group, "the latency phase, its warm-up included");
        }
        eprintln!(
            "fabric bench: burst, {} posts from {} senders, at most {BURST_WINDOW} in flight",
            spec.burst,
            spec.burst_senders()
        );
        let bur = burst(&w, broker, spec, clock, &a, &b, lat.since);
        if let Some(p) = probe.as_mut() {
            p.round(group, "the burst, its senders' spawn and warm-up included");
        }
        let paced = (spec.paced_notes > 0).then(|| {
            eprintln!(
                "fabric bench: paced notes, {} one-way notes {} ms apart",
                spec.paced_notes,
                NOTE_PACE.as_millis()
            );
            let paced = paced_notes(&w, broker, spec, clock, &a, &b);
            if let Some(p) = probe.as_mut() {
                p.round(group, "the paced notes");
            }
            paced
        });
        await_durable(broker);
        (lat, bur, paced, probe.map(tallies::Probe::into_tallies))
    });
    let cal_after = calibrate(&ruler, &w.node, clock, &mut cal_seq);
    let unit = yardstick(&cal_before, &cal_after);
    let recs = stop_and_read(&mut w);
    let lane = format!("/f/{FLEET}/in/{}/{b}/{}/note", w.node, w.node);
    let group = aterm_link::subject::inbox_group(FLEET, &w.node);
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
    // Every round trip's wait for its commit, warm-up included, held to the log: the commit
    // sits below the visible head read once the wait ended, before the next post went out
    // (a commit that post carried, which the bridge writes ahead of it, sits at that head or
    // past it, as does any commit not yet visible then), and what else the broker wrote
    // while it was owed is counted.
    let mut beside = Vec::new();
    let mut rounds_beside = 0;
    for (text, head) in &lat_raw.trips {
        let seq = seq_of(text);
        census::lands_before(&recs, &group, seq, *head)
            .and_then(|()| census::before_its_commit(&recs, &group, seq))
            .map(|owed| {
                rounds_beside += usize::from(!owed.is_empty());
                beside.extend(owed);
            })
            .unwrap_or_else(|e| {
                panic!(
                    "`{text}`: the latency phase waits for each delivery's inbox commit to be \
                     visible before the next post goes out, and {e}, the visible head read \
                     before that post — so the next round trip left with this commit still \
                     owed, and may carry it. Make `await_commit` wait for the commit itself."
                )
            });
    }
    let beside_census = census::census(beside.iter().copied());
    let lat = &lat_raw.samples;

    let mut post_call = Vec::new();
    let mut deliver = Vec::new();
    let mut end_to_end = Vec::new();
    let mut to_acked = Vec::new();
    let mut acked_to_visible = Vec::new();
    let mut to_durable = Vec::new();
    let mut visible_to_committed = Vec::new();
    for s in lat {
        let seq = seq_of(&s.text);
        let acked = heads.acked.landed(seq);
        let committed = census::covering_commit(&recs, &group, seq, seq)
            .and_then(|r| heads.acked.landed(r.seq));
        post_call.push(ms(s.t_call, s.t_ret));
        deliver.push(ms(s.t_ret, s.t_seen));
        end_to_end.push(ms(s.t_call, s.t_seen));
        to_acked.extend(ms_opt(Some(s.t_ret), acked));
        acked_to_visible.extend(ms_opt(acked, Some(s.t_seen)));
        to_durable.extend(ms_opt(Some(s.t_ret), heads.durable.landed(seq)));
        visible_to_committed.extend(ms_opt(Some(s.t_seen), committed));
    }
    let lat_lo = seq_of(&lat.first().expect("at least one sample").text);
    let lat_last = seq_of(&lat.last().expect("at least one sample").text);
    let lat_hi =
        census::covering_commit(&recs, &group, lat_last, lat_last).map_or(lat_last, |r| r.seq);
    let lat_census = census::census(census::window(&recs, lat_lo, lat_hi));

    let n = bur.rets.len();
    let mut b_deliver = Vec::new();
    let mut b_to_acked = Vec::new();
    let mut b_acked_to_visible = Vec::new();
    let mut b_to_durable = Vec::new();
    let mut seqs = Vec::with_capacity(n);
    let mut acked_all = Vec::with_capacity(n);
    let mut durable_all = Vec::with_capacity(n);
    for (j, &t_ret) in bur.rets.iter().enumerate() {
        let seen = bur.seen[j];
        let seq = seq_of(&format!("burst-{j}"));
        assert_eq!(
            seq, seen.off,
            "burst-{j}: the row's off= is the record's offset"
        );
        seqs.push(seq);
        let acked = heads.acked.landed(seq);
        let durable = heads.durable.landed(seq);
        acked_all.push(acked);
        durable_all.push(durable);
        b_deliver.push(ms(t_ret, seen.t));
        b_to_acked.extend(ms_opt(Some(t_ret), acked));
        b_acked_to_visible.extend(ms_opt(acked, Some(seen.t)));
        b_to_durable.extend(ms_opt(Some(t_ret), durable));
    }
    let latest = |all: &[Option<u64>]| -> Option<u64> {
        all.iter()
            .copied()
            .collect::<Option<Vec<u64>>>()
            .and_then(|v| v.into_iter().max())
    };
    let t_sent = bur.rets.iter().copied().max().unwrap_or(bur.t0);
    let t_all_visible = bur.seen.iter().map(|s| s.t).max().unwrap_or(bur.t0);
    let t_all_acked = latest(&acked_all);
    let t_all_durable = latest(&durable_all);
    let b_lo = seqs.iter().copied().min().unwrap_or(0);
    let b_last = seqs.iter().copied().max().unwrap_or(0);
    census::lands_before(&recs, &group, b_last, bur.fence_head).unwrap_or_else(|e| {
        panic!(
            "the burst's window ends at the commit covering its last message, and {e}, the \
             visible head read before the fence's post — so the burst's counts stop short of \
             that commit, and the fence's round trip may carry it. Make `burst` wait for that \
             commit itself before it posts the fence."
        )
    });
    let cover = census::covering_commit(&recs, &group, b_last, b_last);
    let t_covered = cover.and_then(|r| heads.acked.landed(r.seq));
    let b_hi = cover.map_or(b_last, |r| r.seq);
    let b_census = census::census(census::window(&recs, b_lo, b_hi));
    let after = |t: Option<u64>| t.map(|t| stats::round3(ms(bur.t0, t)));
    let rate = |t: Option<u64>| t.map(|t| stats::round3(n as f64 / (ms(bur.t0, t) / 1000.0)));
    let p50 = |v: &[f64]| stats::summarize(v).map(|s| s.p50);
    let deliver_x = per_unit(p50(&deliver), unit);
    let burst_ms_per_message = after(Some(t_all_visible)).map(|t| t / n as f64);
    let burst_x = per_unit(burst_ms_per_message, unit);
    let (paced_json, paced_md) = paced_raw.as_ref().map_or((Value::Null, Vec::new()), |raw| {
        paced_report(raw, &recs, &group, &heads, seq_of)
    });

    let mut json = json!({
        "node": w.node.clone(),
        "served": served.trim_end(),
        "head_changes_sampled": heads.acked.len(),
        "durable_head_changes_sampled": heads.durable.len(),
        "yardstick": yardstick_json(&ruler, &cal_before, &cal_after, unit),
        "normalized": {
            "what": "the measure divided by the strict yardstick's p50: how many bare Strict publishes it costs. Takes the disk's speed out, not CPU contention: compare runs at similar load",
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
            "post_to_acked_ms": stats::to_json(&to_acked),
            "acked_to_visible_ms": stats::to_json(&acked_to_visible),
            "post_to_durable_head_ms": stats::to_json(&to_durable),
            "visible_to_commit_acked_ms": stats::to_json(&visible_to_committed),
            "flushes": flushes_json(lat_raw.flushes, lat.len()),
            "retention_ticks": ticks_json(lat_raw.ticks),
            "rate_limited": lat_raw.ticks.rate_limited,
            "census": census::to_json(&lat_census, lat.len(), (lat_lo, lat_hi)),
            "between_delivery_and_commit": {
                "what": "the records the broker wrote between a delivery and its inbox commit, over every round trip (warm-up included); each round trip waits for its commit before the next post",
                "round_trips": lat_raw.trips.len(),
                "round_trips_with_any": rounds_beside,
                "records": beside_census.total,
                "by_class": beside_census
                    .by_class
                    .iter()
                    .map(|(class, n)| (class.clone(), json!(n)))
                    .collect::<aterm_json::Map>(),
            },
        },
        "burst": {
            "messages": n,
            "senders": bur.senders,
            "in_flight_max": BURST_WINDOW,
            "all_accepted_ms": after(Some(t_sent)),
            "all_acked_ms": after(t_all_acked),
            "all_durable_head_ms": after(t_all_durable),
            "all_visible_ms": after(Some(t_all_visible)),
            "commit_covered_ms": after(t_covered),
            "acked_rate_per_s": rate(t_all_acked),
            "visible_rate_per_s": rate(Some(t_all_visible)),
            "broker_batches": bur.batches,
            "broker_batches_per_message": (n > 0).then(|| stats::round3(bur.batches as f64 / n as f64)),
            "flushes": flushes_json(bur.flushes, n),
            "retention_ticks": ticks_json(bur.ticks),
            "rate_limited": bur.ticks.rate_limited,
            "deliver_ms": stats::to_json(&b_deliver),
            "post_to_acked_ms": stats::to_json(&b_to_acked),
            "acked_to_visible_ms": stats::to_json(&b_acked_to_visible),
            "post_to_durable_head_ms": stats::to_json(&b_to_durable),
            "census": census::to_json(&b_census, n, (b_lo, b_hi)),
        },
        "paced_notes": paced_json,
    });

    let fmt_ms = |v: Option<f64>| v.map_or("not seen".to_string(), |v| format!("{v:.1}"));
    let fmt_rate = |v: Option<f64>| v.map_or("–".to_string(), |v| format!("{v:.1}"));
    let mut md = vec![
        format!(
            "## Messaging: post -> deliver on one node (node `{}`)\n",
            w.node
        ),
        served.clone(),
        format!(
            "{} round trips after {} warm-up, one at a time on an idle bridge: session A posts \
             `kind=note` (no `--wait`) to session B, and B's `await inbox` is answered. Acked = \
             the record's offset first seen under the broker's visible head (its publish acked, \
             readers may see it); durable = first seen under its durable head (on stable media \
             by a full flush). On a strict broker the two are one; on an ordered one durable \
             follows at the next full flush, at most the horizon plus a batch and a flush after \
             the first uncovered ack; on a relaxed one no record is ever durable. The inbox \
             commit follows the delivery, after the row is visible, and the next post waits for \
             it: for the broker to count it where the inbox group resumes \
             (`Broker::group_start`, read each time the visible head moves), and each \
             commit is held to the visible head read before the next post went out.\n",
            lat.len(),
            spec.warmup
        ),
        "| measure (ms) | n | mean | p50 | p95 | p99 |".to_string(),
        "|---|---:|---:|---:|---:|---:|".to_string(),
        stats::row("post call (request -> `OK`)", &post_call),
        stats::row("**post -> visible in B's inbox**", &deliver),
        stats::row("post call -> visible (end to end)", &end_to_end),
        stats::row("post -> acked on the broker", &to_acked),
        stats::row("acked -> visible", &acked_to_visible),
        stats::row("post -> durable on the broker", &to_durable),
        stats::row("visible -> the inbox commit acked", &visible_to_committed),
        String::new(),
        flushes_line(lat_raw.flushes, lat.len()),
        ticks_line(lat_raw.ticks),
        format!(
            "Records the broker wrote per delivered message (offsets {lat_lo}..={lat_hi}, through \
             the commit covering the last one):\n"
        ),
    ];
    md.extend(census::rows(&lat_census, lat.len()));
    md.extend([
        String::new(),
        beside_line(&beside_census, rounds_beside, lat_raw.trips.len()),
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
            "| every record acked on the broker | {} | {} |",
            fmt_ms(after(t_all_acked)),
            fmt_rate(rate(t_all_acked))
        ),
        format!(
            "| **every message visible in the recipient's inbox** | {} | {} |",
            fmt_ms(after(Some(t_all_visible))),
            fmt_rate(rate(Some(t_all_visible)))
        ),
        format!(
            "| every record durable on the broker | {} | – |",
            fmt_ms(after(t_all_durable))
        ),
        format!(
            "| the recipient bridge's commit covers them | {} | – |",
            fmt_ms(after(t_covered))
        ),
        String::new(),
        "| per message (ms) | n | mean | p50 | p95 | p99 |".to_string(),
        "|---|---:|---:|---:|---:|---:|".to_string(),
        stats::row("post -> visible", &b_deliver),
        stats::row("post -> acked", &b_to_acked),
        stats::row("acked -> visible", &b_acked_to_visible),
        stats::row("post -> durable", &b_to_durable),
        String::new(),
        format!(
            "Broker batches over the burst (writer batches that appended a record, counted once \
             the commit covering the last message was visible): {} — {} per message.\n",
            bur.batches,
            if n == 0 {
                "–".to_string()
            } else {
                format!("{:.3}", bur.batches as f64 / n as f64)
            }
        ),
        flushes_line(bur.flushes, n),
        ticks_line(bur.ticks),
        format!("Records per delivered message (offsets {b_lo}..={b_hi}):\n"),
    ]);
    md.extend(census::rows(&b_census, n));
    md.push(String::new());
    md.extend(paced_md);
    md.extend(yardstick_rows(&ruler, &cal_before, &cal_after));
    md.extend([format!(
        "Normalized to the strict yardstick's p50 ({} ms): post -> visible p50 = **{} \
             publishes**; the burst = {} ms per message = **{} publishes per message**. The ratio \
             takes the disk's speed out of a comparison, not CPU contention (the yardstick is one \
             thread and one flush; the fabric is three processes): compare runs taken at similar \
             load averages.\n",
        fmt_x(unit),
        fmt_x(deliver_x),
        fmt_x(burst_ms_per_message),
        fmt_x(burst_x)
    )]);
    tallies::report(counted.as_ref(), &mut json, &mut md);
    drop(ruler);
    (json, md)
}

/// One bridge restart.
struct Round {
    sessions: usize,
    /// The visible head just before the kill: every record the new bridge writes is at or past it.
    mark: u64,
    /// The restarted bridge's pid: its node row names it.
    pid: i32,
    t_kill: u64,
    t_pid: u64,
    sids: Vec<String>,
    /// The live mail ([`attach`]): its text, and the moment it was visible in its recipient's inbox.
    mail: String,
    t_mail_seen: u64,
    /// With a backlog ([`backlog`]): when every other session's live mail
    /// (`<mail>-to-<i>`, posted in the kill window after the first) was
    /// visible too, and so every session's refill done.
    t_every_mail_seen: Option<u64>,
    /// When the kill window's posts were all answered: a backlog run posts one per session
    /// there, and they can outlast the relaunch, so [`Round::t_pid`] is watched across them.
    t_posted: u64,
    /// The records the broker read back cold from just before the kill to the attach's last row
    /// and every live mail (`Broker::cold_reads`, read outside the timed window, D10d).
    cold_reads: u64,
    /// What the broker's residency tier ran over the same window ([`backlog::tier_tally`], D10f):
    /// `None` from a build that counts nothing.
    tier: Option<astream_broker::TierTally>,
}

/// The bridge's pid from its state dir, if it has written one.
fn read_pid(w: &World) -> Option<i32> {
    std::fs::read_to_string(w.state.join("pid"))
        .ok()?
        .trim()
        .parse()
        .ok()
}

/// Where one bridge's attach landed: its node row, and each session's row in the order the sessions were named.
#[derive(Debug, Clone, PartialEq, Eq)]
struct AttachRows {
    node: u64,
    sessions: Vec<u64>,
}

impl AttachRows {
    /// The last of them, the node row included: when the attach is done.
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

/// Where the bridge `pid` published its attach among `rows` — `(offset, subject, body head line)`, all at or past the round's mark.
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

/// The kill window's posts as the log holds them ([`attach`]: the live mail, and with a backlog
/// the note to every other session), which the new bridge's first drain published: the first's
/// offset and the last's (critic item G04).
fn kill_window_posts(recs: &[Rec], r: &Round, node: &str) -> Option<(u64, u64)> {
    let lanes = format!("/f/{FLEET}/in/{node}/");
    let others = format!("{}-to-", r.mail);
    let mut offs = recs
        .iter()
        .filter(|rec| {
            rec.seq >= r.mark
                && rec.subject.starts_with(&lanes)
                && rec.subject.ends_with("/note")
                && (rec.text == r.mail || rec.text.starts_with(&others))
        })
        .map(|rec| rec.seq);
    let first = offs.next()?;
    Some(offs.fold((first, first), |(lo, hi), off| (lo.min(off), hi.max(off))))
}

/// The bus mirror's first snapshot at or past `mark` (a `v=2` row of the mirror on one of the
/// node's session `ev` faces, `/f/<fleet>/pub/<node>/<sid>/ev`), the first send of the new
/// bridge's mirror after the kill (critic item G04, the row F07 named missing).
fn mirror_first_send(recs: &[Rec], mark: u64, node: &str) -> Option<u64> {
    let faces = format!("/f/{FLEET}/pub/{node}/");
    recs.iter()
        .find(|rec| {
            rec.seq >= mark
                && rec.subject.starts_with(&faces)
                && rec.subject.ends_with("/ev")
                && rec.subject.split('/').count() == 7
                && rec.head.starts_with("v=2 ")
        })
        .map(|rec| rec.seq)
}

/// The attach's stop signal, with nothing asked of the broker inside the timed window.
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
    /// Each round's census of the attach, the killed bridge's fired will left out.
    censuses: Vec<census::Census>,
    /// Each round's fired wills.
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

/// One population's backlog ([`backlog`], B09): the fill, and the paced
/// publisher's ack latencies inside its attaches and outside them.
struct Backlog {
    sessions: usize,
    fill_ms: f64,
    /// `(inside, outside)` ([`backlog::split`]): inside is a beat that meets
    /// a round's kill -> every session's live mail visible.
    steady: (Vec<f64>, Vec<f64>),
    /// What the broker held in memory once this population's restarts were done (D10).
    held: backlog::Held,
    /// The records the broker read back cold in each restart ([`Round::cold_reads`], D10d).
    cold_per_restart: Vec<u64>,
    /// What the broker's residency tier ran in each restart ([`Round::tier`], D10f).
    tier_per_restart: Vec<Option<astream_broker::TierTally>>,
}

/// One `| … | n | mean | p50 | p95 | p99 | max |` cell run of `values`.
fn with_max(values: &[f64]) -> String {
    match stats::summarize(values) {
        None => "0 | – | – | – | – | –".to_string(),
        Some(s) => format!(
            "{} | {:.2} | {:.2} | {:.2} | {:.2} | {:.2}",
            s.n, s.mean, s.p50, s.p95, s.p99, s.max
        ),
    }
}

/// The attach section's backlog part, pushed onto `md`: its JSON, `null` for a run without one.
fn backlog_report(
    spec: &Spec,
    node: &str,
    backlogs: &[Backlog],
    reopened: &Reopen,
    budget: Option<usize>,
    md: &mut Vec<String>,
) -> Value {
    if spec.backlog == 0 {
        return Value::Null;
    }
    md.push(format!(
        "### The backlog: {} records behind every session's refill watermark (B09)\n",
        spec.backlog
    ));
    md.push(format!(
        "Before each population's restarts every session is sent a note, marks it seen, and the \
         bridge persists its refill watermark there (`seen/<sid>`); then {} records of {} bytes \
         are filled on `{}`, an `ev` face no session owns. Each restart's bridge refills every \
         session from its watermark, so it pages a `Fetch` of each session's lane over the \
         backlog, astream scanning at most {} records per page under the broker's log lock. In \
         the kill window the boot session posts one more note to every other session; \"kill -> \
         every session's live mail visible\" ends when the last of them is listed. A paced \
         publisher on its own connection publishes on `{}` every {} ms across the restarts \
         (the yardstick's kind of bare publish, not aterm's): an ack that meets a round's kill -> \
         every live mail is inside, every other is outside, and the difference is what the \
         attach — the refill's scans above all — costs every other writer. Its records and the \
         other sessions' notes are left out of the census.\n",
        spec.backlog,
        backlog::FILL_BODY_LEN,
        backlog::fill_subject(node),
        astream_broker::broker::FETCH_SCAN_MAX,
        backlog::steady_subject(),
        backlog::STEADY_EVERY.as_millis()
    ));
    md.push(
        "| sessions | fill (ms) | paced publish -> ack (ms) | n | mean | p50 | p95 | p99 | max |"
            .to_string(),
    );
    md.push("|---:|---:|---|---:|---:|---:|---:|---:|---:|".to_string());
    let mut sizes = Vec::new();
    for b in backlogs {
        let (inside, outside) = &b.steady;
        md.push(format!(
            "| {} | {:.0} | inside an attach | {} |",
            b.sessions,
            b.fill_ms,
            with_max(inside)
        ));
        md.push(format!(
            "| {} | | outside | {} |",
            b.sessions,
            with_max(outside)
        ));
        sizes.push(json!({
            "sessions": b.sessions,
            "fill_ms": stats::round3(b.fill_ms),
            "steady_inside_ms": stats::to_json(inside),
            "steady_outside_ms": stats::to_json(outside),
            "held": held_json(&b.held),
            "cold_reads_per_restart": b.cold_per_restart,
            "tier_per_restart": b
                .tier_per_restart
                .iter()
                .map(|t| t.map_or(Value::Null, backlog::tier_json))
                .collect::<Vec<_>>(),
        }));
    }
    md.push(String::new());
    md.push(format!(
        "**A broker's start on this log** ({:.1} MiB, the page cache warm, as an update's or a \
         poisoned log's restart finds it): each open scans every retained record (under a key, \
         authenticates it) and sets the retention rules, what `aterm link broker` does before it \
         binds its socket.\n",
        reopened.bytes as f64 / (1024.0 * 1024.0)
    ));
    md.push("| measure (ms) | n | mean | p50 | p95 | p99 |".to_string());
    md.push("|---|---:|---:|---:|---:|---:|".to_string());
    md.push(stats::row("open the stopped log", &reopened.ms));
    md.push(String::new());
    let budget_words = budget.map_or_else(
        || "none: every retained record resident".to_string(),
        |b| format!("{:.1} MiB", b as f64 / (1024.0 * 1024.0)),
    );
    md.push(format!(
        "**What the broker holds in memory** (D10), its residency budget {budget_words} \
         (astream's `OpenOptions::resident_budget`, as the world's options set it): the retained \
         records it keeps decoded (`Broker::residency`: the records, and the bytes the budget \
         charges them), and this process's physical footprint (`ri_phys_footprint`, with its \
         peak over this process's lifetime), read once a population's restarts are done. The broker \
         runs in this process, so the footprint is the broker's and the bench's own clients' and \
         tables beside it. The last open of the stopped log reads its residency alone: by then \
         this process also holds the census's copy of the whole log.\n"
    ));
    md.push(
        "| read | resident records | resident (MiB) | retained records | footprint (MiB) | peak \
         (MiB) | cold reads since the open |"
            .to_string(),
    );
    md.push("|---|---:|---:|---:|---:|---:|---:|".to_string());
    for b in backlogs {
        md.push(held_row(
            &format!("after {} session(s)' restarts", b.sessions),
            &b.held,
        ));
    }
    if let Some(held) = &reopened.held {
        md.push(held_row("the last open of the stopped log", held));
    }
    md.push(String::new());
    md.push(
        "**What the budget made the broker read back** (D10d): the records it read from its log \
         file because the budget left them cold (`Broker::cold_reads`), from just before each \
         kill to the attach's last row and every live mail, read outside the timed window. With \
         no budget every retained record is resident and each count is 0.\n"
            .to_string(),
    );
    md.push("| sessions | restarts | cold reads per restart | max |".to_string());
    md.push("|---:|---:|---|---:|".to_string());
    for b in backlogs {
        let each: Vec<String> = b.cold_per_restart.iter().map(u64::to_string).collect();
        md.push(format!(
            "| {} | {} | {} | {} |",
            b.sessions,
            b.cold_per_restart.len(),
            each.join(", "),
            b.cold_per_restart.iter().max().copied().unwrap_or(0)
        ));
    }
    md.push(String::new());
    tier_report(backlogs, md);
    json!({
        "records": spec.backlog,
        "body_bytes": backlog::FILL_BODY_LEN,
        "subject": backlog::fill_subject(node),
        "steady_every_ms": u64::try_from(backlog::STEADY_EVERY.as_millis()).unwrap_or(u64::MAX),
        "fetch_scan_max": astream_broker::broker::FETCH_SCAN_MAX,
        "resident_budget": budget,
        "sizes": Value::Array(sizes),
        "reopen": {
            "log_bytes": reopened.bytes,
            "ms": stats::to_json(&reopened.ms),
            "held": reopened.held.as_ref().map_or(Value::Null, held_json),
        },
    })
}

/// `bytes` in MiB to a tenth, `–` for none.
fn mib(bytes: Option<u64>) -> String {
    bytes.map_or_else(
        || "–".to_string(),
        |b| format!("{:.1}", b as f64 / (1024.0 * 1024.0)),
    )
}

/// One `| read | resident records | resident | retained | footprint | peak | cold reads |` row
/// of `held`.
fn held_row(read: &str, held: &backlog::Held) -> String {
    format!(
        "| {read} | {} | {} | {} | {} | {} | {} |",
        held.resident_records,
        mib(u64::try_from(held.resident_bytes).ok()),
        held.retained,
        mib(held.footprint),
        mib(held.peak_footprint),
        held.cold_reads
    )
}

/// The backlog's table of what the broker's residency tier ran in each restart (D10f), pushed
/// onto `md`; a line saying the build counts nothing when no restart was counted. Its columns
/// after the restart, heading and cells alike, are [`backlog::tier_counts`]'s, in that order.
fn tier_report(backlogs: &[Backlog], md: &mut Vec<String>) {
    md.push(
        "**What each restart ran through the broker's residency tier** (D10f): astream's \
         `Broker::tier_tally`, read where the cold reads are. Pushes are the records promoted; \
         lookups the one-record reads by index (a cold read takes two); walks the reads of a \
         range of summaries a page at a time (a filtered read's pass is one, `Tier::passing`, \
         and takes no lookup) and rows the records they read; pages passed the pages a filtered \
         read passed whole, wanting none of their subjects (F11c), and verdicts the times it \
         asked its filter of a subject (at most once a page a subject, and again in a later page \
         once another subject took its slot of 256, so not the backlog's distinct subjects); \
         wide the values read from the tier's maps of values no column holds. The pending wills \
         read no page since F19 (astream's latest-will map), so no column counts them.\n"
            .to_string(),
    );
    if backlogs
        .iter()
        .all(|b| b.tier_per_restart.iter().all(Option::is_none))
    {
        md.push(
            "This build counts nothing (a build without debug assertions keeps no tally).\n"
                .to_string(),
        );
        return;
    }
    let counts = backlog::tier_counts(astream_broker::TierTally::default());
    let mut heading = vec!["sessions", "restart"];
    heading.extend(counts.iter().map(|c| c.column));
    md.push(format!("| {} |", heading.join(" | ")));
    md.push(format!("|{}", "---:|".repeat(heading.len())));
    for b in backlogs {
        for (i, t) in b.tier_per_restart.iter().enumerate() {
            let Some(t) = t else { continue };
            let mut row = vec![b.sessions.to_string(), (i + 1).to_string()];
            row.extend(backlog::tier_counts(*t).iter().map(|c| c.n.to_string()));
            md.push(format!("| {} |", row.join(" | ")));
        }
    }
    md.push(String::new());
}

/// `held` as the report's JSON carries it.
fn held_json(held: &backlog::Held) -> Value {
    json!({
        "resident_records": held.resident_records,
        "resident_bytes": held.resident_bytes,
        "retained_records": held.retained,
        "cold_reads": held.cold_reads,
        "footprint_bytes": held.footprint,
        "peak_footprint_bytes": held.peak_footprint,
    })
}

/// How many series a size's attach rows hold ([`ATTACH_ROWS`]).
const ATTACH_SERIES: usize = 13;

/// The attach table's rows, in the order of a size's series: the last two
/// ([`ATTACH_BACKLOG_ROWS`] on) are the backlog's (B09). The three before them split "every row
/// acked -> the live mail visible": when the new bridge's drain had its first and its last post
/// of the kill window acked, and when the bus mirror's first snapshot after the kill was (critic
/// item G04, the rows F07 named missing).
const ATTACH_ROWS: [&str; ATTACH_SERIES] = [
    "**pid -> every row acked (attach)**",
    "pid -> node row acked",
    "node row -> last session row",
    "kill -> new pid (supervisor relaunch)",
    "kill -> every row acked",
    "pid -> every row durable",
    "**kill -> the first live mail visible**",
    "every row acked -> the live mail visible (the mirror's start, the refill, the drain)",
    "every row acked -> the drain's first post acked (the kill window's)",
    "every row acked -> the drain's last post acked (the kill window's)",
    "every row acked -> the mirror's first send acked (its first snapshot past the kill)",
    "**kill -> every session's live mail visible** (the backlog's: every refill done)",
    "kill -> the kill window's posts answered (the backlog's)",
];

/// The first of the backlog's own rows in [`ATTACH_ROWS`].
const ATTACH_BACKLOG_ROWS: usize = 11;

/// One size's rows of the attach table: every row, an empty one as `0 | – …`
/// (a head that never landed is a hole the table shows), but the backlog's
/// own two on a run without a backlog, which entries before it never had.
fn attach_table_rows(
    size: usize,
    series: &[Vec<f64>; ATTACH_SERIES],
    backlog: usize,
) -> Vec<String> {
    ATTACH_ROWS
        .iter()
        .zip(series)
        .enumerate()
        .filter(|(i, _)| backlog > 0 || *i < ATTACH_BACKLOG_ROWS)
        .map(|(_, (name, values))| format!("| {size} |{}", &stats::row(name, values)[1..]))
        .collect()
}

/// The attach world: grow the population, restart the bridge `rounds` times at each size, census.
fn attach(spec: &Spec, clock: Clock) -> (Value, Vec<String>) {
    eprintln!("fabric bench: booting the attach world");
    let mut w = boot_world("fbench-att", spec, &tallies::world_env(spec, &[]));
    w.wait_ready();
    let served = served_line(&w, spec);
    let first = until("aterm's boot session", || {
        w.sessions().first().map(|(_, sid, _)| sid.clone())
    });
    let node_subject = presence_subject(&w, "node");
    let presence_filter = format!("/f/{FLEET}/pub/{}/*/presence", w.node);
    let ruler = Ruler::for_world(&w);
    let mut cal_seq = 0;
    let cal_before = calibrate(&ruler, &w.node, clock, &mut cal_seq);
    let (rounds, heads) = sampled(&w, clock, || {
        let broker = w.broker.as_ref().expect("the world serves its own broker");
        let mut mail_ctl = w.ctl();
        let mut mail_since = newest_id(&mut mail_ctl, &first);
        let mailer = first.clone();
        let mut sids = vec![first];
        let mut pid = w.bridge_pid();
        // A round is one restart: the restarted bridge's whole life, and aterm's handlers from
        // just before the kill to the attach's last row.
        let mut counted = spec.tally.then(|| tallies::Tallies::of(&w));
        let gui = w.gui.as_ref().expect("the world's aterm").id();
        let mut up_since = Instant::now();
        let mut rounds = Vec::new();
        let mut backlogs = Vec::new();
        let mut steady_seq = 1;
        for (size_index, &size) in spec.attach.iter().enumerate() {
            while sids.len() < size {
                sids.push(spawn(&w));
            }
            wait_live(&w, &sids, 0, "every session's presence row to say live");
            let session_subjects: Vec<String> =
                sids.iter().map(|sid| presence_subject(&w, sid)).collect();
            let mut others_since = HashMap::new();
            let mut fill_ms = None;
            if spec.backlog > 0 {
                eprintln!(
                    "fabric bench: {size} session(s) mark a note seen, then {} records fill the \
                     backlog",
                    spec.backlog
                );
                others_since =
                    backlog::seed_seen(&w, &mut mail_ctl, clock, &mailer, &sids, &size.to_string());
                mail_since = others_since.remove(&mailer).unwrap_or(mail_since);
                fill_ms = Some(backlog::fill(
                    &w,
                    backlog::FILL_PRODUCER + size_index as u64,
                    spec.backlog,
                ));
            }
            eprintln!(
                "fabric bench: attach with {size} session(s), {} restart(s)",
                spec.rounds
            );
            let stop_steady = AtomicBool::new(false);
            let size_rounds = std::thread::scope(|s| {
                let steady = (spec.backlog > 0)
                    .then(|| s.spawn(|| backlog::steady(&w, clock, steady_seq, &stop_steady)));
                let mut size_rounds = Vec::new();
                for _ in 0..spec.rounds {
                    if let Some(rest) = SUPERVISOR_HEALTHY.checked_sub(up_since.elapsed()) {
                        std::thread::sleep(rest);
                    }
                    if let Some(t) = counted.as_mut() {
                        let at = tallies::At {
                            bridge: pid,
                            through: 0,
                            sid: &mailer,
                            since: mail_since,
                        };
                        let now = t.read(&w, &mut mail_ctl, gui, &at);
                        t.rebase(now);
                        t.free_wait();
                    }
                    let cold_before = broker.cold_reads();
                    let tier_before = backlog::tier_tally(broker);
                    let mark = broker.visible_head();
                    let mut watch = w
                        .god()
                        .subscribe(mark, &presence_filter)
                        .unwrap_or_else(|e| {
                            panic!("subscribe to {presence_filter} from {mark}: {e}")
                        });
                    watch
                        .set_read_timeout(Some(DEADLINE))
                        .expect("bound the presence subscription");
                    let t_kill = clock.us();
                    harness::kill(pid, 9);
                    let new_pid = {
                        let (world, killed) = (&w, pid);
                        s.spawn(move || {
                            let new_pid = aterm_spec::wait::within(DEADLINE, PID_POLL, || {
                                read_pid(world).filter(|p| *p != killed)
                            });
                            (new_pid, clock.us())
                        })
                    };
                    let mail = format!("live-{}", rounds.len() + size_rounds.len());
                    post(&mut mail_ctl, clock, &mailer, &mailer, &mail);
                    if spec.backlog > 0 {
                        for (i, sid) in sids.iter().enumerate().skip(1) {
                            post(
                                &mut mail_ctl,
                                clock,
                                &mailer,
                                sid,
                                &format!("{mail}-to-{i}"),
                            );
                        }
                    }
                    let t_posted = clock.us();
                    let (new_pid, t_pid) = new_pid.join().expect("the new pid's watcher");
                    let new_pid = new_pid.unwrap_or_else(|| {
                        panic!("the bridge did not come back within {DEADLINE:?}")
                    });
                    pid = new_pid;
                    up_since = Instant::now();
                    let (woke_at, t_mail_seen) =
                        await_note(&mut mail_ctl, clock, &mailer, mail_since)
                            .unwrap_or_else(|e| panic!("the live mail `{mail}`: {e}"));
                    let listed = list_since(&mut mail_ctl, &mailer, mail_since);
                    assert!(
                        listed.len() == 1 && listed[0].text == mail && listed[0].id == woke_at,
                        "the live mail `{mail}`: woke at row {woke_at}, listed {listed:?}"
                    );
                    mail_since = woke_at;
                    let mail_off = listed[0].off;
                    let t_every_mail_seen = (spec.backlog > 0).then(|| {
                        for (i, sid) in sids.iter().enumerate().skip(1) {
                            let text = format!("{mail}-to-{i}");
                            let since = others_since.get(sid).copied().unwrap_or(0);
                            let (id, _) = await_note(&mut mail_ctl, clock, sid, since)
                                .unwrap_or_else(|e| panic!("the live mail `{text}`: {e}"));
                            let listed = list_since(&mut mail_ctl, sid, since);
                            assert!(
                                listed.len() == 1 && listed[0].text == text && listed[0].id == id,
                                "the live mail `{text}`: woke at row {id}, listed {listed:?}"
                            );
                            others_since.insert(sid.clone(), id);
                        }
                        clock.us()
                    });
                    await_attach(&mut watch, &node_subject, &session_subjects, new_pid);
                    drop(watch);
                    let cold_reads = broker.cold_reads().saturating_sub(cold_before);
                    let tier = backlog::tier_tally(broker)
                        .zip(tier_before)
                        .map(|(now, before)| now.since(before));
                    if let Some(t) = counted.as_mut() {
                        let at = tallies::At {
                            bridge: new_pid,
                            through: mail_off,
                            sid: &mailer,
                            since: mail_since,
                        };
                        let now = t.read(&w, &mut mail_ctl, gui, &at);
                        let restart = format!("restart {}", size_rounds.len() + 1);
                        t.round(&format!("attach, {size} session(s)"), restart, now);
                    }
                    size_rounds.push(Round {
                        sessions: size,
                        mark,
                        pid: new_pid,
                        t_kill,
                        t_pid,
                        sids: sids.clone(),
                        mail,
                        t_mail_seen,
                        t_every_mail_seen,
                        t_posted,
                        cold_reads,
                        tier,
                    });
                }
                stop_steady.store(true, std::sync::atomic::Ordering::Relaxed);
                let beats = steady
                    .map(|h| h.join().expect("the paced publisher"))
                    .unwrap_or_default();
                (size_rounds, beats)
            });
            let (size_rounds, beats) = size_rounds;
            steady_seq += beats.len() as u64;
            if let Some(fill_ms) = fill_ms {
                let windows: Vec<(u64, u64)> = size_rounds
                    .iter()
                    .map(|r| (r.t_kill, r.t_every_mail_seen.unwrap_or(r.t_mail_seen)))
                    .collect();
                backlogs.push(Backlog {
                    sessions: size,
                    fill_ms,
                    steady: backlog::split(&beats, &windows),
                    held: backlog::held(broker),
                    cold_per_restart: size_rounds.iter().map(|r| r.cold_reads).collect(),
                    tier_per_restart: size_rounds.iter().map(|r| r.tier).collect(),
                });
            }
            rounds.extend(size_rounds);
        }
        await_durable(broker);
        (rounds, backlogs, counted)
    });
    let (rounds, backlogs, counted) = rounds;
    let cal_after = calibrate(&ruler, &w.node, clock, &mut cal_seq);
    let unit = yardstick(&cal_before, &cal_after);
    let (recs, reopened) = stop_read_then(&mut w, |w| {
        if spec.backlog > 0 {
            reopen(w)
        } else {
            Reopen {
                bytes: 0,
                ms: Vec::new(),
                held: None,
            }
        }
    });
    let fired = census::will_firings(&recs);

    let mut by_size: BTreeMap<usize, Vec<Value>> = BTreeMap::new();
    let mut series: BTreeMap<usize, [Vec<f64>; ATTACH_SERIES]> = BTreeMap::new();
    let steady_subject = backlog::steady_subject();
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
        let t_node = heads.acked.landed(found.node);
        let t_all = heads.acked.landed(last);
        let t_last_session = found.last_session().and_then(|s| heads.acked.landed(s));
        let t_all_durable = heads.durable.landed(last);
        let mail_lane = format!("/f/{FLEET}/in/{}/{}/{}/note", w.node, r.sids[0], w.node);
        let mut mails = recs
            .iter()
            .filter(|rec| rec.subject == mail_lane && rec.text == r.mail);
        let mail_seq = mails
            .next()
            .unwrap_or_else(|| panic!("no record of the live mail `{}` on {mail_lane}", r.mail))
            .seq;
        assert!(
            mails.next().is_none(),
            "the live mail `{}` is on {mail_lane} twice",
            r.mail
        );
        assert!(
            mail_seq > found.node,
            "the live mail `{}` landed at offset {mail_seq}, before the new bridge's node row at \
             {}: it is not the new bridge's",
            r.mail,
            found.node
        );
        let others = format!("{}-to-", r.mail);
        let in_window = || {
            census::window(&recs, r.mark, last).filter(|rec| {
                rec.seq != mail_seq
                    && !rec.text.starts_with(&others)
                    && rec.subject != steady_subject
            })
        };
        let c = census::census(in_window().filter(|rec| !fired.contains(&rec.seq)));
        let fired_here = in_window().filter(|rec| fired.contains(&rec.seq)).count();
        let s = series.entry(r.sessions).or_default();
        s[0].extend(ms_opt(Some(r.t_pid), t_all));
        s[1].extend(ms_opt(Some(r.t_pid), t_node));
        s[2].extend(ms_opt(t_node, t_last_session));
        s[3].push(ms(r.t_kill, r.t_pid));
        s[4].extend(ms_opt(Some(r.t_kill), t_all));
        s[5].extend(ms_opt(Some(r.t_pid), t_all_durable));
        s[6].push(ms(r.t_kill, r.t_mail_seen));
        s[7].extend(ms_opt(t_all, Some(r.t_mail_seen)));
        let drained = kill_window_posts(&recs, r, &w.node);
        let mirror_first = mirror_first_send(&recs, r.mark, &w.node);
        let landed = |off: Option<u64>| off.and_then(|off| heads.acked.landed(off));
        s[8].extend(ms_opt(t_all, landed(drained.map(|(first, _)| first))));
        s[9].extend(ms_opt(t_all, landed(drained.map(|(_, last)| last))));
        s[10].extend(ms_opt(t_all, landed(mirror_first)));
        s[11].extend(ms_opt(Some(r.t_kill), r.t_every_mail_seen));
        if r.t_every_mail_seen.is_some() {
            s[12].push(ms(r.t_kill, r.t_posted));
        }
        by_size.entry(r.sessions).or_default().push(json!({
            "mark": r.mark,
            "pid": r.pid,
            "node_offset": found.node,
            "last_offset": last,
            "live_mail_offset": mail_seq,
            "drain_first_and_last_offsets": drained
                .map_or(Value::Null, |(first, last)| json!([first, last])),
            "mirror_first_offset": mirror_first.map_or(Value::Null, |off| json!(off)),
            "records": c.total,
            "fired_wills": fired_here,
            "kill_to_pid_ms": stats::round3(ms(r.t_kill, r.t_pid)),
            "pid_to_all_ms": ms_opt(Some(r.t_pid), t_all).map(stats::round3),
            "kill_to_live_mail_ms": stats::round3(ms(r.t_kill, r.t_mail_seen)),
            "kill_to_every_live_mail_ms": ms_opt(Some(r.t_kill), r.t_every_mail_seen).map(stats::round3),
            "cold_reads": r.cold_reads,
            "tier": r.tier.map_or(Value::Null, backlog::tier_json),
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
        served.clone(),
        format!(
            "The bridge is SIGKILLed after a run past the supervisor's healthy mark, and the \
             supervisor relaunches it at once, the killed bridge's lanes left to end; the \
             clock starts when the new bridge writes its pid (its aterm connection is up; a \
             thread of its own watches for it from the kill on, past any post in the kill window) \
             and stops when its node \
             row and every session's presence row — the rows under its own pid and incarnation, \
             whichever lands last — are acked again (on a strict broker, durable; \"pid -> every \
             row durable\" is the durable head's). Inside that window the bench asks the \
             broker nothing: it holds one subscription to the node's presence rows, opened before \
             the kill, and reads the times off the log afterwards. \"node row -> last session \
             row\" is negative for a bridge that publishes a session's row before its node's. \
             THE SECOND STOP, the live mail: the moment the bridge is killed the boot session \
             posts a note to itself, which aterm holds in its outbox until the new bridge's live \
             drain publishes it, after the presence rows, the bus mirror's start (over a \
             retaining broker at `--log auto`) and the refill the rows owe; \"kill -> the first \
             live mail visible\" ends when its `await inbox` answers, and its record is \
             left out of the attach's census. Read off the log the same way: \"the drain's first \
             post acked\" and \"last post acked\" are when the acked head first passed the first and \
             the last of the kill window's posts, and \"the mirror's first send acked\" when it \
             passed the mirror's first snapshot (a `v=2` row on a session's `ev` face) at or \
             past the kill, each from every row acked (critic item G04). {} restart(s) per size.\n",
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
    for (size, s) in &series {
        md.extend(attach_table_rows(*size, s, spec.backlog));
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
            "attach_durable_ms": stats::to_json(&s[5]),
            "kill_to_live_mail_ms": stats::to_json(&s[6]),
            "attach_to_live_mail_ms": stats::to_json(&s[7]),
            "attach_to_first_post_acked_ms": stats::to_json(&s[8]),
            "attach_to_last_post_acked_ms": stats::to_json(&s[9]),
            "attach_to_mirror_first_acked_ms": stats::to_json(&s[10]),
            "kill_to_every_live_mail_ms": stats::to_json(&s[11]),
            "kill_to_posted_ms": stats::to_json(&s[12]),
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
    let backlog_json = backlog_report(
        spec,
        &w.node,
        &backlogs,
        &reopened,
        w.opts.resident_budget,
        &mut md,
    );
    md.extend(yardstick_rows(&ruler, &cal_before, &cal_after));
    md.extend(normalized);
    md.push(String::new());
    let mut json = json!({
        "node": w.node.clone(),
        "served": served.trim_end(),
        "head_changes_sampled": heads.acked.len(),
        "durable_head_changes_sampled": heads.durable.len(),
        "yardstick": yardstick_json(&ruler, &cal_before, &cal_after, unit),
        "sizes": Value::Array(sizes),
        "backlog": backlog_json,
    });
    tallies::report(counted.as_ref(), &mut json, &mut md);
    drop(ruler);
    (json, md)
}

/// What the run's broker promised, for the report's head and its JSON.
fn broker_note(spec: &Spec) -> String {
    match spec.durability {
        Tier::Strict => "astream Broker::open_opts with aterm's strict options (Durability::Strict: \
                         every ack after a full flush, so acked and durable are one)"
            .to_string(),
        Tier::Ordered => format!(
            "astream Broker::open_opts with aterm's ordered options (Durability::Ordered at a {} \
             ms horizon, aterm's four-rule escalation table): every ack after an ordering barrier \
             (F_BARRIERFSYNC), a full flush once the horizon has passed since the first ack no \
             full flush covers (at most the horizon plus a batch and a flush after it) and \
             before the ack of every \
             escalated record. \"Acked\" below is the barrier's; \"durable\" the full flush's. \
             The yardstick's unit is still one Strict publish, on a Strict broker of its own.",
            spec.horizon_ms.unwrap_or(ORDERED_HORIZON_MS)
        ),
        Tier::Relaxed => "astream Broker::open_opts with aterm's relaxed options \
                          (Durability::Relaxed, aterm's escalation table): every ack after the \
                          batch's page-cache write, NO flush. The ceiling arm, not what aterm runs \
                          by default: \"acked\" below means in the page cache, and no record of the \
                          run is ever durable. The yardstick's unit is still one Strict publish, on \
                          a Strict broker of its own."
            .to_string(),
    }
}

/// What the run's broker retains and what the bridge mirrors, for the report's head beside [`broker_note`].
fn retention_note(spec: &Spec) -> String {
    let rules = if spec.retention {
        "retained by aterm's default rules, set at every open \
         (`aterm_link::broker_rules::retain`, what `aterm link broker` does)"
    } else {
        "NOT retained (no rules: what `aterm link broker` serves when its rules cannot be set, \
         and every broker before astream's R4)"
    };
    let tick = match (spec.retention, spec.retention_interval_ms) {
        (false, _) => String::new(),
        (true, None) => ", its retention tick at astream's minute (what ships)".to_string(),
        (true, Some(ms)) => format!(
            ", its retention tick every {ms} ms (`Broker::set_retention_interval`; aterm ships \
             astream's minute)"
        ),
    };
    let mirror = match (spec.log, spec.retention) {
        (Log::Auto, true) => {
            "`--log auto` (what ships: the bus mirror is on over a retaining broker)"
        }
        (Log::Auto, false) => {
            "`--log auto`, which mirrors nothing over a broker that retains nothing"
        }
        (Log::Off, _) => "`--log off` (no bus mirror)",
    };
    // An arm whose broker runs in a process of its own boots no yardstick.
    let at_rest = match (spec.at_rest, spec.broker_process.outside()) {
        (true, false) => {
            " **Sealed at rest:** the log and its `.state` and `.held` under a key, its `.seal` \
             binding required (what ships: what `aterm link broker` opens with a `rest.key` beside \
             it); the yardstick's brokers are not."
        }
        (true, true) => {
            " **Sealed at rest:** the log and its `.state` and `.held` under a key, its `.seal` \
             binding required (what ships: what `aterm link broker` opens with a `rest.key` beside \
             it)."
        }
        (false, _) => {
            " **Not sealed at rest** (`--at-rest off`: a plaintext log, which a fabric root no \
             longer ships)."
        }
    };
    format!("**Retention:** the log {rules}{tick}; the bridge runs {mirror}.{at_rest}")
}

/// What the run's broker limits, for the report's head beside [`retention_note`] (critic item E06).
fn rate_limit_note(spec: &Spec) -> String {
    if spec.rate_limit {
        // A broker in a process of its own keeps its `BrokerStats` there (`run/outside.rs`).
        let counted = if spec.broker_process.outside() {
            "no window counts the publishes it refused for the rate: its `BrokerStats` stay in its \
             own process"
        } else {
            "each window counts the publishes it refused for the rate (`BrokerStats::rate_limited`)"
        };
        format!(
            "**Rate limit:** every world's broker limits each producer by aterm's default ({}; \
             `aterm_link::broker_rules::limit`, what `aterm link broker` sets at every open); \
             {counted}.",
            aterm_link::broker_rules::rate_summary()
        )
    } else {
        "**No rate limit** (`--rate-limit off`: every world's broker limits no producer, as no \
         `aterm link broker` before critic item E06 did)."
            .to_string()
    }
}

/// Run only the selected scenario.
fn measure_scenario(
    selected: Scenario,
    scenario: Scenario,
    measure: impl FnOnce() -> (Value, Vec<String>),
) -> (Value, Vec<String>) {
    if selected == Scenario::All || selected == scenario {
        measure()
    } else {
        (Value::Null, Vec::new())
    }
}

/// Measure everything `spec` names.
#[must_use]
pub fn measure(spec: &Spec) -> Report {
    let binary_at_start = binary_identity();
    let clock = Clock::start();
    let started = Instant::now();
    let outside = spec.broker_process.outside();
    let (messaging, messaging_md) = measure_scenario(spec.scenario, Scenario::Messaging, || {
        if outside {
            outside::messaging(spec, clock)
        } else {
            messaging(spec, clock)
        }
    });
    let (pingpong, pingpong_md) = measure_scenario(spec.scenario, Scenario::Pingpong, || {
        if outside {
            outside::pingpong(spec, clock)
        } else {
            pingpong::measure(spec, clock)
        }
    });
    let (attach, attach_md) = measure_scenario(spec.scenario, Scenario::Attach, || {
        if outside {
            outside::attach(spec, clock)
        } else {
            attach(spec, clock)
        }
    });
    // The worlds only an in-process broker serves: with a broker of its own `spec::parse`
    // refuses them by name, and `all` is the three above.
    let inproc_only = |scenario: Scenario, measure: &dyn Fn() -> (Value, Vec<String>)| {
        if outside {
            (Value::Null, Vec::new())
        } else {
            measure_scenario(spec.scenario, scenario, measure)
        }
    };
    let (handoff, handoff_md) = inproc_only(Scenario::Handoff, &|| handoff::measure(spec, clock));
    let (asks, asks_md) = inproc_only(Scenario::Ask, &|| ask::measure(spec, clock));
    let (cross, cross_md) = inproc_only(Scenario::Ask, &|| cross::measure(spec, clock));
    let broker = if outside {
        outside::note(spec)
    } else {
        broker_note(spec)
    };
    let mut md = vec![
        format!("**Broker:** {broker}"),
        String::new(),
        retention_note(spec),
        String::new(),
        rate_limit_note(spec),
        String::new(),
    ];
    if spec.scenario != Scenario::All {
        md.push(format!(
            "**Scenario:** {} only; the other scenarios were not run.",
            spec.scenario.word()
        ));
        md.push(String::new());
    }
    md.extend(messaging_md);
    md.extend(pingpong_md);
    md.extend(attach_md);
    md.extend(handoff_md);
    md.extend(asks_md);
    md.extend(cross_md);
    let elapsed_s = started.elapsed().as_secs_f64();
    let binary_at_end = binary_identity();
    let arm = Profile::of_binary(Path::new(harness::aterm()));
    let json = json!({
        "schema": "aterm-fabric-bench-v1",
        "spec": spec.words(),
        "scenario": spec.scenario.word(),
        "profile": arm.map_or("unknown", Profile::name),
        "build": arm.map_or("a binary in neither arm's build directory", Profile::describe),
        "aterm_binary": harness::aterm(),
        "aterm_binary_identity": { "start": binary_at_start, "end": binary_at_end },
        "broker": broker,
        "retention": retention_note(spec),
        "rate_limit": rate_limit_note(spec),
        "clock": "one monotonic origin in the bench process; microsecond stamps",
        "elapsed_s": stats::round3(elapsed_s),
        "messaging": messaging,
        "pingpong": pingpong,
        "attach": attach,
        "handoff": handoff,
        "ask": asks,
        "cross_node": cross,
    });
    Report {
        json,
        markdown: md.join("\n"),
    }
}

/// [`profile::identity`] of the harness's `aterm`, or why it could not be read, which the lane takes as no identity.
fn binary_identity() -> String {
    profile::identity(Path::new(harness::aterm()))
        .unwrap_or_else(|why| format!("unreadable: {why}"))
}

#[cfg(test)]
mod tests {
    /// The report's head claims only what the arm measured: an arm whose broker runs in a
    /// process of its own counts no refusal for the rate (its `BrokerStats` are not the bench's
    /// to read) and boots no yardstick, while the in-process arm does both.
    #[cfg(target_os = "macos")]
    #[test]
    fn the_head_of_a_broker_process_arm_claims_no_refusal_count_and_no_yardstick() {
        use super::*;

        let inproc = crate::spec::parse("scenario=messaging", Path::new("/s")).expect("inproc");
        assert!(rate_limit_note(&inproc).contains("BrokerStats::rate_limited"));
        assert!(retention_note(&inproc).contains("yardstick"));
        for arm in ["background", "interactive"] {
            let words = format!("scenario=messaging broker_process={arm}");
            let outside = crate::spec::parse(&words, Path::new("/s")).expect("outside");
            let (rate, retention) = (rate_limit_note(&outside), retention_note(&outside));
            assert!(
                !rate.contains("BrokerStats::rate_limited") && rate.contains("no window counts"),
                "{arm}: {rate}"
            );
            assert!(!retention.contains("yardstick"), "{arm}: {retention}");
        }
    }

    /// The attach table keeps every row a run measured, an empty one too (a
    /// relaxed run's `pid -> every row durable`, an acked head that never
    /// landed), and leaves out only the backlog's own two rows on a run
    /// without a backlog.
    #[test]
    fn the_attach_table_keeps_an_empty_row_and_names_the_backlogs_only_with_one() {
        use super::*;

        let mut series: [Vec<f64>; ATTACH_SERIES] = Default::default();
        for (i, s) in series.iter_mut().enumerate() {
            if i != 5 && i < ATTACH_BACKLOG_ROWS {
                s.push(1.0);
            }
        }
        let bare = attach_table_rows(8, &series, 0);
        assert_eq!(bare.len(), ATTACH_BACKLOG_ROWS, "{bare:#?}");
        assert_eq!(
            bare[5], "| 8 | pid -> every row durable | 0 | – | – | – | – |",
            "the empty row is a hole the table shows"
        );
        assert!(bare.iter().all(|row| !row.contains("the backlog's")));
        let backlog = attach_table_rows(8, &series, 1);
        assert_eq!(backlog.len(), ATTACH_ROWS.len(), "{backlog:#?}");
        assert!(
            backlog[ATTACH_BACKLOG_ROWS..]
                .iter()
                .all(|row| row.contains("the backlog's") && row.contains("| 0 |"))
        );
    }

    /// The drain's rows read the kill window's posts alone, at or past the round's mark: the
    /// live mail and every `<mail>-to-<i>` note, never another round's mail (`live-1` beside
    /// `live-10`) or a record before the kill; the mirror's row reads the first `v=2` row on a
    /// session's `ev` face at or past the mark, never the node's own `ev` face or an `ev`
    /// event's `v=1` row.
    #[test]
    fn the_drains_and_the_mirrors_rows_read_the_kill_windows_records_alone() {
        use super::*;

        let node = "n-bench";
        let rec = |seq: u64, subject: String, text: &str, head: &str| Rec {
            seq,
            producer: (1, seq),
            subject,
            text: text.to_string(),
            head: head.to_string(),
            commit: None,
            bytes: 0,
        };
        let note = |sid: &str| format!("/f/{FLEET}/in/{node}/{sid}/{node}/note");
        let ev = |sid: &str| format!("/f/{FLEET}/pub/{node}/{sid}/ev");
        let recs = vec![
            rec(3, note("s-a"), "live-1", "v=1"),
            rec(4, ev("s-a"), "", "v=2 t=1 ep=1 seq=1"),
            rec(10, note("s-a"), "live-10", "v=1"),
            rec(11, format!("/f/{FLEET}/pub/{node}/ev"), "", "v=2 t=2"),
            rec(12, ev("s-b"), "", "v=1 t=2 ev=log-refused"),
            rec(13, note("s-b"), "live-1-to-1", "v=1"),
            rec(14, ev("s-b"), "", "v=2 t=3 ep=1 seq=4"),
            rec(15, note("s-a"), "live-1", "v=1"),
            rec(17, note("s-c"), "live-1-to-2", "v=1"),
        ];
        let round = Round {
            sessions: 3,
            mark: 5,
            pid: 1,
            t_kill: 0,
            t_pid: 0,
            sids: vec!["s-a".into(), "s-b".into(), "s-c".into()],
            mail: "live-1".to_string(),
            t_mail_seen: 0,
            t_every_mail_seen: None,
            t_posted: 0,
            cold_reads: 0,
            tier: None,
        };
        assert_eq!(kill_window_posts(&recs, &round, node), Some((13, 17)));
        assert_eq!(mirror_first_send(&recs, round.mark, node), Some(14));
        assert_eq!(mirror_first_send(&recs, 0, node), Some(4));
        assert_eq!(
            kill_window_posts(&recs, &Round { mark: 18, ..round }, node),
            None
        );
    }

    /// The wait for a commit takes the log lock only when the lock-free head moved: the
    /// head reads 5, 5, 5, 6, 6, 7, 7, 8, and the group resumes past offset 6 once the head
    /// is at 8 (the commit at 7 visible). Eight polls, and four reads of the group's place,
    /// one at each head.
    #[test]
    fn the_commit_wait_reads_the_groups_place_only_when_the_head_moved() {
        use super::*;
        use std::cell::Cell;

        let heads = [5, 5, 5, 6, 6, 7, 7, 8];
        let polls = Cell::new(0);
        let starts = Cell::new(Vec::new());
        let head = || {
            let h = heads[polls.get()];
            polls.set(polls.get() + 1);
            h
        };
        let start = || {
            let at = heads[polls.get() - 1];
            let mut seen = starts.take();
            seen.push(at);
            starts.set(seen);
            if at >= 8 { 7 } else { 6 }
        };
        let mut probe = commit_counted(6, head, start);
        let answers: Vec<Option<()>> = (0..heads.len()).map(|_| probe()).collect();
        assert_eq!(
            answers,
            [None, None, None, None, None, None, None, Some(())],
            "the commit counts at the last poll, and not before"
        );
        drop(probe);
        assert_eq!(
            starts.take(),
            [5, 6, 7, 8],
            "one read of the group's place per head"
        );
    }

    #[test]
    fn only_selected_scenarios_are_measured() {
        use super::*;
        use std::cell::Cell;

        for phase in [
            Scenario::Messaging,
            Scenario::Attach,
            Scenario::Handoff,
            Scenario::Ask,
        ] {
            let (json, markdown) = measure_scenario(Scenario::Pingpong, phase, || {
                panic!("pingpong-only started {}", phase.word());
            });
            assert!(json.is_null());
            assert!(markdown.is_empty());
        }
        let calls = Cell::new(0);
        let (json, markdown) = measure_scenario(Scenario::Pingpong, Scenario::Pingpong, || {
            calls.set(calls.get() + 1);
            (
                json!({"measured": "pingpong"}),
                vec!["pingpong".to_string()],
            )
        });
        assert_eq!(calls.get(), 1);
        assert_eq!(json["measured"].as_str(), Some("pingpong"));
        assert_eq!(markdown, ["pingpong"]);

        calls.set(0);
        for phase in [
            Scenario::Messaging,
            Scenario::Pingpong,
            Scenario::Attach,
            Scenario::Handoff,
            Scenario::Ask,
        ] {
            let (json, markdown) = measure_scenario(Scenario::All, phase, || {
                calls.set(calls.get() + 1);
                (
                    json!({"measured": phase.word()}),
                    vec![phase.word().to_string()],
                )
            });
            assert_eq!(json["measured"].as_str(), Some(phase.word()));
            assert_eq!(markdown, [phase.word()]);
        }
        assert_eq!(calls.get(), 5);
    }

    #[test]
    fn each_arm_opens_its_broker_with_aterms_options() {
        use super::*;
        use aterm_link::durability::{Durability as Aterm, escalation};
        assert_eq!(
            Duration::from_millis(ORDERED_HORIZON_MS),
            aterm_link::durability::ORDERED_HORIZON
        );
        let spec =
            |words: &str| crate::spec::parse(words, std::path::Path::new("/s")).expect(words);
        let bare = opts_for(&spec(""));
        let shipped = harness::broker_opts(aterm_link::durability::default_for(true));
        assert_eq!(
            (&bare.durability, &bare.escalation),
            (&shipped.durability, &shipped.escalation),
            "the bench's default arm is aterm's default for its guarded broker"
        );
        let strict = opts_for(&spec("durability=strict"));
        assert_eq!(strict.durability, Durability::Strict);
        assert!(strict.escalation.is_empty());
        assert_eq!(strict.guard.as_deref(), Some(harness::SECRET));
        let ordered = opts_for(&spec("durability=ordered"));
        assert_eq!(ordered.durability, Aterm::Ordered.tier());
        assert_eq!(ordered.escalation, escalation());
        let slow = opts_for(&spec("durability=ordered horizon_ms=1000"));
        assert_eq!(
            slow.durability,
            Durability::Ordered {
                horizon: Some(Duration::from_millis(1000))
            }
        );
        assert_eq!(slow.escalation, escalation(), "the horizon only");
        let relaxed = opts_for(&spec("durability=relaxed"));
        assert_eq!(relaxed.durability, Durability::Relaxed);
        assert_eq!(relaxed.escalation, escalation());
    }

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
        assert_eq!(
            tally(&[seen(0, 10), seen(1, 11), seen(1, 11)], 2),
            Err(Miscount::Twice {
                j: 1,
                first: 11,
                again: 11
            })
        );
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

        assert!(may_post(2, 0, 3));
        assert!(!may_post(3, 0, 3));
        assert!(may_post(3, 1, 3));
        let credit = Credit::new(2);
        assert!(credit.acquire(0) && credit.acquire(1));
        std::thread::scope(|s| {
            s.spawn(|| credit.listed(1));
            assert!(credit.acquire(2));
        });
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
        let node_last = vec![
            (20, sa.as_str(), "v=1 inc=6 state=live"),
            (21, node, "v=1 state=live inc=6 pid=80"),
        ];
        let found = attach_rows(&node_last, node, std::slice::from_ref(&sa), 80).expect("complete");
        assert_eq!((found.last(), found.last_session()), (21, Some(20)));
    }

    /// A paced note's shape names its records in the visible head's steps: a record ahead of
    /// the delivery only when it landed after the post call (the one before is the previous
    /// note's), the commit's step with the recipient's snapshot in it, and, after `||`, what
    /// landed past that step before the next post call, and nothing after it.
    #[test]
    fn a_paced_notes_shape_is_its_records_by_the_heads_steps() {
        use super::*;

        let rec = |seq: u64, subject: &str, commit: bool| Rec {
            seq,
            producer: (1, seq),
            subject: subject.to_string(),
            text: String::new(),
            head: String::new(),
            commit: commit.then(|| ("/f/f1/cur/n-x/node/inbox".to_string(), 10)),
            bytes: 0,
        };
        let recs = [
            rec(9, "/f/f1/pub/n-x/s-a/ev", false),
            rec(10, "/f/f1/in/n-x/s-b", false),
            rec(11, "/f/f1/pub/n-x/s-a/ev", false),
            rec(12, "/a/commit", true),
            rec(13, "/f/f1/pub/n-x/s-b/ev", false),
            rec(14, "/f/f1/pub/n-x/s-b/presence", false),
            rec(15, "/f/f1/pub/n-x/s-a/ev", false),
        ];
        // When the visible head passed each offset: 9 before the post call at 100, 12 and 13
        // in one step, 14 at 500, 15 at 1200.
        let landed = |seq: u64| match seq {
            9 => Some(90),
            10 => Some(120),
            11 => Some(150),
            12 | 13 => Some(300),
            14 => Some(500),
            15 => Some(1200),
            _ => None,
        };
        let shape = paced_shape(&recs, &landed, ("s-a", "s-b"), (10, 13, 100), 0, 1000);
        assert_eq!(shape, "note | evA | commit+evB || presence");
        let alone = paced_shape(&recs, &landed, ("s-a", "s-b"), (10, 13, 100), 0, 400);
        assert_eq!(
            alone, "note | evA | commit+evB",
            "nothing past the step landed by then"
        );
    }
}
