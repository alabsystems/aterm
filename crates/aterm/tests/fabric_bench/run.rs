// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The measured phases, each selected scenario over its own world, booted as the fabric suite boots it (`harness::World`: a guarded broker in this process, opened with aterm's own options for the arm's tier — ordered at aterm's horizon by default — and retained by aterm's default rules as `aterm link broker` retains it, a headless `aterm`, and the real `aterm link serve` child it launched, its bus mirror at `--log auto`; [`boot_world`] takes an arm's `retention=off`, `log=off` and tick interval):

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
use crate::spec::{Log, ORDERED_HORIZON_MS, Scenario, Spec, Tier};
use crate::stats;

#[path = "run/pingpong.rs"]
mod pingpong;

#[path = "run/handoff.rs"]
mod handoff;

/// What a run hands back.
pub struct Report {
    pub json: Value,
    pub markdown: String,
}

/// How long the bench lets a bridge run before it kills it again.
const SUPERVISOR_HEALTHY: Duration = Duration::from_millis(5_500);

/// How often the pid file is read while a killed bridge comes back: it marks
/// the start of an attach, so it is read finely.
const PID_POLL: Duration = Duration::from_millis(1);

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
/// # Errors
///
/// Any answer but a row — `OK timeout` after the hang detector — as the
/// request and its reply.
fn await_note(ctl: &mut Ctl, clock: Clock, sid: &str, since: u64) -> Result<(u64, u64), String> {
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

/// SPAWN `n` IDLE BYSTANDER SESSIONS ([`Spec::bystanders`]): sessions that never
/// post or read, beside the two that talk.
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
const BURST_WINDOW: usize = 56;

/// aterm's per-peer allowance of unlisted rows (`aterm-gui` `SENDER_QUOTA`,
/// crate-private there), which [`BURST_WINDOW`] stays under.
const RECIPIENT_ALLOWANCE: usize = 64;
const _: () = assert!(BURST_WINDOW < RECIPIENT_ALLOWANCE);

/// Whether post `posted` (0-based) may go out with `listed` messages listed.
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
    w.stop().unwrap_or_else(|why| {
        panic!(
            "fabric world cleanup incomplete: {why}; kept {}",
            w.tmp.display()
        )
    });
    recs
}

/// Run `phases` beside a head sampler on `w`'s broker: what they answered, and
/// both heads over the run ([`Heads`]).
fn sampled<T: Send>(w: &World, clock: Clock, phases: impl FnOnce() -> T + Send) -> (T, Heads) {
    let broker = w.broker.as_ref().expect("the world serves its own broker");
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

/// THE LAST RECORDS, SEEN DURABLE.
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

/// The flushes a window cost: `after - before`, field by field, of
/// [`Broker::flush_counts`] read at its two ends (under the log lock, so each
/// count is between two whole batches; never inside a timed round trip).
fn flushes_between(before: FlushCounts, after: FlushCounts) -> FlushCounts {
    FlushCounts {
        barrier: after.barrier - before.barrier,
        full: after.full - before.full,
        skipped: after.skipped - before.skipped,
        failed: after.failed - before.failed,
    }
}

/// A window's flushes for the report's JSON: the counts and each per delivered
/// message.
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

/// A window's flushes for the report's tables: what a strict broker spends one
/// full flush on per batch, an ordered one spends a barrier on, plus a full
/// flush per escalated batch and per horizon; a relaxed one spends nothing.
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

/// The broker's RETENTION TICKS so far: how many it ran and how many it
/// skipped because a compaction was under way (`BrokerStats::retention_ticks`
/// and `retention_skips`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct Ticks {
    run: u64,
    skipped: u64,
}

/// The broker's ticks now: read through `Broker::stats`, under the log lock,
/// so never inside a timed round trip.
fn ticks(broker: &Broker) -> Ticks {
    let stats = broker.stats();
    Ticks {
        run: stats.retention_ticks,
        skipped: stats.retention_skips,
    }
}

/// The ticks a window held: `after - before`.
fn ticks_between(before: Ticks, after: Ticks) -> Ticks {
    Ticks {
        run: after.run - before.run,
        skipped: after.skipped - before.skipped,
    }
}

/// A window's ticks for the report's JSON.
fn ticks_json(t: Ticks) -> Value {
    json!({ "run": t.run, "skipped": t.skipped })
}

/// A window's ticks for the report's tables, beside its flushes.
fn ticks_line(t: Ticks) -> String {
    format!(
        "Retention ticks in the window (`BrokerStats::retention_ticks`, `retention_skips`): {} \
         run, {} skipped.\n",
        t.run, t.skipped
    )
}

/// Publishes in one yardstick block.
const CALIBRATION_PUBLISHES: usize = 50;

/// The producer id the yardstick publishes under.
const CALIBRATION_PRODUCER: u64 = 0x0fab_0be7_0000_0001;

/// THE YARDSTICK: blocks of `CALIBRATION_PUBLISHES` bare publishes, one at a
/// time, from a client in this process — one round trip each, no aterm, no
/// bridge — of three kinds ([`Yardstick`]): to a STRICT broker (one full
/// flush each: the unit), to an ORDERED one on an ordinary subject (one
/// barrier each), and to the same ordered broker on a PERSON-LANE subject,
/// which aterm's table escalates (one full flush each, before the ack).
fn calibrate(ruler: &Ruler, w: &World, clock: Clock, seq: &mut u64) -> Yardstick {
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
    let mut strict = ruler.strict_client(w);
    let mut ordered = ruler.ordered_client();
    let escalated = format!(
        "{}/note",
        aterm_link::subject::person_lane(FLEET, "h-bench", &w.node)
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
    /// Bare publishes to an ordered broker (aterm's options, the default
    /// horizon), on a subject its table does not escalate.
    ordered: Vec<f64>,
    /// The same, on a person-lane subject its table escalates.
    escalated: Vec<f64>,
}

/// The brokers the yardstick publishes to: for the strict block, the world's
/// own when the world is Strict (what every run before the tier knob
/// measured), else a Strict broker opened for it in the world's scratch
/// directory; for the ordered and escalated blocks, always an ordered broker
/// of its own, opened with aterm's ordered options (the default horizon and
/// aterm's table), so the world's log holds none of their records.
struct Ruler {
    own: Option<(Broker, BrokerHandle)>,
    sock: String,
    ordered: (Broker, BrokerHandle),
    ordered_sock: String,
}

impl Ruler {
    fn for_world(w: &World) -> Ruler {
        let serve = |log: &str, sock: &str, tier: aterm_link::durability::Durability| {
            let sock = w.tmp.join(sock).to_string_lossy().into_owned();
            let broker = harness::open_broker(w.tmp.join(log), &harness::broker_opts(tier));
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
        if w.opts.durability == Durability::Strict {
            return Ruler {
                own: None,
                sock: w.broker_sock.clone(),
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

    /// A god client of the strict block's broker.
    fn strict_client(&self, w: &World) -> astream_broker::Client {
        if self.own.is_none() {
            return w.god();
        }
        Self::god_of(&self.sock)
    }

    /// A god client of the ordered blocks' broker.
    fn ordered_client(&self) -> astream_broker::Client {
        Self::god_of(&self.ordered_sock)
    }

    /// What the ordered blocks' broker serves (astream serves ordered strict
    /// off Apple and off the boot volume): the report says, beside them.
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

/// BOOT ONE OF THE RUN'S WORLDS as the arm names it: its broker opened with
/// [`opts_for`], retained by aterm's default rules unless the arm says
/// `retention=off` (what `aterm link broker` opens: the rules at every open),
/// its tick at the arm's interval when it names one, and the bridge's
/// `--log off` when the arm says `log=off` (else the bridge's own default,
/// `auto`, what ships).
fn boot_world(tag: &str, spec: &Spec, env: &[(&str, &str)]) -> World {
    let flags: &[&str] = match spec.log {
        Log::Auto => &[],
        Log::Off => &["--log", "off"],
    };
    let w = World::boot_arm(tag, opts_for(spec), spec.retention, flags, env);
    if let Some(ms) = spec.retention_interval_ms {
        w.broker
            .as_ref()
            .expect("the world serves its own broker")
            .set_retention_interval(Duration::from_millis(ms));
    }
    w
}

/// The options a run's world opens its broker with: aterm's own for the arm's
/// tier ([`harness::broker_opts`]: the tier and, on a weak one, aterm's
/// escalation table) — so the ordered arm measures aterm's real table — with
/// the arm's horizon when it names one.
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
    /// The broker's flushes over the measured round trips: from just before
    /// the first measured post to the end of the last one's idle wait (its
    /// commit acked).
    flushes: FlushCounts,
    /// The broker's retention ticks over the same window ([`Ticks`]).
    ticks: Ticks,
}

/// The latency phase: `warmup + samples` round trips, one at a time.
fn latency(w: &World, broker: &Broker, spec: &Spec, clock: Clock, a: &str, b: &str) -> LatRaw {
    let mut ctl = w.ctl();
    let mut since = newest_id(&mut ctl, b);
    let mut out = Vec::with_capacity(spec.samples);
    let mut texts = Vec::with_capacity(spec.warmup + spec.samples);
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
    /// The broker's BATCHES over the burst: writer batches that appended a
    /// record, from just before the first post to the record past the last
    /// message (the burst's commit) — on the Strict log one full flush each,
    /// on the Ordered one one barrier each: the cost pipelining folds
    /// ([`burst`]).
    batches: u64,
    /// The flushes those batches cost, by kind, over the same window
    /// ([`Broker::flush_counts`]).
    flushes: FlushCounts,
    /// The broker's retention ticks over the same window ([`Ticks`]).
    ticks: Ticks,
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
    aterm_spec::wait::within(DEADLINE, IDLE_POLL, || {
        (broker.visible_head() > b_last + 1).then_some(())
    })
    .unwrap_or_else(|| {
        panic!("burst: nothing landed after the last message's offset {b_last} within {DEADLINE:?}")
    });
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
    }
}

/// What a world's broker serves, in one line for its section: the tier in
/// force, the one asked, why they differ, the horizon and the rule count —
/// read from the broker itself (astream serves ordered strict off Apple and
/// off the boot volume), never from the arm's word — and what it retains,
/// read from it too (its retention rules and its tick's interval), beside the
/// bridge's `--log` the arm set.
fn served_line(w: &World, spec: &Spec) -> String {
    let broker = w.broker.as_ref().expect("the world serves its own broker");
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
    format!(
        "The world's broker serves **{}** (asked {}, reason {:?}; horizon {horizon}; {} \
         escalation rules) and {retains}; the bridge runs `--log {}`{}.\n",
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
    let mut w = boot_world("fbench-msg", spec, &[]);
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
    let cal_before = calibrate(&ruler, &w, clock, &mut cal_seq);
    eprintln!(
        "fabric bench: latency, {} round trips ({} warm-up)",
        spec.samples, spec.warmup
    );
    let ((lat_raw, bur), heads) = sampled(&w, clock, || {
        let broker = w.broker.as_ref().expect("the world serves its own broker");
        let lat = latency(&w, broker, spec, clock, &a, &b);
        eprintln!(
            "fabric bench: burst, {} posts from {} senders, at most {BURST_WINDOW} in flight",
            spec.burst,
            spec.burst_senders()
        );
        let bur = burst(&w, broker, spec, clock, &a, &b, lat.since);
        await_durable(broker);
        (lat, bur)
    });
    let cal_after = calibrate(&ruler, &w, clock, &mut cal_seq);
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
    for text in &lat_raw.texts {
        census::next_is_its_commit(&recs, &group, seq_of(text)).unwrap_or_else(|e| {
            panic!(
                "`{text}`: the latency phase's idle wait passed on the record after the \
                 delivery, and {e} — so the next sample may carry this commit's tail. Make \
                 `latency` wait for the commit itself."
            )
        });
    }
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
    census::lands_before(&recs, &group, b_last, seq_of("fence-0")).unwrap_or_else(|e| {
        panic!(
            "the burst's window ends at the commit covering its last message, and {e} — so the \
             burst's numbers would carry the fence's round trip. Make `burst` wait for that \
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

    let json = json!({
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
            "census": census::to_json(&lat_census, lat.len(), (lat_lo, lat_hi)),
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
            "deliver_ms": stats::to_json(&b_deliver),
            "post_to_acked_ms": stats::to_json(&b_to_acked),
            "acked_to_visible_ms": stats::to_json(&b_acked_to_visible),
            "post_to_durable_head_ms": stats::to_json(&b_to_durable),
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
        served.clone(),
        format!(
            "{} round trips after {} warm-up, one at a time on an idle bridge: session A posts \
             `kind=note` (no `--wait`) to session B, and B's `await inbox` is answered. Acked = \
             the record's offset first seen under the broker's visible head (its publish acked, \
             readers may see it); durable = first seen under its durable head (on stable media \
             by a full flush). On a strict broker the two are one; on an ordered one durable \
             follows at the next full flush, at most the horizon plus a batch and a flush after \
             the first uncovered ack; on a relaxed one no record is ever durable. The inbox \
             commit follows the delivery, after the row is visible.\n",
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
            "Broker batches over the burst (writer batches that appended a record, through the \
             record past the last message): {} — {} per message.\n",
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
    drop(ruler);
    (json, md)
}

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
    /// THE LIVE MAIL ([`attach`]): its text, and the moment it was visible
    /// in its recipient's inbox.
    mail: String,
    t_mail_seen: u64,
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

/// Where the bridge `pid` published its attach among `rows` — `(offset,
/// subject, body head line)`, all at or past the round's mark: its node row
/// (`state=live … pid=<pid>`) and, under that row's `inc=`, each of
/// `session_subjects`' first live row.
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
    let mut w = boot_world("fbench-att", spec, &[]);
    w.wait_ready();
    let served = served_line(&w, spec);
    let first = until("aterm's boot session", || {
        w.sessions().first().map(|(_, sid, _)| sid.clone())
    });
    let node_subject = presence_subject(&w, "node");
    let presence_filter = format!("/f/{FLEET}/pub/{}/*/presence", w.node);
    let ruler = Ruler::for_world(&w);
    let mut cal_seq = 0;
    let cal_before = calibrate(&ruler, &w, clock, &mut cal_seq);
    let (rounds, heads) = sampled(&w, clock, || {
        let broker = w.broker.as_ref().expect("the world serves its own broker");
        let mut mail_ctl = w.ctl();
        let mut mail_since = newest_id(&mut mail_ctl, &first);
        let mailer = first.clone();
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
                let mail = format!("live-{}", rounds.len());
                post(&mut mail_ctl, clock, &mailer, &mailer, &mail);
                let new_pid = aterm_spec::wait::within(DEADLINE, PID_POLL, || {
                    read_pid(&w).filter(|p| *p != pid)
                })
                .unwrap_or_else(|| panic!("the bridge did not come back within {DEADLINE:?}"));
                let t_pid = clock.us();
                pid = new_pid;
                up_since = Instant::now();
                let (woke_at, t_mail_seen) = await_note(&mut mail_ctl, clock, &mailer, mail_since)
                    .unwrap_or_else(|e| panic!("the live mail `{mail}`: {e}"));
                let listed = list_since(&mut mail_ctl, &mailer, mail_since);
                assert!(
                    listed.len() == 1 && listed[0].text == mail && listed[0].id == woke_at,
                    "the live mail `{mail}`: woke at row {woke_at}, listed {listed:?}"
                );
                mail_since = woke_at;
                await_attach(&mut watch, &node_subject, &session_subjects, new_pid);
                drop(watch);
                rounds.push(Round {
                    sessions: size,
                    mark,
                    pid: new_pid,
                    t_kill,
                    t_pid,
                    sids: sids.clone(),
                    mail,
                    t_mail_seen,
                });
            }
        }
        await_durable(broker);
        rounds
    });
    let cal_after = calibrate(&ruler, &w, clock, &mut cal_seq);
    let unit = yardstick(&cal_before, &cal_after);
    let recs = stop_and_read(&mut w);
    let fired = census::will_firings(&recs);

    let mut by_size: BTreeMap<usize, Vec<Value>> = BTreeMap::new();
    let mut series: BTreeMap<usize, [Vec<f64>; 8]> = BTreeMap::new();
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
        let in_window = || census::window(&recs, r.mark, last).filter(|rec| rec.seq != mail_seq);
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
        by_size.entry(r.sessions).or_default().push(json!({
            "mark": r.mark,
            "pid": r.pid,
            "node_offset": found.node,
            "last_offset": last,
            "live_mail_offset": mail_seq,
            "records": c.total,
            "fired_wills": fired_here,
            "kill_to_pid_ms": stats::round3(ms(r.t_kill, r.t_pid)),
            "pid_to_all_ms": ms_opt(Some(r.t_pid), t_all).map(stats::round3),
            "kill_to_live_mail_ms": stats::round3(ms(r.t_kill, r.t_mail_seen)),
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
            "The bridge is SIGKILLed and aterm's supervisor relaunches it; the clock starts when \
             the new bridge writes its pid (its aterm connection is up) and stops when its node \
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
             left out of the attach's census. {} restart(s) per size.\n",
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
        "**pid -> every row acked (attach)**",
        "pid -> node row acked",
        "node row -> last session row",
        "kill -> new pid (supervisor relaunch)",
        "kill -> every row acked",
        "pid -> every row durable",
        "**kill -> the first live mail visible**",
        "every row acked -> the live mail visible (the mirror's start, the refill, the drain)",
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
            "attach_durable_ms": stats::to_json(&s[5]),
            "kill_to_live_mail_ms": stats::to_json(&s[6]),
            "attach_to_live_mail_ms": stats::to_json(&s[7]),
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
    md.extend(yardstick_rows(&ruler, &cal_before, &cal_after));
    md.extend(normalized);
    md.push(String::new());
    let json = json!({
        "node": w.node.clone(),
        "served": served.trim_end(),
        "head_changes_sampled": heads.acked.len(),
        "durable_head_changes_sampled": heads.durable.len(),
        "yardstick": yardstick_json(&ruler, &cal_before, &cal_after, unit),
        "sizes": Value::Array(sizes),
    });
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

/// What the run's broker RETAINS and what the bridge mirrors, for the
/// report's head beside [`broker_note`]: what ships (aterm's default rules at
/// every open, the bridge's `--log auto`, astream's minute between ticks)
/// unless the arm names otherwise.
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
    format!("**Retention:** the log {rules}{tick}; the bridge runs {mirror}.")
}

/// Run only the selected scenario. The closure must contain world setup too.
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
    let (messaging, messaging_md) = measure_scenario(spec.scenario, Scenario::Messaging, || {
        messaging(spec, clock)
    });
    let (pingpong, pingpong_md) = measure_scenario(spec.scenario, Scenario::Pingpong, || {
        pingpong::measure(spec, clock)
    });
    let (attach, attach_md) =
        measure_scenario(spec.scenario, Scenario::Attach, || attach(spec, clock));
    let (handoff, handoff_md) = measure_scenario(spec.scenario, Scenario::Handoff, || {
        handoff::measure(spec, clock)
    });
    let mut md = vec![
        format!("**Broker:** {}", broker_note(spec)),
        String::new(),
        retention_note(spec),
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
        "broker": broker_note(spec),
        "retention": retention_note(spec),
        "clock": "one monotonic origin in the bench process; microsecond stamps",
        "elapsed_s": stats::round3(started.elapsed().as_secs_f64()),
        "messaging": messaging,
        "pingpong": pingpong,
        "attach": attach,
        "handoff": handoff,
    });
    Report {
        json,
        markdown: md.join("\n"),
    }
}

/// [`profile::identity`] of the harness's `aterm`, or why it could not be read,
/// which the lane takes as no identity.
fn binary_identity() -> String {
    profile::identity(Path::new(harness::aterm()))
        .unwrap_or_else(|why| format!("unreadable: {why}"))
}

#[cfg(test)]
mod tests {
    #[test]
    fn only_selected_scenarios_are_measured() {
        use super::*;
        use std::cell::Cell;

        for phase in [Scenario::Messaging, Scenario::Attach, Scenario::Handoff] {
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
        assert_eq!(calls.get(), 4);
    }

    /// EACH ARM OPENS ITS BROKER AS ATERM OPENS IT: aterm's options for the
    /// tier — the escalation table on the weak tiers, none on strict — and
    /// the arm's horizon on ordered, aterm's own unless named.
    #[test]
    fn each_arm_opens_its_broker_with_aterms_options() {
        use super::*;
        use aterm_link::durability::{Durability as Aterm, escalation};
        assert_eq!(
            Duration::from_millis(ORDERED_HORIZON_MS),
            aterm_link::durability::ORDERED_HORIZON
        );
        let spec = |words: &str| crate::spec::parse(&format!("out=/o {words}")).expect(words);
        let bare = opts_for(&spec(""));
        let shipped = harness::broker_opts(aterm_link::durability::default_for(true));
        assert_eq!(
            (bare.durability, bare.escalation),
            (shipped.durability, shipped.escalation),
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
}
