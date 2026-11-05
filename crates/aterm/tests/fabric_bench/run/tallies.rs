// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE OPT-IN TALLY (D02, `tally=on`): the messaging, attach, handoff and ask worlds booted with
//! the bridge's wait tally (`aterm_link::wait_tally`) and aterm's handler tally
//! (`aterm_types::wait_tally::HANDLER_SEAM`) on, both read at each round's end, and each round's
//! rows reported. Off, those worlds boot as every entry before this one did: the tallies' own
//! writes cost time inside what they measure, so a run that names no `tally=` stays comparable
//! with them.
//!
//! A round's rows are what each tally counted since the reading before it. Each reading fences
//! both processes at its own moment. aterm writes its handler tally before it answers the
//! reading's request. The bridge writes its wait tally only at an idle moment in which something
//! moved, so a bridge parked at the reading would show the line from before its park and count
//! the whole park in the next round; the reading wakes it with one record it takes and drops
//! ([`fence_subject`]), and takes its first line in which it has taken a record since the reading
//! began. The fence's own record counts in the round that reading ends, or in the next when
//! another record reached the bridge ahead of it or no line was on file: one record wake, one
//! take that does nothing, one tally write. A bridge or an aterm that is a new process since
//! that reading (a restart, a handoff's successor) counts from its start, so its rows are its
//! whole life up to the reading, and the report says so. Each tally opens its file's name
//! exclusively, so a world that brings up a new process first frees the name
//! ([`Tallies::free_wait`], [`Tallies::free_handlers`]): the process on its way out writes on to
//! the file it holds, and the new one opens its own.

use std::path::PathBuf;

use astream_broker::Broker;
use aterm_json::{Value, json};
use aterm_link::ctl::Ctl;
use aterm_link::wait_tally::{self, Flushed, Hist, Row as TallyRow, Tally, Unit};
use aterm_types::wait_tally::{HANDLER_FILE, HANDLER_SEAM, HandlerRow, HandlerTally};

use super::ask::{await_tally_where, read_tally};
use super::pingpong::{TAIL_1MS, band_quantile, describe, describe_handler, largest, ns_ms};
use super::{FLEET, World};
use crate::spec::Spec;
use crate::stats;

/// The seams a world boots with: its own (`own`), and with `tally=on` both tallies'. Off, `own`
/// alone, as every entry before `tally=` booted.
pub(super) fn world_env(
    spec: &Spec,
    own: &[(&'static str, &'static str)],
) -> Vec<(&'static str, &'static str)> {
    let mut env = own.to_vec();
    if spec.tally {
        for seam in [wait_tally::SEAM, HANDLER_SEAM] {
            if !env.iter().any(|&(k, _)| k == seam) {
                env.push((seam, "1"));
            }
        }
    }
    env
}

/// The fence records' producer (D02).
const FENCE_PRODUCER: u64 = 0x0fab_0d02_0000_0000;

/// Where a reading's fence record goes: under the fleet filter every bridge reads live
/// (`aterm_link::subject::fleet_filter`), and no halt row (`/f/<F>/fleet/<h-name>/halt`,
/// `aterm_link::subject::parse_halt`), so a bridge wakes for it, takes it and does nothing else
/// with it, and an attach's read of the retained halt rows does not see it.
fn fence_subject() -> String {
    format!("/f/{FLEET}/fleet/bench-fence")
}

/// The broker records the bridge whose wait tally `t` is has taken, parked or not.
fn records_taken(t: &Tally) -> u64 {
    t.row(TallyRow::WakeRecord)
        .count
        .saturating_add(t.row(TallyRow::QueuedRecord).count)
}

/// A reading's wait tally: a line in which the bridge has taken a broker record since `before`,
/// the line on file when the reading began (`None`: no line of this bridge then), with its inbox
/// cursor through `through`. The record is the reading's fence or one that reached the bridge
/// ahead of it. The line on file is the one a parked bridge wrote before its park, and taking it
/// would count that park in the next round.
fn fenced(through: u64, before: Option<Tally>) -> impl Fn(&Tally) -> Result<(), String> {
    move |t| {
        if let Some(b) = before.filter(|b| records_taken(t) <= records_taken(b)) {
            Err(format!(
                "it has taken no broker record since the line on file when the reading began \
                 ({} taken, at {} ns of its clock)",
                records_taken(&b),
                b.at_ns
            ))
        } else if t.committed < through {
            Err(format!(
                "its cursor is at {}, not yet through {through}",
                t.committed
            ))
        } else {
            Ok(())
        }
    }
}

/// Where one reading is taken: the bridge whose wait tally is read once its inbox cursor is
/// through `through`, and the session whose inbox the handler tally's fence waits on, past row
/// `since`.
pub(super) struct At<'a> {
    pub bridge: i32,
    pub through: u64,
    pub sid: &'a str,
    pub since: u64,
}

/// Both tallies, read one after the other, and this process's flush times at the reading:
/// its in-process brokers' (`astream_sys::flush_times`, E09), and when it was taken on their
/// clock, after both flush rings were read ([`Mark`]).
#[derive(Debug, Clone, Copy)]
pub(super) struct Reading {
    waits: Tally,
    handlers: HandlerTally,
    bench: Flushed,
    at: u64,
}

impl Reading {
    fn mark(&self) -> Mark {
        Mark::of(&self.waits, &self.bench, self.at)
    }
}

/// One round's rows.
struct Counted {
    /// Its slow calls and the flush rings to line them up with, its group and round named.
    slow: SlowRound,
    waits: Tally,
    /// The bridge was a new process since the reading before: `waits` is its whole life.
    new_bridge: bool,
    handlers: HandlerTally,
    /// The same of aterm.
    new_aterm: bool,
}

/// A world's two tallies, and what its rounds counted.
pub(super) struct Tallies {
    wait: PathBuf,
    handlers: PathBuf,
    last: Option<Reading>,
    /// Every reading's mark so far, oldest first.
    marks: Vec<Mark>,
    rounds: Vec<Counted>,
    /// The fence records published so far: the next one's sequence number less one.
    fences: u64,
}

impl Tallies {
    /// `w`'s tallies: the wait tally in the bridge's state dir, the handler tally in aterm's.
    pub(super) fn of(w: &World) -> Self {
        Self {
            wait: w.state.join(wait_tally::FILE),
            handlers: w.instance_state().join(HANDLER_FILE),
            last: None,
            marks: Vec::new(),
            rounds: Vec::new(),
            fences: 0,
        }
    }

    /// Free the wait tally's name for the next bridge, which opens it exclusively; the running
    /// bridge writes on to the file it holds until it ends.
    pub(super) fn free_wait(&self) {
        free(&self.wait);
    }

    /// Free the handler tally's name for the next aterm, as [`Tallies::free_wait`] does the wait
    /// tally's. An aterm opens it at its first fabric verb, so the name may not be taken yet.
    pub(super) fn free_handlers(&self) {
        free(&self.handlers);
    }

    /// Both tallies now. First the bridge `at.bridge`'s wait tally: the line on file when the
    /// reading begins, then one fence record on `w`'s broker ([`fence_subject`]), which wakes a
    /// parked bridge, then the first line in which the bridge has taken a record since that one,
    /// with its inbox cursor through `at.through` ([`fenced`]; a wait that must end: the
    /// one-minute hang detector bounds it). Then aterm `aterm`'s handler tally behind a fence of
    /// its own: an `await inbox` with no time to wait has aterm write the line before it answers,
    /// so the line holds every span that ended before the request, and the request itself counts
    /// in no row.
    ///
    /// # Panics
    ///
    /// A fence record the broker refused, no such wait tally within the hang detector, a fence
    /// aterm refused, or a handler line that is not `aterm`'s.
    pub(super) fn read(&mut self, w: &World, ctl: &mut Ctl, aterm: u32, at: &At<'_>) -> Reading {
        let before = read_tally(&self.wait, at.bridge).ok();
        self.fences += 1;
        let subject = fence_subject();
        w.god()
            .publish(FENCE_PRODUCER, self.fences, &subject, b"fence")
            .unwrap_or_else(|e| panic!("the reading's fence record on {subject}: {e}"));
        let waits = await_tally_where(
            &self.wait,
            at.bridge,
            "the round's wait tally",
            fenced(at.through, before),
        );
        let line = format!(
            "@{} await inbox since={} kinds=note timeout=0",
            at.sid, at.since
        );
        let reply = super::request(ctl, &line);
        assert!(
            reply.ok(),
            "the handler tally's fence `{line}`: {}",
            reply.header()
        );
        let text =
            aterm_uds::runs::read_bounded_regular_text(&self.handlers, super::ask::TALLY_BYTES)
                .unwrap_or_else(|e| {
                    panic!(
                        "aterm wrote no handler tally at {}: {e}",
                        self.handlers.display()
                    )
                });
        let handlers = HandlerTally::parse(&text)
            .unwrap_or_else(|e| panic!("{}: {e}", self.handlers.display()));
        assert_eq!(
            handlers.pid,
            aterm,
            "the handler tally at {} is another aterm's",
            self.handlers.display()
        );
        Reading {
            waits,
            handlers,
            bench: Flushed::now(),
            at: wait_tally::clock_ns(),
        }
    }

    /// Count the next round from `now`.
    pub(super) fn rebase(&mut self, now: Reading) {
        self.last = Some(now);
        self.marks.push(now.mark());
    }

    /// Round `round` of `group`: what each tally counted from the last reading to `now`, which
    /// the next round counts from.
    ///
    /// # Panics
    ///
    /// Two readings of one process that do not pair (a count that went backwards).
    pub(super) fn round(&mut self, group: &str, round: String, now: Reading) {
        let before = self.last.replace(now);
        let (waits, new_bridge) = match before.map(|b| b.waits) {
            Some(b) if b.pid == now.waits.pid => (
                now.waits
                    .since(&b)
                    .unwrap_or_else(|e| panic!("{group}, {round}: the wait tallies: {e}")),
                false,
            ),
            _ => (now.waits, true),
        };
        let (handlers, new_aterm) = match before.map(|b| b.handlers) {
            Some(b) if b.pid == now.handlers.pid => (
                now.handlers
                    .since(&b)
                    .unwrap_or_else(|e| panic!("{group}, {round}: the handler tallies: {e}")),
                false,
            ),
            _ => (now.handlers, true),
        };
        self.rounds.push(Counted {
            slow: SlowRound {
                group: group.to_string(),
                round,
                calls: waits.calls,
                bridge_pid: now.waits.pid,
                bridge: now.waits.times,
                bench: now.bench,
                window: (before.map_or(now.at, |b| b.at), now.at),
                marks: self.marks.clone(),
            },
            waits,
            new_bridge,
            handlers,
            new_aterm,
        });
        self.marks.push(now.mark());
    }

    /// Every round's rows, for the report's JSON.
    pub(super) fn json(&self) -> Value {
        let rounds: Vec<Value> = self
            .rounds
            .iter()
            .map(|c| {
                json!({
                    "group": c.slow.group,
                    "round": c.slow.round,
                    "wait": {
                        "format": wait_tally::FORMAT,
                        "pid": c.waits.pid,
                        "new_process": c.new_bridge,
                        "clock_ms": stats::round3(ns_ms(c.waits.at_ns)),
                        "committed": c.waits.committed,
                        "dir_counted": c.waits.dir,
                        "astream_flushes": {
                            "full": c.waits.flushes.full,
                            "barrier": c.waits.flushes.barrier,
                            "plain": c.waits.flushes.plain,
                            "counted": wait_tally::FLUSHES_COUNTED,
                        },
                        "rows": rows_json(
                            TallyRow::ALL.map(|r| (r.name(), r.unit(), *c.waits.row(r))),
                        ),
                    },
                    "handlers": {
                        "pid": c.handlers.pid,
                        "new_process": c.new_aterm,
                        "clock_ms": stats::round3(ns_ms(c.handlers.at_ns)),
                        "rows": rows_json(
                            HandlerRow::ALL.map(|r| (r.name(), Unit::Ns, *c.handlers.row(r)))
                        ),
                    },
                })
            })
            .collect();
        json!({
            "what": "both tallies over each round (tally=on): what each counted since the reading before, or a new process's whole life",
            "bands": "log2: band 0 under 1024 ns, band i from 1 [2^(i+9), 2^(i+10)) ns, the last from 2^26 ns; a count's band 0 is 0, band i [2^(i-1), 2^i)",
            "rounds": rounds,
            "slow_calls": slow_calls_json(self.rounds.iter().map(|c| &c.slow)),
        })
    }

    /// Each round that counted a slow call, lined up ([`lined_up`]).
    fn slow_calls(&self) -> Vec<RoundCalls> {
        lined_up(self.rounds.iter().map(|c| &c.slow))
    }

    /// Every round's rows, a table for each group of rounds, for the entry.
    pub(super) fn md(&self) -> Vec<String> {
        let mut md = Vec::new();
        let mut groups: Vec<&str> = Vec::new();
        for c in &self.rounds {
            if !groups.contains(&c.slow.group.as_str()) {
                groups.push(c.slow.group.as_str());
            }
        }
        for group in groups {
            let rounds: Vec<&Counted> = self
                .rounds
                .iter()
                .filter(|c| c.slow.group == group)
                .collect();
            md.push(format!("#### Both tallies, {group} (`tally=on`)\n"));
            md.push(format!(
                "Each round's rows since the reading before it (`{}`, and aterm's handler \
                 tally); a process new since then counts from its start, its whole life so far. \
                 The bridge's astream-sys flushes count `{}` (`aterm_durable::ASTREAM_COUNTS`).\n",
                wait_tally::FORMAT,
                wait_tally::FLUSHES_COUNTED
            ));
            for c in &rounds {
                md.push(format!(
                    "- {}: the bridge (pid {}{}) {:.1} ms of its clock, astream-sys flushes full \
                     {}, barrier {} and plain {}; aterm (pid {}{}) {:.1} ms of its clock.",
                    c.slow.round,
                    c.waits.pid,
                    if c.new_bridge { ", a new process" } else { "" },
                    ns_ms(c.waits.at_ns),
                    c.waits.flushes.full,
                    c.waits.flushes.barrier,
                    c.waits.flushes.plain,
                    c.handlers.pid,
                    if c.new_aterm { ", a new process" } else { "" },
                    ns_ms(c.handlers.at_ns),
                ));
            }
            md.push(String::new());
            md.push(
                "| row | round | n | ms | µs a call | p50 / p95 / p99 (µs, the log2 band's bound) \
                 | ≥ 1.05 ms | largest (ms) |"
                    .to_string(),
            );
            md.push("|---|---|---:|---:|---:|---:|---:|---:|".to_string());
            for row in TallyRow::ALL {
                if row.unit() == Unit::Ns {
                    let each: Vec<(&str, Hist)> = rounds
                        .iter()
                        .map(|c| (c.slow.round.as_str(), *c.waits.row(row)))
                        .collect();
                    md.extend(table_rows(&format!("bridge: {}", describe(row)), &each));
                }
            }
            for row in HandlerRow::ALL {
                let each: Vec<(&str, Hist)> = rounds
                    .iter()
                    .map(|c| (c.slow.round.as_str(), *c.handlers.row(row)))
                    .collect();
                md.extend(table_rows(
                    &format!("aterm: {}", describe_handler(row)),
                    &each,
                ));
            }
            md.push(String::new());
            let calls: Vec<RoundCalls> = self
                .slow_calls()
                .into_iter()
                .filter(|r| r.group == group)
                .collect();
            md.extend(slow_calls_md(group, &calls));
        }
        md
    }
}

/// A world of one aterm and one bridge throughout (the messaging and ask worlds): every reading's
/// fixed parts. A round's wait tally is the bridge's first line after the reading's fence with its
/// cursor through the inbox commit the broker holds at the round's end ([`Tallies::read`]); its
/// handler fence waits on one session's inbox past its newest row when the probe began.
pub(super) struct Probe<'a> {
    tallies: Tallies,
    w: &'a World,
    ctl: Ctl,
    aterm: u32,
    bridge: i32,
    broker: &'a Broker,
    group: String,
    sid: String,
    since: u64,
}

impl<'a> Probe<'a> {
    /// `w`'s tallies, its fences on `sid`'s inbox.
    pub(super) fn of(w: &'a World, broker: &'a Broker, sid: &str) -> Self {
        let mut ctl = w.ctl();
        let since = super::newest_id(&mut ctl, sid);
        Self {
            tallies: Tallies::of(w),
            w,
            ctl,
            aterm: w.gui.as_ref().expect("the world's aterm").id(),
            bridge: w.bridge_pid(),
            broker,
            group: aterm_link::subject::inbox_group(FLEET, &w.node),
            sid: sid.to_string(),
            since,
        }
    }

    fn reading(&mut self) -> Reading {
        let at = At {
            bridge: self.bridge,
            through: self.broker.group_start(&self.group).saturating_sub(1),
            sid: &self.sid,
            since: self.since,
        };
        self.tallies.read(self.w, &mut self.ctl, self.aterm, &at)
    }

    /// The reading the first round counts from.
    pub(super) fn start(&mut self) {
        let now = self.reading();
        self.tallies.rebase(now);
    }

    /// Round `round` of `group` ends now ([`Tallies::round`]): the bridge's wait tally the
    /// reading took, past its fence.
    pub(super) fn round(&mut self, group: &str, round: impl Into<String>) -> Tally {
        let now = self.reading();
        self.tallies.round(group, round.into(), now);
        now.waits
    }

    /// What the rounds counted.
    pub(super) fn into_tallies(self) -> Tallies {
        self.tallies
    }
}

/// Free a tally's name: a name no file holds is free already.
fn free(name: &std::path::Path) {
    match std::fs::remove_file(name) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => panic!("free the tally's name {}: {e}", name.display()),
    }
}

/// A world's report with what its rounds counted, when it counted: the JSON's `tallies` and the
/// entry's tables.
pub(super) fn report(counted: Option<&Tallies>, json: &mut Value, md: &mut Vec<String>) {
    let Some(t) = counted else {
        return;
    };
    if let Some(fields) = json.as_object_mut() {
        fields.insert("tallies".to_string(), t.json());
    }
    md.extend(t.md());
}

/// Rows by name, each one that counted anything: its unit, and its count, total, largest and
/// bands, the total and largest in that unit (nanoseconds, or a count row's items), as the
/// ping-pong report writes them.
fn rows_json(rows: impl IntoIterator<Item = (&'static str, Unit, Hist)>) -> Value {
    let mut out = aterm_json::Map::new();
    for (name, unit, h) in rows {
        if h.count > 0 {
            out.insert(
                name.to_string(),
                json!({
                    "unit": if unit == Unit::Ns { "ns" } else { "count" },
                    "count": h.count,
                    "total": h.total,
                    "max": h.max,
                    "bands": h.buckets.to_vec(),
                }),
            );
        }
    }
    Value::Object(out)
}

/// The rounds' values of one row pooled: every count, total and band summed, the largest kept.
fn pooled(each: &[(&str, Hist)]) -> Hist {
    each.iter().fold(Hist::default(), |mut sum, (_, h)| {
        sum.count = sum.count.saturating_add(h.count);
        sum.total = sum.total.saturating_add(h.total);
        sum.max = sum.max.max(h.max);
        for (s, b) in sum.buckets.iter_mut().zip(h.buckets) {
            *s = s.saturating_add(b);
        }
        sum
    })
}

/// One time row's lines: one a round that counted it, and with more than one round, every round
/// pooled. A row no round counted has none.
fn table_rows(what: &str, each: &[(&str, Hist)]) -> Vec<String> {
    if each.iter().all(|(_, h)| h.count == 0) {
        return Vec::new();
    }
    let line = |what: &str, round: &str, h: &Hist| {
        format!(
            "| {what} | {round} | {} | {:.3} | {} | {} / {} / {} | {} | {} |",
            h.count,
            ns_ms(h.total),
            if h.count == 0 {
                "–".to_string()
            } else {
                format!("{:.1}", h.total as f64 / h.count as f64 / 1e3)
            },
            band_quantile(h, 0.50),
            band_quantile(h, 0.95),
            band_quantile(h, 0.99),
            h.at_least(TAIL_1MS),
            largest(h),
        )
    };
    let mut rows: Vec<String> = each
        .iter()
        .enumerate()
        .map(|(i, (round, h))| line(if i == 0 { what } else { "" }, round, h))
        .collect();
    if each.len() > 1 {
        rows.push(line("", &format!("**all {}**", each.len()), &pooled(each)));
    }
    rows
}

/// What the report's slow calls are (E09, E09c).
const SLOW_CALLS_WHAT: &str = concat!(
    "every slow call (1.05 ms or more) the bridge made in each round, as its wait tally keeps ",
    "them (the process's last calls_kept, each call's row, start and length; the loop's park, ",
    "its idle wait for work, and the mailbox's queue waits are no calls), joined into stalls ",
    "where calls overlap (an ack and its commit are one span, the pin's fence holds the ",
    "replace it makes, a pin's write the append it makes), with every kept slow flush (slower ",
    "than 1 ms, astream-sys's ring) that overlaps a stall: the bridge's own (its wait tally ",
    "line's ring, its flushes through astream-sys: aterm-durable's and its state dir's) and, ",
    "on the machine's clock only, the bench process's (its in-process brokers', ",
    "astream_sys::flush_times at the round's reading). counted is how many slow calls the ",
    "round's tally counted and kept how many of them its ring still held, the round's last ",
    "ones. Each kept slow flush carries its index, its place among its process's slow ",
    "flushes, and each reading marks each ring's count of them, read before the bench's clock: ",
    "every flush indexed below a mark's count ended before the mark (astream-sys's release and ",
    "acquire orders, argued there and tested by nothing), so a ring holds every slow flush ",
    "that returned by the round's reading and could run at a time exactly when it holds every ",
    "index from the count of the latest mark at or before it. A flush still running at the ",
    "round's reading has no index yet and no ring here holds it; each reading counts those ",
    "(running, every flush call of its process begun and not yet returned), and one may have ",
    "begun under any stall of the round. flush_rings gives each ring's kept slow flushes, every ",
    "one its process recorded by the round's reading, the index the round's window starts at ",
    "(the reading before's count, 0 for a process new since), how many of the window the ring ",
    "lost (to its turning over or to a write the reading raced) and which (missing, each run of ",
    "indices as its first and last), and the calls its process was still running; rings_whole ",
    "says no ring lost one from the round's start or any stall's and none was running, so every ",
    "slow flush that could have run in the round is kept. A stall's lost counts, over the ",
    "rings, the slow flushes that could have run under it and that its rings no longer held, ",
    "and its running the calls still running at the round's reading; its explained_ns is the ",
    "part of it some kept flush ran under, exact when both are 0 and otherwise a floor. ",
    "rest_of_round is the baseline: the share of the round's span (the reading before to the ",
    "round's, on the bench's clock) outside its stalls that some kept slow flush ran under, ",
    "complete when the rings are whole (otherwise a floor), and null off the machine's clock, ",
    "where that span and the bridge's times do not line up (and the bridge's ring is judged ",
    "from its process's start). A flush a stall makes itself (the pin's fence) counts in its ",
    "explained share and never in the baseline, so the two say where the flushes fell, not ",
    "which call waited on which; the baseline describes the round and tests nothing. ",
    "summary.chance_ns weighs each stall by its round's baseline, null when a round has none",
);

/// The part of `[from, to)` that `spans` cover, a stretch two of them share counted once.
fn covered(spans: impl IntoIterator<Item = (u64, u64)>, from: u64, to: u64) -> u64 {
    let mut clipped: Vec<(u64, u64)> = spans
        .into_iter()
        .map(|(start, end)| (start.max(from), end.min(to)))
        .filter(|(start, end)| end > start)
        .collect();
    clipped.sort_unstable();
    let (mut sum, mut reach) = (0u64, from);
    for (start, end) in clipped {
        let start = start.max(reach);
        if end > start {
            sum += end - start;
            reach = end;
        }
    }
    sum
}

/// What a reading says of the slow flushes before it (E09c): when it was taken on the bench's
/// flush clock, read after both rings were, and each ring's slow count then, the bridge's with
/// its pid. astream-sys takes a slow flush's index after it reads the flush's end, and a
/// reading takes its count after its ring, so every slow flush indexed below a mark's count
/// ended before the mark's time: none of them can run under anything that starts at that time
/// or later (astream-sys's release and acquire orders, which its module docs argue and no
/// test checks). The bridge's count is its line's, written before the bench read it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Mark {
    pub(super) at: u64,
    pub(super) bench: u64,
    pub(super) bridge_pid: u32,
    pub(super) bridge: u64,
}

impl Mark {
    /// The mark of a reading that took the bridge's line `waits`, then this process's flush
    /// times `bench`, then the clock `at`.
    pub(super) const fn of(waits: &Tally, bench: &Flushed, at: u64) -> Self {
        Self {
            at,
            bench: bench.slow_count,
            bridge_pid: waits.pid,
            bridge: waits.times.slow_count,
        }
    }
}

/// One flush ring a round lines up: whose it is, the ring at the round's reading, whole, and
/// the marks its indices are judged by.
#[derive(Clone, Copy)]
struct Ring<'a> {
    process: &'static str,
    flushes: &'a Flushed,
    /// The bridge's pid, whose marks alone count for its ring; `None` for the bench's own.
    pid: Option<u32>,
    /// The readings before the round, oldest first; none for a ring whose times no mark's
    /// clock compares with (the bridge's off the machine's clock), judged from its start.
    marks: &'a [Mark],
}

impl Ring<'_> {
    /// The index the ring must hold every slow flush from to hold each that returned by the
    /// round's reading and could run at `at` or later: the count of the latest mark at or
    /// before `at` ([`Mark`]); 0 (the process's whole life) when there is none, or it is
    /// another bridge's (a relaunch: its count is another process's).
    fn from(&self, at: u64) -> u64 {
        self.marks
            .iter()
            .rev()
            .find(|m| m.at <= at)
            .map_or(0, |m| match self.pid {
                None => m.bench,
                Some(pid) if pid == m.bridge_pid => m.bridge,
                Some(_) => 0,
            })
    }

    /// How many of the slow flushes that returned by the round's reading and could run at `at`
    /// or later the ring no longer holds.
    fn lost(&self, at: u64) -> u64 {
        self.flushes.missed(self.from(at))
    }

    /// The indices from `from` up to the ring's count it does not hold, each run of them as its
    /// first and last: the ones [`Flushed::missed`] counts.
    fn missing(&self, from: u64) -> Vec<(u64, u64)> {
        let to = self.flushes.slow_count;
        let mut runs = Vec::new();
        let mut next = from;
        for i in self.flushes.slow().map(|f| f.index) {
            if i >= to {
                break;
            }
            if i > next {
                runs.push((next, i - 1));
            }
            next = next.max(i.saturating_add(1));
        }
        if next < to {
            runs.push((next, to - 1));
        }
        runs
    }
}

/// The share of a round outside its stalls some kept slow flush ran under: the baseline a
/// stall's share is read against.
#[derive(Debug, Clone, Copy)]
struct Chance {
    /// The round's time outside its stalls.
    span: u64,
    /// The part of it some kept slow flush ran under: a floor unless the rings are whole.
    covered: u64,
}

impl Chance {
    /// The share of `window` outside `stalls` (disjoint spans) the flushes of `rings` cover.
    fn outside(window: (u64, u64), stalls: &[(u64, u64)], rings: &[Ring<'_>]) -> Self {
        let (from, to) = window;
        let flushes: Vec<(u64, u64)> = rings
            .iter()
            .flat_map(|ring| ring.flushes.slow().map(|f| (f.start_ns, f.end_ns)))
            .collect();
        let inside: u64 = stalls
            .iter()
            .map(|&(start, end)| covered(flushes.iter().copied(), start.max(from), end.min(to)))
            .sum();
        Self {
            span: to
                .saturating_sub(from)
                .saturating_sub(covered(stalls.iter().copied(), from, to)),
            covered: covered(flushes.iter().copied(), from, to).saturating_sub(inside),
        }
    }

    fn share(self) -> f64 {
        self.covered as f64 / self.span.max(1) as f64
    }
}

/// One slow flush under a stall: whose ring held it, and how much of the stall it covers.
struct Under {
    process: &'static str,
    flush: wait_tally::SlowFlush,
    overlap: u64,
}

/// Slow calls that overlap, one stretch of the bridge's time, and the slow flushes under it.
struct Stall {
    /// Its calls, in the order they started.
    calls: Vec<wait_tally::SlowCall>,
    /// Its span on astream-sys's clock: the first call's start to the last end.
    start: u64,
    end: u64,
    under: Vec<Under>,
    /// The part of it some kept flush ran under (the union of the overlaps).
    explained: u64,
    /// The slow flushes that returned by the round's reading and could have run under it that
    /// its rings no longer held ([`Ring::lost`] at its start), over the rings.
    lost: u64,
    /// The flush calls its rings' processes were still running at the round's reading
    /// (`Flushed::running`), over the rings: each may have begun under it, and none has an
    /// index or a ring place yet. `under` names every slow flush under it exactly when this
    /// and `lost` are both 0.
    running: u64,
}

impl Stall {
    /// `calls` joined into stalls: sorted by start (the longer first on a tie), a call that
    /// starts before the stall so far ends joins it.
    fn join(
        calls: impl IntoIterator<Item = wait_tally::SlowCall>,
    ) -> Vec<Vec<wait_tally::SlowCall>> {
        let mut calls: Vec<wait_tally::SlowCall> = calls.into_iter().collect();
        calls.sort_by_key(|c| (c.start_ns, std::cmp::Reverse(c.ns), c.row));
        let mut out: Vec<(u64, Vec<wait_tally::SlowCall>)> = Vec::new();
        for c in calls {
            match out.last_mut() {
                Some((end, joined)) if c.start_ns < *end => {
                    *end = (*end).max(c.end_ns());
                    joined.push(c);
                }
                _ => out.push((c.end_ns(), vec![c])),
            }
        }
        out.into_iter().map(|(_, joined)| joined).collect()
    }

    /// The stall of `calls` (overlapping ones, [`Stall::join`]), the flushes of `rings` that
    /// overlap it, how many that could have its rings lost, and how many were still running.
    fn of(calls: Vec<wait_tally::SlowCall>, rings: &[Ring<'_>]) -> Self {
        let start = calls.iter().map(|c| c.start_ns).min().unwrap_or(0);
        let end = calls.iter().map(|c| c.end_ns()).max().unwrap_or(start);
        let mut under = Vec::new();
        for ring in rings {
            for flush in ring.flushes.slow() {
                let overlap = end
                    .min(flush.end_ns)
                    .saturating_sub(start.max(flush.start_ns));
                if overlap > 0 {
                    under.push(Under {
                        process: ring.process,
                        flush,
                        overlap,
                    });
                }
            }
        }
        let explained = covered(
            under.iter().map(|u| (u.flush.start_ns, u.flush.end_ns)),
            start,
            end,
        );
        Self {
            calls,
            start,
            end,
            under,
            explained,
            lost: rings.iter().map(|ring| ring.lost(start)).sum(),
            running: rings.iter().map(|ring| ring.flushes.running).sum(),
        }
    }

    /// Whether a slow flush could have run under it that `under` does not name.
    const fn unsure(&self) -> bool {
        self.lost > 0 || self.running > 0
    }

    const fn ns(&self) -> u64 {
        self.end - self.start
    }
}

/// One round's slow calls and what to line them up with: what a world read at the round's two
/// readings (the messaging, attach, handoff and ask worlds through [`Tallies`], ping-pong at
/// its own).
#[derive(Debug, Clone)]
pub(super) struct SlowRound {
    /// The rounds this one is reported with ("attach, 8 sessions").
    pub(super) group: String,
    /// The round within it.
    pub(super) round: String,
    /// The round's slow calls: the bridge's wait tally difference's (`Tally::since`), or a new
    /// bridge's whole life.
    pub(super) calls: wait_tally::SlowCalls,
    /// The bridge whose flush times `bridge` are.
    pub(super) bridge_pid: u32,
    /// The bridge's flush times at the round's reading, whole: its ring and the count of every
    /// slow flush it recorded (a difference's counts only the round's).
    pub(super) bridge: Flushed,
    /// This process's flush times at the round's reading: its brokers' ring, whole.
    pub(super) bench: Flushed,
    /// The round's span on the flush clock: the reading before to this one's.
    pub(super) window: (u64, u64),
    /// The marks of every reading before the round's, oldest first.
    pub(super) marks: Vec<Mark>,
}

/// One ring of a round, lined up: its process, the slow flushes it kept, every one its process
/// recorded by the round's reading, the index the round's window starts at, how many of the
/// window it lost and which (each run of indices as its first and last), and the flush calls
/// its process was still running at the reading.
#[derive(Debug, Clone, PartialEq, Eq)]
struct RingLine {
    process: &'static str,
    kept: usize,
    recorded: u64,
    from: u64,
    missed: u64,
    missing: Vec<(u64, u64)>,
    running: u64,
}

impl RingLine {
    /// How many slow flushes the round's window holds: from its start to the ring's count.
    const fn window(&self) -> u64 {
        self.recorded.saturating_sub(self.from)
    }
}

/// A round's slow calls, joined into stalls, and the share of the rest of the round under a
/// slow flush.
struct RoundCalls {
    group: String,
    round: String,
    /// How many slow calls the round's tally counted; its ring kept the last ones, the stalls'.
    counted: u64,
    stalls: Vec<Stall>,
    /// Each flush ring lined up.
    rings: Vec<RingLine>,
    /// No ring lost a slow flush that could have run in the round or under one of its stalls,
    /// and no process was still running a flush call at the round's reading: every slow flush
    /// that could have run in the round is kept, and `none` under a stall is no slow flush at
    /// all.
    whole: bool,
    /// The rest of the round's share under a kept slow flush; `None` off the machine's clock,
    /// where the round's span (the bench's clock) and the bridge's times do not line up.
    rest: Option<Chance>,
}

impl RoundCalls {
    /// `r`'s slow calls and the flushes under them, on this platform's flush clock.
    fn of(r: &SlowRound) -> Self {
        Self::on(r, wait_tally::ACROSS_PROCESSES)
    }

    /// `r`'s slow calls and the flushes under them; `across` when the flush clock is the
    /// machine's, so the bench's ring, the marks and the round's span (all the bench's) line up
    /// with the bridge's times.
    fn on(r: &SlowRound, across: bool) -> Self {
        let mut rings = vec![Ring {
            process: "bridge",
            flushes: &r.bridge,
            pid: Some(r.bridge_pid),
            marks: if across { &r.marks } else { &[] },
        }];
        if across {
            rings.push(Ring {
                process: "bench",
                flushes: &r.bench,
                pid: None,
                marks: &r.marks,
            });
        }
        let stalls: Vec<Stall> = Stall::join(r.calls.iter())
            .into_iter()
            .map(|calls| Stall::of(calls, &rings))
            .collect();
        let spans: Vec<(u64, u64)> = stalls.iter().map(|s| (s.start, s.end)).collect();
        let lines: Vec<RingLine> = rings
            .iter()
            .map(|ring| {
                let from = ring.from(r.window.0);
                RingLine {
                    process: ring.process,
                    kept: ring.flushes.slow().count(),
                    recorded: ring.flushes.slow_count,
                    from,
                    missed: ring.flushes.missed(from),
                    missing: ring.missing(from),
                    running: ring.flushes.running,
                }
            })
            .collect();
        Self {
            group: r.group.clone(),
            round: r.round.clone(),
            counted: r.calls.count,
            whole: lines.iter().all(|l| l.missed == 0 && l.running == 0)
                && stalls.iter().all(|s| !s.unsure()),
            rings: lines,
            rest: across.then(|| Chance::outside(r.window, &spans, &rings)),
            stalls,
        }
    }

    fn kept(&self) -> usize {
        self.stalls.iter().map(|s| s.calls.len()).sum()
    }
}

/// Each round of `rounds` that counted a slow call, its calls joined into stalls with the slow
/// flushes that ran under each and the share of the rest of the round they cover
/// ([`RoundCalls`]).
fn lined_up<'a>(rounds: impl IntoIterator<Item = &'a SlowRound>) -> Vec<RoundCalls> {
    rounds
        .into_iter()
        .filter(|r| r.calls.count > 0)
        .map(RoundCalls::of)
        .collect()
}

/// The slow calls of `rounds` for a report's JSON (`tallies.slow_calls`, and ping-pong's
/// `slow_calls`): what they are, the clock, the call ring's size, each round that counted one
/// and the summary.
pub(super) fn slow_calls_json<'a>(rounds: impl IntoIterator<Item = &'a SlowRound>) -> Value {
    let calls = lined_up(rounds);
    json!({
        "what": SLOW_CALLS_WHAT,
        "clock": wait_tally::FLUSH_CLOCK.word(),
        "calls_kept": wait_tally::CALLS_KEPT,
        "rounds": rounds_json(&calls),
        "summary": summary_json(&calls),
    })
}

/// The slow calls of `rounds` in `group`, for the entry ([`slow_calls_md`]).
pub(super) fn slow_calls_tables<'a>(
    group: &str,
    rounds: impl IntoIterator<Item = &'a SlowRound>,
) -> Vec<String> {
    let calls: Vec<RoundCalls> = lined_up(rounds)
        .into_iter()
        .filter(|r| r.group == group)
        .collect();
    slow_calls_md(group, &calls)
}

/// What the stalls' time a flush ran under would be at the rest of their rounds' rate: each
/// stall's length times its round's baseline; `None` when a round has none.
fn chance_ns(rounds: &[RoundCalls]) -> Option<u64> {
    rounds.iter().try_fold(0u64, |sum, r| {
        let rest = r.rest?;
        Some(
            sum + r
                .stalls
                .iter()
                .map(|s| (s.ns() as f64 * rest.share()).round() as u64)
                .sum::<u64>(),
        )
    })
}

/// The slow calls counted and kept, the stalls they make, how many a flush ran under, how many
/// a ring lost one under and how many had a flush call still running at the reading, the share
/// of their time a kept flush ran under, and that share at the rest of their rounds' rate.
fn summary_json(rounds: &[RoundCalls]) -> Value {
    let stalls = || rounds.iter().flat_map(|r| &r.stalls);
    let whole = rounds.iter().all(|r| r.whole);
    let chance = chance_ns(rounds);
    json!({
        "calls_counted": rounds.iter().map(|r| r.counted).sum::<u64>(),
        "calls_kept": rounds.iter().map(RoundCalls::kept).sum::<usize>(),
        "stalls": stalls().count(),
        "under_a_flush": stalls().filter(|s| !s.under.is_empty()).count(),
        "with_flushes_lost": stalls().filter(|s| s.lost > 0).count(),
        "with_flushes_running": stalls().filter(|s| s.running > 0).count(),
        "ns": stalls().map(Stall::ns).sum::<u64>(),
        "explained_ns": stalls().map(|s| s.explained).sum::<u64>(),
        "rings_whole": whole,
        "chance_ns": chance,
        "chance_complete": chance.map(|_| whole),
    })
}

/// The slow calls' rounds for the report's JSON.
fn rounds_json(rounds: &[RoundCalls]) -> Value {
    let rounds: Vec<Value> = rounds
        .iter()
        .map(|r| {
            json!({
                "group": r.group,
                "round": r.round,
                "counted": r.counted,
                "kept": r.kept(),
                "flush_rings": r.rings.iter().map(|l| json!({
                    "process": l.process,
                    "kept": l.kept,
                    "recorded": l.recorded,
                    "from": l.from,
                    "missed": l.missed,
                    "missing": l.missing
                        .iter()
                        .map(|&(first, last)| json!([first, last]))
                        .collect::<Vec<Value>>(),
                    "running": l.running,
                })).collect::<Vec<Value>>(),
                "rings_whole": r.whole,
                "rest_of_round": r.rest.map_or(Value::Null, |rest| json!({
                    "span_ns": rest.span,
                    "covered_ns": rest.covered,
                    "complete": r.whole,
                })),
                "stalls": r.stalls.iter().map(|s| json!({
                    "start_ns": s.start,
                    "ns": s.ns(),
                    "explained_ns": s.explained,
                    "lost": s.lost,
                    "running": s.running,
                    "calls": s.calls.iter().map(|c| json!({
                        "row": c.row.name(),
                        "start_ns": c.start_ns,
                        "ns": c.ns,
                    })).collect::<Vec<Value>>(),
                    "flushes": s.under.iter().map(|u| json!({
                        "process": u.process,
                        "index": u.flush.index,
                        "kind": u.flush.kind.word(),
                        "start_ns": u.flush.start_ns,
                        "end_ns": u.flush.end_ns,
                        "overlap_ns": u.overlap,
                    })).collect::<Vec<Value>>(),
                })).collect::<Vec<Value>>(),
            })
        })
        .collect();
    Value::Array(rounds)
}

/// A group's slow calls, for the entry: one line a stall, each round's calls its ring did not
/// keep, then the share explained and the rest of the rounds' rate.
fn slow_calls_md(group: &str, rounds: &[RoundCalls]) -> Vec<String> {
    let mut md = vec![
        format!("#### Slow calls and the flushes under them, {group} (E09)\n"),
        format!(
            "Every slow call (1.05 ms or more) the bridge made in each round, as its wait tally \
             keeps them (its last {}; the loop's park, its idle wait, and the mailbox's queue \
             waits are no calls), joined into stalls where calls overlap (an ack and its commit \
             are one span, the pin's fence holds its replace), and every kept slow flush \
             (> 1 ms) that ran under a stall: the bridge's own and{} the bench process's (its \
             brokers'). Each kept flush carries its index, so a ring says how many slow flushes \
             that could have run under a stall it lost (E09c), and each reading counts the flush \
             calls its process was still running, which no ring holds yet: under a stall, `none` \
             is no slow flush at all, and `none kept` none the rings still held, then `N lost` \
             where they lost N that could have run under it and `N running` where N calls were \
             still running at the round's reading, any of which may have begun under it; a share \
             under a flush is `≥` where either is. The last column is the share of the rest of \
             the stall's round, outside its stalls, that some kept slow flush ran under (`≥` \
             where a ring lost one or a call was running; {}): the rate a stall would show \
             if flushes fell on it as they fell elsewhere, a description and no test. A flush a \
             stall makes itself counts in its share and not in that rate. Times on \
             astream-sys's `{}` clock.\n",
            wait_tally::CALLS_KEPT,
            if wait_tally::ACROSS_PROCESSES {
                ""
            } else {
                " not (another process's clock)"
            },
            if wait_tally::ACROSS_PROCESSES {
                "`unavailable` off the machine's clock"
            } else {
                "`unavailable` here, the round's span on another process's clock"
            },
            wait_tally::FLUSH_CLOCK.word()
        ),
    ];
    if rounds.iter().all(|r| r.counted == 0) {
        md.push(
            "The bridge's tally counted no slow call (1.05 ms or more) in these rounds.\n"
                .to_string(),
        );
        return md;
    }
    md.push(
        "| round | stall (ms) | its slow calls (row ms) | flushes under it (process kind ms: \
         overlap ms) | share under a flush | rest of the round under a flush |"
            .to_string(),
    );
    md.push("|---|---:|---|---|---:|---:|".to_string());
    let pct = |part: u64, whole: u64| 100.0 * part as f64 / whole.max(1) as f64;
    for r in rounds {
        for s in &r.stalls {
            let calls = s
                .calls
                .iter()
                .map(|c| format!("{} {:.3}", c.row.name(), ns_ms(c.ns)))
                .collect::<Vec<_>>()
                .join(", ");
            let mut under = if s.under.is_empty() {
                (if s.unsure() { "none kept" } else { "none" }).to_string()
            } else {
                s.under
                    .iter()
                    .map(|u| {
                        format!(
                            "{} {} {:.2}: {:.2}",
                            u.process,
                            u.flush.kind.word(),
                            ns_ms(u.flush.end_ns - u.flush.start_ns),
                            ns_ms(u.overlap)
                        )
                    })
                    .collect::<Vec<_>>()
                    .join("; ")
            };
            if s.lost > 0 {
                under.push_str(&format!(", {} lost", s.lost));
            }
            if s.running > 0 {
                under.push_str(&format!(", {} running", s.running));
            }
            let rest = r.rest.map_or_else(
                || "unavailable".to_string(),
                |rest| {
                    format!(
                        "{}{:.0}%",
                        if r.whole { "" } else { "≥ " },
                        100.0 * rest.share()
                    )
                },
            );
            md.push(format!(
                "| {} | {:.3} | {calls} | {under} | {}{:.0}% | {rest} |",
                r.round,
                ns_ms(s.ns()),
                if s.unsure() { "≥ " } else { "" },
                pct(s.explained, s.ns()),
            ));
        }
    }
    md.push(String::new());
    for r in rounds {
        let kept = r.kept() as u64;
        if kept < r.counted {
            md.push(format!(
                "- {}: the tally's ring kept the last {kept} of the round's {} slow calls; the \
                 {} before them are not lined up.",
                r.round,
                r.counted,
                r.counted - kept
            ));
        }
        for l in r.rings.iter().filter(|l| l.missed > 0) {
            md.push(format!(
                "- {}: the {} ring lost {} of the round's {} slow flushes, {} (the window {} to \
                 {}); it holds the other {}.",
                r.round,
                l.process,
                l.missed,
                l.window(),
                indices_text(&l.missing),
                l.from,
                l.recorded.saturating_sub(1),
                l.window() - l.missed
            ));
        }
        for l in r.rings.iter().filter(|l| l.running > 0) {
            md.push(format!(
                "- {}: the {} process was still running {} flush call(s) at the round's \
                 reading, which no ring holds yet; each may have begun under any of the round's \
                 stalls.",
                r.round, l.process, l.running
            ));
        }
    }
    let stalls: Vec<&Stall> = rounds.iter().flat_map(|r| &r.stalls).collect();
    let total: u64 = stalls.iter().map(|s| s.ns()).sum();
    let explained: u64 = stalls.iter().map(|s| s.explained).sum();
    let rest = chance_ns(rounds).map_or_else(
        || "and their rounds have no baseline (another process's clock).".to_string(),
        |chance| {
            format!(
                "where the rest of their rounds gives {}{:.0}%.",
                if rounds.iter().all(|r| r.whole) {
                    ""
                } else {
                    "at least "
                },
                pct(chance, total)
            )
        },
    );
    md.push(format!(
        "{} slow call(s) counted, {} kept, in {} stall(s), {} with a flush under it, {} with \
         one lost, {} with a flush call still running; {}{:.0}% of their {:.3} ms ran under a \
         flush, {rest}\n",
        rounds.iter().map(|r| r.counted).sum::<u64>(),
        rounds.iter().map(RoundCalls::kept).sum::<usize>(),
        stalls.len(),
        stalls.iter().filter(|s| !s.under.is_empty()).count(),
        stalls.iter().filter(|s| s.lost > 0).count(),
        stalls.iter().filter(|s| s.running > 0).count(),
        if stalls.iter().any(|s| s.unsure()) {
            "at least "
        } else {
            ""
        },
        pct(explained, total),
        ns_ms(total),
    ));
    md
}

/// Runs of indices (each its first and last) as the entry names them: `index 128`, `indices 0
/// to 35`, `indices 0 to 35 and 40`, `indices 2, 5 to 6 and 9`.
fn indices_text(runs: &[(u64, u64)]) -> String {
    let each: Vec<String> = runs
        .iter()
        .map(|&(first, last)| {
            if first == last {
                first.to_string()
            } else {
                format!("{first} to {last}")
            }
        })
        .collect();
    let joined = match each.split_last() {
        Some((last, [])) => last.clone(),
        Some((last, rest)) => format!("{} and {last}", rest.join(", ")),
        None => String::new(),
    };
    let one = matches!(runs, [(first, last)] if first == last);
    format!("{} {joined}", if one { "index" } else { "indices" })
}

#[cfg(test)]
mod tests {
    /// THE DEFAULT BOOT SETS NO TALLY: a run that names no `tally=` boots every world with its
    /// own seams alone, as the entries before the knob did, and `tally=on` adds both tallies'
    /// once each. NEGATIVE CONTROL: an env that let the knob's seams into the default boot fails
    /// the first half.
    #[test]
    fn the_default_boot_sets_no_tally_and_the_knob_sets_both() {
        use super::*;

        let quit_bye = ("ATERM_DEBUG_QUIT_BYE", "handoff");
        let off = crate::spec::parse("", std::path::Path::new("/o")).expect("the default run");
        for own in [&[][..], &[quit_bye][..], &[(wait_tally::SEAM, "1")][..]] {
            assert_eq!(world_env(&off, own), own.to_vec(), "{own:?}");
        }
        assert!(
            !world_env(&off, &[quit_bye])
                .iter()
                .any(|&(k, _)| k == wait_tally::SEAM || k == HANDLER_SEAM),
            "the default handoff boot counts nothing"
        );
        let on = crate::spec::parse("tally=on", std::path::Path::new("/o")).expect("the knob");
        assert_eq!(
            world_env(&on, &[quit_bye]),
            vec![quit_bye, (wait_tally::SEAM, "1"), (HANDLER_SEAM, "1")]
        );
        assert_eq!(
            world_env(&on, &[(wait_tally::SEAM, "1")]),
            vec![(wait_tally::SEAM, "1"), (HANDLER_SEAM, "1")],
            "the ask world's own wait tally, once"
        );
    }

    /// A round counts from the reading before it while the process is the same, and from a new
    /// process's start once it is not; the next round counts from this one's reading.
    #[test]
    fn a_round_is_its_readings_difference_or_a_new_processs_whole_life() {
        use super::*;

        let hist = |values: &[u64]| {
            let mut h = Hist::default();
            for &v in values {
                h.add(v, Unit::Ns);
            }
            h
        };
        let mut t = Tallies {
            wait: PathBuf::new(),
            handlers: PathBuf::new(),
            last: None,
            marks: Vec::new(),
            rounds: Vec::new(),
            fences: 0,
        };
        let reading = |bridge: u32, aterm: u32, status: &[u64], fleet: &[u64]| {
            let mut waits = Tally {
                pid: bridge,
                ..Tally::default()
            };
            waits.rows[TallyRow::LaneStatus.index()] = hist(status);
            let mut handlers = HandlerTally {
                pid: aterm,
                ..HandlerTally::default()
            };
            handlers.rows[HandlerRow::FleetMail.index()] = hist(fleet);
            Reading {
                waits,
                handlers,
                bench: Flushed::default(),
                at: 0,
            }
        };
        t.rebase(reading(10, 7, &[5_000], &[2_000]));
        let mut flushed = reading(10, 7, &[5_000, 9_000], &[2_000]);
        flushed.waits.flushes.full = 3;
        flushed.waits.flushes.barrier = 5;
        flushed.waits.flushes.plain = 2;
        t.round("g", "1".into(), flushed);
        t.round("g", "2".into(), reading(11, 7, &[3_000], &[2_000, 4_000]));
        let [one, two] = &t.rounds[..] else {
            panic!("two rounds")
        };
        assert_eq!(
            (one.new_bridge, one.waits.row(TallyRow::LaneStatus).count),
            (false, 1),
            "the same bridge: the round's one status read"
        );
        assert_eq!(one.waits.row(TallyRow::LaneStatus).total, 9_000);
        assert_eq!(one.handlers.row(HandlerRow::FleetMail).count, 0);
        assert_eq!(
            (two.new_bridge, two.waits.row(TallyRow::LaneStatus).count),
            (true, 1),
            "a restarted bridge: its whole life"
        );
        assert_eq!(
            (two.new_aterm, two.handlers.row(HandlerRow::FleetMail).total),
            (false, 4_000)
        );
        let md = t.md().join("\n");
        assert!(md.contains("#### Both tallies, g (`tally=on`)"), "{md}");
        assert!(md.contains("**all 2**"), "{md}");
        assert!(md.contains("(pid 11, a new process)"), "{md}");
        let json = t.json();
        assert_eq!(
            json["rounds"][1]["wait"]["rows"]["lane_status"]["count"].as_u64(),
            Some(1)
        );
        // Every kind the tally counts, the plain fsync(2)s too, beside the claim it counts all.
        let flushes = &json["rounds"][0]["wait"]["astream_flushes"];
        assert_eq!(
            ["full", "barrier", "plain"].map(|k| flushes[k].as_u64()),
            [Some(3), Some(5), Some(2)],
            "{flushes}"
        );
        assert_eq!(flushes["counted"].as_str(), Some("all"), "{flushes}");
        assert_eq!(
            json["rounds"][0]["handlers"]["rows"]
                .as_object()
                .map(aterm_json::Map::len),
            Some(0),
            "a row that counted nothing is left out"
        );
    }

    /// A READING TAKES THE BRIDGE'S FIRST LINE PAST ITS FENCE: the line on file when it began,
    /// which a bridge parked since still shows, is refused with its cursor through, and so is a
    /// later write that took no broker record; a line that took one, parked or queued, is taken
    /// once its cursor is through, and with no line on file then, the first one is. NEGATIVE
    /// CONTROL: the cursor alone (no line on file to be past), what a reading took before the
    /// fence, takes the stale line, and the park in progress counted in the next round.
    #[test]
    fn a_reading_takes_the_bridges_first_line_past_its_fence() {
        use super::*;

        let line = |at_ns, committed, wakes: &[u64], queued: &[u64]| {
            let mut t = Tally {
                at_ns,
                committed,
                ..Tally::default()
            };
            for (row, waits) in [
                (TallyRow::WakeRecord, wakes),
                (TallyRow::QueuedRecord, queued),
            ] {
                for &ns in waits {
                    t.rows[row.index()].add(ns, Unit::Ns);
                }
            }
            t
        };
        let on_file = line(5_000_000, 40, &[9_000], &[]);
        let ready = fenced(40, Some(on_file));
        assert!(
            ready(&on_file).is_err(),
            "the line on file when the reading began"
        );
        assert!(
            ready(&line(5_000_001, 40, &[9_000], &[])).is_err(),
            "a later write that took no record"
        );
        assert!(
            ready(&line(5_000_001, 39, &[9_000, 30_000], &[])).is_err(),
            "a record taken, short of the cursor"
        );
        assert_eq!(ready(&line(5_000_001, 40, &[9_000, 30_000], &[])), Ok(()));
        assert_eq!(ready(&line(5_000_001, 40, &[9_000], &[4_000])), Ok(()));
        assert!(fenced(40, None)(&line(1, 39, &[], &[])).is_err());
        assert_eq!(
            fenced(40, None)(&on_file),
            Ok(()),
            "the cursor alone takes the stale line"
        );
    }

    /// THE FENCE RECORD IS NO HALT: it goes under the fleet filter every bridge reads live, and
    /// no bridge reads it as a person's halt row. NEGATIVE CONTROL: a halt row's subject parses.
    #[test]
    fn the_fence_record_is_under_the_fleet_filter_and_no_halt() {
        use super::*;

        let filter = aterm_link::subject::fleet_filter(FLEET);
        let subject = fence_subject();
        assert!(
            subject.starts_with(filter.trim_end_matches('>')),
            "{subject} under {filter}"
        );
        assert!(aterm_link::subject::parse_halt(FLEET, &subject, b"state=on").is_none());
        let halt = format!("/f/{FLEET}/fleet/h-bench/halt");
        assert!(aterm_link::subject::parse_halt(FLEET, &halt, b"state=on").is_some());
    }

    /// EACH ROW NAMES ITS UNIT: the JSON carries every row's `unit` beside its `total` and `max`
    /// in that unit, as the ping-pong report's does, so a count row's posts are not read as
    /// nanoseconds. NEGATIVE CONTROL: a time row says `ns`.
    #[test]
    fn each_rows_json_names_its_unit() {
        use super::*;

        let mut t = Tallies {
            wait: PathBuf::new(),
            handlers: PathBuf::new(),
            last: None,
            marks: Vec::new(),
            rounds: Vec::new(),
            fences: 0,
        };
        let idle = Reading {
            waits: Tally {
                pid: 10,
                ..Tally::default()
            },
            handlers: HandlerTally {
                pid: 7,
                ..HandlerTally::default()
            },
            bench: Flushed::default(),
            at: 0,
        };
        let mut now = idle;
        for posts in [13, 9] {
            now.waits.rows[TallyRow::DrainPosts.index()].add(posts, Unit::Count);
        }
        now.waits.rows[TallyRow::Park.index()].add(2_000, Unit::Ns);
        now.handlers.rows[HandlerRow::Deliver.index()].add(5_000, Unit::Ns);
        t.rebase(idle);
        t.round("g", "1".into(), now);
        let json = t.json();
        let rows = &json["rounds"][0]["wait"]["rows"];
        let drain = &rows["drain_posts"];
        assert_eq!(drain["unit"].as_str(), Some("count"), "{drain}");
        assert_eq!(
            (drain["total"].as_u64(), drain["max"].as_u64()),
            (Some(22), Some(13))
        );
        assert!(drain["total_ns"].is_null(), "{drain}");
        assert_eq!(rows["park"]["unit"].as_str(), Some("ns"));
        assert_eq!(rows["park"]["total"].as_u64(), Some(2_000));
        let deliver = &json["rounds"][0]["handlers"]["rows"]["deliver"];
        assert_eq!(deliver["unit"].as_str(), Some("ns"), "{deliver}");
    }

    /// SLOW CALLS THAT OVERLAP ARE ONE STALL (E09): an ack and its commit time one span, and a
    /// fence holds the replace it makes, so each pair is one stall, counted and timed once; a
    /// call that starts where another ends is a stall of its own.
    #[test]
    fn overlapping_slow_calls_are_one_stall() {
        use super::*;
        let ms = 1_000_000;
        let call = |row, start_ns, ns| wait_tally::SlowCall { row, start_ns, ns };
        let joined = Stall::join([
            call(TallyRow::DirAppend, 9 * ms, ms + ms / 2),
            call(TallyRow::Commit, ms, 4 * ms),
            call(TallyRow::DirReplace, 6 * ms + ms / 10, 2 * ms),
            call(TallyRow::Ack, ms, 4 * ms),
            call(TallyRow::LogpinFence, 6 * ms, 3 * ms),
        ]);
        let rows: Vec<Vec<TallyRow>> = joined
            .iter()
            .map(|s| s.iter().map(|c| c.row).collect())
            .collect();
        assert_eq!(
            rows,
            [
                vec![TallyRow::Ack, TallyRow::Commit],
                vec![TallyRow::LogpinFence, TallyRow::DirReplace],
                vec![TallyRow::DirAppend],
            ],
            "the outer call first in each stall; one that starts at an end stands alone"
        );
        let stalls: Vec<Stall> = joined.into_iter().map(|c| Stall::of(c, &[])).collect();
        assert_eq!(
            stalls.iter().map(|s| (s.start, s.ns())).collect::<Vec<_>>(),
            [(ms, 4 * ms), (6 * ms, 3 * ms), (9 * ms, ms + ms / 2)],
            "each stall's span, once"
        );
    }

    /// A STALL IS LINED UP WITH THE FLUSHES UNDER IT (E09): a flush that overlaps it counts by
    /// its overlap, two overlapping flushes count their union once, and one that ends at its
    /// start or begins at its end is not under it. The baseline is the rest of the round: the
    /// union of every kept flush over the round's time outside its stalls; a flush under a
    /// stall never counts in it. A ring that turned over says how many slow flushes it lost
    /// that could have run under a stall, by their indices (E09c): with no reading before the
    /// stall, the one flush it turned over could have, so the stall is `none kept, 1 lost`
    /// and both its share and the baseline are floors. NEGATIVE CONTROLS: the same stall with
    /// empty rings is under nothing, explained 0, at a rate of 0; and the same turned-over ring
    /// after a reading that had counted the lost flush (so it ended before the stall began)
    /// lost none that could have, and its stall is `none`.
    #[test]
    fn a_stall_names_the_flushes_under_it_and_the_rest_of_its_round_is_the_baseline() {
        use super::*;
        /// A round of `calls` with `bridge` (the bridge's ring at the round's reading, pid 0's) and
        /// `bench`, over `window`, after the readings `marks`.
        fn slow_round(
            round: &str,
            calls: wait_tally::SlowCalls,
            bridge: Flushed,
            bench: Flushed,
            window: (u64, u64),
            marks: Vec<Mark>,
        ) -> SlowRound {
            SlowRound {
                group: "g".to_string(),
                round: round.to_string(),
                calls,
                bridge_pid: 0,
                bridge,
                bench,
                window,
                marks,
            }
        }

        /// A slow flush of `kind` with its index.
        const fn flush(
            index: u64,
            kind: wait_tally::FlushKind,
            start_ns: u64,
            end_ns: u64,
        ) -> wait_tally::SlowFlush {
            wait_tally::SlowFlush {
                index,
                kind,
                start_ns,
                end_ns,
            }
        }

        /// A ring of `count` recorded slow flushes holding `held`, oldest first.
        fn ring(count: u64, held: &[wait_tally::SlowFlush]) -> Flushed {
            let mut r = Flushed {
                slow_count: count,
                ..Flushed::default()
            };
            for (slot, f) in r.slow.iter_mut().zip(held) {
                *slot = Some(*f);
            }
            r
        }

        /// One slow call of `row`, counted and kept.
        fn one_call(row: TallyRow, start_ns: u64, ns: u64) -> wait_tally::SlowCalls {
            let mut calls = wait_tally::SlowCalls::default();
            calls.kept[0] = Some(wait_tally::SlowCall { row, start_ns, ns });
            calls.count = 1;
            calls
        }
        use wait_tally::FlushKind::{Barrier, Full, Plain};
        let ms = 1_000_000;
        let bridge = ring(
            2,
            &[
                flush(0, Full, 0, ms),
                flush(1, Barrier, 3 * ms / 2, 5 * ms / 2),
            ],
        );
        let bench = ring(
            2,
            &[
                flush(0, Full, 2 * ms, 3 * ms),
                flush(1, Plain, 5 * ms, 6 * ms),
            ],
        );
        let mut calls = wait_tally::SlowCalls::default();
        for (i, row) in [TallyRow::Ack, TallyRow::Commit].into_iter().enumerate() {
            calls.kept[i] = Some(wait_tally::SlowCall {
                row,
                start_ns: ms,
                ns: 4 * ms,
            });
            calls.count += 1;
        }
        let r = RoundCalls::on(
            &slow_round("1", calls, bridge, bench, (0, 10 * ms), Vec::new()),
            true,
        );
        assert_eq!(
            (r.counted, r.kept(), r.stalls.len()),
            (2, 2, 1),
            "an ack and its commit"
        );
        let s = &r.stalls[0];
        let named: Vec<(&str, u64)> = s.under.iter().map(|u| (u.process, u.overlap)).collect();
        assert_eq!(named, [("bridge", ms), ("bench", ms)], "the two under it");
        assert_eq!(s.explained, 3 * ms / 2, "1.5..3 ms, their union, once");
        assert!(s.lost == 0 && r.whole);
        assert_eq!(
            r.rest.map(|c| (c.covered, c.span)),
            Some((2 * ms, 6 * ms)),
            "0..1 and 5..6 ms of the round's 6 outside its stall; 1.5..3 is the stall's"
        );
        let none = RoundCalls::on(
            &slow_round(
                "1",
                calls,
                Flushed::default(),
                Flushed::default(),
                (0, 10 * ms),
                Vec::new(),
            ),
            true,
        );
        let s = &none.stalls[0];
        assert!(
            s.under.is_empty() && s.explained == 0 && none.rest.is_some_and(|c| c.covered == 0),
            "CONTROL: empty rings"
        );
        assert!(none.whole, "an empty ring kept every slow flush: none");
        let held: Vec<wait_tally::SlowFlush> = (0..wait_tally::RING as u64)
            .map(|i| {
                let at = 10 * ms + 2 * ms * i;
                flush(i + 1, Barrier, at, at + ms)
            })
            .collect();
        let turned = ring(wait_tally::RING as u64 + 1, &held);
        let early = RoundCalls::on(
            &slow_round(
                "2",
                one_call(TallyRow::Ack, ms, 4 * ms),
                turned,
                Flushed::default(),
                (0, 13 * ms),
                Vec::new(),
            ),
            true,
        );
        assert!(
            early.stalls[0].lost == 1 && early.stalls[0].under.is_empty(),
            "the ring lost index 0, which could have run under the stall"
        );
        assert_eq!(
            (early.rest.map(|c| (c.covered, c.span)), early.whole),
            (Some((2 * ms, 9 * ms)), false),
            "10..11 and 12..13 ms kept, index 0 lost: a floor"
        );
        assert_eq!(
            early.rings[0],
            RingLine {
                process: "bridge",
                kept: wait_tally::RING,
                recorded: wait_tally::RING as u64 + 1,
                from: 0,
                missed: 1,
                missing: vec![(0, 0)],
                running: 0,
            }
        );
        let marked = RoundCalls::on(
            &slow_round(
                "3",
                one_call(TallyRow::Ack, ms, 4 * ms),
                turned,
                Flushed::default(),
                (0, 13 * ms),
                vec![Mark {
                    at: 0,
                    bench: 0,
                    bridge_pid: 0,
                    bridge: 1,
                }],
            ),
            true,
        );
        assert!(
            marked.whole && marked.stalls[0].lost == 0,
            "CONTROL: the reading at 0 ms had counted index 0, so it ended before the stall"
        );
        let md = slow_calls_md("g", &[r, early, marked]);
        assert!(
            md.iter().any(|l| l.contains(
                "| 1 | 4.000 | ack 4.000, commit 4.000 | bridge barrier 1.00: 1.00; bench full \
                 1.00: 1.00 | 38% | 33% |"
            )),
            "{md:?}"
        );
        assert!(
            md.iter().any(|l| l.contains(
                "| 2 | 4.000 | ack 4.000 | none kept, 1 lost | ≥ 0% | ≥ 22% |"
            )),
            "{md:?}"
        );
        assert!(
            md.iter()
                .any(|l| l.contains("| 3 | 4.000 | ack 4.000 | none | 0% | 22% |")),
            "{md:?}"
        );
        assert!(
            md.iter().any(|l| l.contains(
                "- 2: the bridge ring lost 1 of the round's 65 slow flushes, index 0 (the window 0 \
                 to 64); it holds the other 64."
            )),
            "{md:?}"
        );
        assert!(
            md.iter().any(|l| l.starts_with(
                "4 slow call(s) counted, 4 kept, in 3 stall(s), 1 with a flush under it, 1 with \
                 one lost, 0 with a flush call still running; at least 12% of their 12.000 ms \
                 ran under a flush, where the rest of their rounds gives at least 26%."
            )),
            "{md:?}"
        );
        assert!(
            !md.iter().any(|l| l.contains("not lined up")),
            "every call kept: {md:?}"
        );
    }

    /// EVERY SLOW CALL OF A ROUND IS LISTED, not only the slowest of its row (E09): a round's
    /// 2 ms delivery under the 40 ms one before the round is its slow call, and a round with
    /// more slow calls than the ring keeps says how many were not lined up. A round that counted
    /// none is left out. NEGATIVE CONTROL: the earlier 40 ms call, the earlier reading's, is not
    /// this round's.
    #[test]
    fn every_slow_call_of_a_round_is_listed_and_one_the_ring_lost_is_counted() {
        use super::*;
        let ms = 1_000_000;
        let mut t = Tallies {
            wait: PathBuf::new(),
            handlers: PathBuf::new(),
            last: None,
            marks: Vec::new(),
            rounds: Vec::new(),
            fences: 0,
        };
        let with = |mut waits: Tally, row: TallyRow, start_ns: u64, ns: u64| {
            waits.rows[row.index()].add(ns, Unit::Ns);
            let n = waits.calls.len();
            if n == wait_tally::CALLS_KEPT {
                waits.calls.kept.copy_within(1.., 0);
                waits.calls.kept[n - 1] = Some(wait_tally::SlowCall { row, start_ns, ns });
            } else {
                waits.calls.kept[n] = Some(wait_tally::SlowCall { row, start_ns, ns });
            }
            waits.calls.count += 1;
            waits
        };
        let reading = |waits, at| Reading {
            waits,
            handlers: HandlerTally::default(),
            bench: Flushed::default(),
            at,
        };
        let earlier = with(Tally::default(), TallyRow::LaneDeliver, ms, 40 * ms);
        let mut later = with(earlier, TallyRow::LaneDeliver, 60 * ms, 2 * ms);
        later.rows[TallyRow::Park.index()].add(500 * ms, Unit::Ns);
        t.rebase(reading(earlier, 50 * ms));
        t.round("g", "1".into(), reading(later, 100 * ms));
        t.round("g", "quiet".into(), reading(later, 110 * ms));
        let mut busy = later;
        for i in 0..wait_tally::CALLS_KEPT as u64 + 3 {
            busy = with(busy, TallyRow::DirAppend, 200 * ms + 3 * i * ms, 2 * ms);
        }
        t.round("g", "busy".into(), reading(busy, 700 * ms));
        let rounds = t.slow_calls();
        assert_eq!(
            rounds
                .iter()
                .map(|r| (r.round.as_str(), r.counted, r.kept()))
                .collect::<Vec<_>>(),
            [
                ("1", 1, 1),
                (
                    "busy",
                    wait_tally::CALLS_KEPT as u64 + 3,
                    wait_tally::CALLS_KEPT
                )
            ],
            "the quiet round left out"
        );
        assert_eq!(
            rounds[0].stalls[0].calls,
            [wait_tally::SlowCall {
                row: TallyRow::LaneDeliver,
                start_ns: 60 * ms,
                ns: 2 * ms
            }],
            "the round's own slow call, under its row's earlier largest; the earlier reading's \
             40 ms call is not this round's"
        );
        assert_eq!(
            rounds[0].rest.map(|c| c.span),
            wait_tally::ACROSS_PROCESSES.then_some(48 * ms),
            "the round's 50 ms less its stall"
        );
        let md = slow_calls_md("g", &rounds);
        assert!(
            md.iter().any(|l| l.contains(&format!(
                "- busy: the tally's ring kept the last {} of the round's {} slow calls; the 3 \
                 before them are not lined up.",
                wait_tally::CALLS_KEPT,
                wait_tally::CALLS_KEPT + 3
            ))),
            "{md:?}"
        );
        let json = t.json();
        assert_eq!(
            json["slow_calls"]["summary"]["calls_counted"].as_u64(),
            Some(wait_tally::CALLS_KEPT as u64 + 4)
        );
        assert_eq!(
            json["slow_calls"]["calls_kept"].as_u64(),
            Some(wait_tally::CALLS_KEPT as u64),
            "the ring's size, which the `what` names as calls_kept"
        );
        assert_eq!(
            json["slow_calls"]["rounds"][0]["stalls"][0]["calls"][0]["row"].as_str(),
            Some("lane_deliver")
        );
    }

    /// A RING IS JUDGED BY ITS INDICES FROM THE READING BEFORE (E09c): a round's window starts
    /// at that reading's count, so a ring that turned over past flushes from before the round
    /// is whole, where E09's rule (every flush the process ever recorded held) called every ring
    /// past 64 short. A reading that raced a flush's write counts it and holds none, and a ring
    /// that turned over past one of the round's own loses it: either way the ring is not
    /// whole, a stall under no kept flush is `none kept, 1 lost`, its share and the baseline
    /// are `≥`, and the JSON names the window and the loss. NEGATIVE CONTROL: the turned-over
    /// ring judged from the process's start (no reading before) has lost a flush.
    #[test]
    fn a_ring_is_judged_by_its_indices_from_the_reading_before() {
        use super::*;
        /// A round of `calls` with `bridge` (the bridge's ring at the round's reading, pid 0's) and
        /// `bench`, over `window`, after the readings `marks`.
        fn slow_round(
            round: &str,
            calls: wait_tally::SlowCalls,
            bridge: Flushed,
            bench: Flushed,
            window: (u64, u64),
            marks: Vec<Mark>,
        ) -> SlowRound {
            SlowRound {
                group: "g".to_string(),
                round: round.to_string(),
                calls,
                bridge_pid: 0,
                bridge,
                bench,
                window,
                marks,
            }
        }

        /// A slow flush of `kind` with its index.
        const fn flush(
            index: u64,
            kind: wait_tally::FlushKind,
            start_ns: u64,
            end_ns: u64,
        ) -> wait_tally::SlowFlush {
            wait_tally::SlowFlush {
                index,
                kind,
                start_ns,
                end_ns,
            }
        }

        /// A ring of `count` recorded slow flushes holding `held`, oldest first.
        fn ring(count: u64, held: &[wait_tally::SlowFlush]) -> Flushed {
            let mut r = Flushed {
                slow_count: count,
                ..Flushed::default()
            };
            for (slot, f) in r.slow.iter_mut().zip(held) {
                *slot = Some(*f);
            }
            r
        }

        /// One slow call of `row`, counted and kept.
        fn one_call(row: TallyRow, start_ns: u64, ns: u64) -> wait_tally::SlowCalls {
            let mut calls = wait_tally::SlowCalls::default();
            calls.kept[0] = Some(wait_tally::SlowCall { row, start_ns, ns });
            calls.count = 1;
            calls
        }
        use wait_tally::FlushKind::Full;
        let ms = 1_000_000;
        let calls = one_call(TallyRow::Ack, 20 * ms, 2 * ms);
        let before = vec![Mark {
            at: 10 * ms,
            bench: 0,
            bridge_pid: 0,
            bridge: 1,
        }];
        let round = |bridge: Flushed, marks: Vec<Mark>| {
            RoundCalls::on(
                &slow_round(
                    "1",
                    calls,
                    bridge,
                    Flushed::default(),
                    (10 * ms, 30 * ms),
                    marks,
                ),
                true,
            )
        };
        let two = [flush(0, Full, 0, ms), flush(1, Full, 12 * ms, 13 * ms)];
        let kept = round(ring(2, &two), before.clone());
        assert!(kept.whole, "the round's one flush, index 1, held");
        assert_eq!(
            kept.rest.map(|c| (c.covered, c.span)),
            Some((ms, 18 * ms)),
            "12..13 ms of the round's 20 less its stall"
        );
        let raced = round(ring(3, &two), before.clone());
        assert!(
            !raced.whole && raced.stalls[0].lost == 1,
            "a reading that raced index 2's write: counted, not held"
        );
        let held: Vec<wait_tally::SlowFlush> = (0..wait_tally::RING as u64)
            .map(|i| flush(i + 1, Full, 0, ms))
            .collect();
        let turned = ring(wait_tally::RING as u64 + 1, &held);
        let control = round(turned, before.clone());
        assert!(
            control.whole,
            "index 0, the one turned over, ended before the reading that opened the round"
        );
        assert!(
            !round(turned, Vec::new()).whole,
            "CONTROL: from the process's start, index 0 is lost"
        );
        let past: Vec<wait_tally::SlowFlush> = (0..wait_tally::RING as u64)
            .map(|i| flush(i + 2, Full, 0, ms))
            .collect();
        assert!(
            !round(ring(wait_tally::RING as u64 + 2, &past), before).whole,
            "a ring turned over past the round's own index 1"
        );
        let json = rounds_json(std::slice::from_ref(&raced));
        assert_eq!(
            json[0]["flush_rings"],
            json!([
                {"process": "bridge", "kept": 2, "recorded": 3, "from": 1, "missed": 1,
                 "missing": [[2, 2]], "running": 0},
                {"process": "bench", "kept": 0, "recorded": 0, "from": 0, "missed": 0,
                 "missing": [], "running": 0}
            ])
        );
        assert_eq!(json[0]["rest_of_round"]["complete"], json!(false));
        assert_eq!(json[0]["stalls"][0]["lost"], json!(1));
        let md = slow_calls_md("g", &[raced, control]);
        assert!(
            md.iter()
                .any(|l| l.contains("| 1 | 2.000 | ack 2.000 | none kept, 1 lost | ≥ 0% | ≥ 6% |")),
            "{md:?}"
        );
        assert!(
            md.iter()
                .any(|l| l.contains("| 1 | 2.000 | ack 2.000 | none | 0% | 0% |")),
            "{md:?}"
        );
        assert!(
            md.iter()
                .any(|l| l.contains("rest of their rounds gives at least 3%.")),
            "{md:?}"
        );
    }

    /// A STALL IS JUDGED FROM THE LATEST READING AT OR BEFORE IT (E09c): a stall that began
    /// before the round's opening reading, at 15 ms between the readings at 10 and 20 ms, is
    /// judged from the one at 10 (count 5), so the ring that lost index 5 lost one that could
    /// have run under it; one that began after both, from the one at 20 (count 8); one that
    /// began before every reading, from the process's start. NEGATIVE CONTROL: the reading at
    /// 20 ms, which a rule taking the last reading whatever its time would judge all three
    /// from, calls the first two whole.
    #[test]
    fn a_stall_is_judged_from_the_latest_reading_at_or_before_it() {
        use super::*;
        /// A slow flush of `kind` with its index.
        const fn flush(
            index: u64,
            kind: wait_tally::FlushKind,
            start_ns: u64,
            end_ns: u64,
        ) -> wait_tally::SlowFlush {
            wait_tally::SlowFlush {
                index,
                kind,
                start_ns,
                end_ns,
            }
        }

        /// A ring of `count` recorded slow flushes holding `held`, oldest first.
        fn ring(count: u64, held: &[wait_tally::SlowFlush]) -> Flushed {
            let mut r = Flushed {
                slow_count: count,
                ..Flushed::default()
            };
            for (slot, f) in r.slow.iter_mut().zip(held) {
                *slot = Some(*f);
            }
            r
        }
        use wait_tally::FlushKind::Full;
        let ms = 1_000_000;
        let held: Vec<wait_tally::SlowFlush> = (6..10u64)
            .map(|i| {
                let at = 40 * ms + 2 * (i - 6) * ms;
                flush(i, Full, at, at + ms)
            })
            .collect();
        let bench = ring(10, &held);
        let mark = |at: u64, bench: u64| Mark {
            at,
            bench,
            bridge_pid: 0,
            bridge: 0,
        };
        let marks = vec![mark(10 * ms, 5), mark(20 * ms, 8)];
        let r = Ring {
            process: "bench",
            flushes: &bench,
            pid: None,
            marks: &marks,
        };
        assert_eq!(
            [5, 10, 15, 20, 25].map(|t| r.from(t * ms)),
            [0, 5, 5, 8, 8],
            "the latest reading at or before each time, the process's start before any"
        );
        let mut calls = wait_tally::SlowCalls::default();
        for (i, start) in [5, 15, 25].into_iter().enumerate() {
            calls.kept[i] = Some(wait_tally::SlowCall {
                row: TallyRow::Ack,
                start_ns: start * ms,
                ns: ms,
            });
            calls.count += 1;
        }
        let round = RoundCalls::on(
            &SlowRound {
                group: "g".to_string(),
                round: "1".to_string(),
                calls,
                bridge_pid: 0,
                bridge: Flushed::default(),
                bench,
                window: (20 * ms, 50 * ms),
                marks: marks.clone(),
            },
            true,
        );
        assert_eq!(
            round.stalls.iter().map(|s| s.lost).collect::<Vec<_>>(),
            [6, 1, 0],
            "indices 0 to 5 could have run under the first, 5 under the second, none lost under \
             the third"
        );
        assert!(!round.whole);
        assert_eq!(
            (round.rings[1].from, round.rings[1].missed),
            (8, 0),
            "the round's own window, from its opening reading, is whole"
        );
        assert_eq!(
            bench.missed(marks[1].bench),
            0,
            "CONTROL: judged from the reading at 20 ms, the earlier stalls would lose nothing"
        );
    }

    /// A RELAUNCHED BRIDGE'S RING IS JUDGED FROM ITS OWN START (E09c): a reading of another
    /// bridge's (pid 7, 50 slow flushes) says nothing of the new one's (pid 9), whose ring lost
    /// its index 0. NEGATIVE CONTROLS: a reading of the new bridge's own that had counted index
    /// 0 calls the ring whole, and the old bridge's count, taken as the new one's, would too.
    #[test]
    fn a_relaunched_bridges_ring_is_judged_from_its_own_start() {
        use super::*;
        /// A slow flush of `kind` with its index.
        const fn flush(
            index: u64,
            kind: wait_tally::FlushKind,
            start_ns: u64,
            end_ns: u64,
        ) -> wait_tally::SlowFlush {
            wait_tally::SlowFlush {
                index,
                kind,
                start_ns,
                end_ns,
            }
        }

        /// A ring of `count` recorded slow flushes holding `held`, oldest first.
        fn ring(count: u64, held: &[wait_tally::SlowFlush]) -> Flushed {
            let mut r = Flushed {
                slow_count: count,
                ..Flushed::default()
            };
            for (slot, f) in r.slow.iter_mut().zip(held) {
                *slot = Some(*f);
            }
            r
        }
        use wait_tally::FlushKind::Full;
        let ms = 1_000_000;
        let bridge = ring(3, &[flush(1, Full, 0, ms), flush(2, Full, 2 * ms, 3 * ms)]);
        let mut calls = wait_tally::SlowCalls::default();
        calls.kept[0] = Some(wait_tally::SlowCall {
            row: TallyRow::Ack,
            start_ns: 20 * ms,
            ns: 2 * ms,
        });
        calls.count = 1;
        let round = |bridge_pid: u32, bridge_count: u64| {
            RoundCalls::on(
                &SlowRound {
                    group: "g".to_string(),
                    round: "1".to_string(),
                    calls,
                    bridge_pid: 9,
                    bridge,
                    bench: Flushed::default(),
                    window: (10 * ms, 30 * ms),
                    marks: vec![Mark {
                        at: 10 * ms,
                        bench: 0,
                        bridge_pid,
                        bridge: bridge_count,
                    }],
                },
                true,
            )
        };
        let relaunched = round(7, 50);
        assert_eq!(
            (relaunched.rings[0].from, relaunched.rings[0].missed),
            (0, 1),
            "from the new process's start, index 0 lost"
        );
        assert!(!relaunched.whole && relaunched.stalls[0].lost == 1);
        let own = round(9, 1);
        assert!(
            own.whole && own.stalls[0].lost == 0,
            "CONTROL: the new bridge's own reading had counted index 0"
        );
        assert_eq!(
            bridge.missed(50),
            0,
            "CONTROL: the old bridge's count, taken as the new one's, would call it whole"
        );
    }

    /// A FLUSH CALL RUNNING AT THE ROUND'S READING LEAVES NO RING WHOLE (E09c review): a broker
    /// flush that began under a stall and still ran at the reading has no index and no ring
    /// place, so the reading's count of calls running is all that names it. The stall is `none
    /// kept, 1 running`, its share and the baseline `≥`, the ring line says so, the JSON counts
    /// it, and the round is not whole. NEGATIVE CONTROL: the same round with no call running is
    /// `none`, whole.
    #[test]
    fn a_flush_call_running_at_the_rounds_reading_leaves_no_ring_whole() {
        use super::*;
        let ms = 1_000_000;
        let mut calls = wait_tally::SlowCalls::default();
        calls.kept[0] = Some(wait_tally::SlowCall {
            row: TallyRow::Ack,
            start_ns: 20 * ms,
            ns: 2 * ms,
        });
        calls.count = 1;
        let round = |running: u64| {
            RoundCalls::on(
                &SlowRound {
                    group: "g".to_string(),
                    round: "1".to_string(),
                    calls,
                    bridge_pid: 0,
                    bridge: Flushed::default(),
                    bench: Flushed {
                        running,
                        ..Flushed::default()
                    },
                    window: (10 * ms, 30 * ms),
                    marks: vec![Mark {
                        at: 10 * ms,
                        bench: 0,
                        bridge_pid: 0,
                        bridge: 0,
                    }],
                },
                true,
            )
        };
        let running = round(1);
        let s = &running.stalls[0];
        assert_eq!((s.lost, s.running, s.under.len()), (0, 1, 0));
        assert!(!running.whole, "a call that may have run under the stall");
        assert_eq!(running.rings[1].running, 1, "the bench's ring line");
        let json = rounds_json(std::slice::from_ref(&running));
        assert_eq!(json[0]["stalls"][0]["running"], json!(1));
        assert_eq!(json[0]["flush_rings"][1]["running"], json!(1));
        assert_eq!(json[0]["rings_whole"], json!(false));
        let summary = summary_json(std::slice::from_ref(&running));
        assert_eq!(
            (
                &summary["with_flushes_running"],
                &summary["rings_whole"],
                &summary["chance_complete"]
            ),
            (&json!(1), &json!(false), &json!(false))
        );
        let quiet = round(0);
        assert!(
            quiet.whole && !quiet.stalls[0].unsure(),
            "CONTROL: nothing running, nothing lost"
        );
        let md = slow_calls_md("g", &[running, quiet]);
        for line in [
            "| 1 | 2.000 | ack 2.000 | none kept, 1 running | ≥ 0% | ≥ 0% |",
            "| 1 | 2.000 | ack 2.000 | none | 0% | 0% |",
            "- 1: the bench process was still running 1 flush call(s) at the round's reading, \
             which no ring holds yet; each may have begun under any of the round's stalls.",
            "2 slow call(s) counted, 2 kept, in 2 stall(s), 0 with a flush under it, 0 with one \
             lost, 1 with a flush call still running; at least 0% of their 4.000 ms ran under a \
             flush, where the rest of their rounds gives at least 0%.",
        ] {
            assert!(md.iter().any(|l| l.contains(line)), "{line}: {md:?}");
        }
    }

    /// A RING THAT LOST FLUSHES NAMES WHICH (E09c review): each loss line counts the round's
    /// window, names the indices missing from it, each run as its first and last, and how many
    /// of the window the ring holds; the JSON's `missing` gives the same runs, which add up to
    /// `missed`. A bench ring of 64 that raced index 128's write in the window 116 to 128 lost
    /// that one and holds the other 12; a bridge ring holding 2, 3 and 7 of its first 10 lost
    /// three runs.
    #[test]
    fn a_ring_that_lost_flushes_names_which() {
        use super::*;
        /// A ring of `count` recorded slow flushes holding the indices `held`, oldest first.
        fn ring(count: u64, held: impl IntoIterator<Item = u64>) -> Flushed {
            let mut r = Flushed {
                slow_count: count,
                ..Flushed::default()
            };
            for (slot, index) in r.slow.iter_mut().zip(held) {
                *slot = Some(wait_tally::SlowFlush {
                    index,
                    kind: wait_tally::FlushKind::Full,
                    start_ns: 0,
                    end_ns: 1_000,
                });
            }
            r
        }
        let ms = 1_000_000;
        let mut calls = wait_tally::SlowCalls::default();
        calls.kept[0] = Some(wait_tally::SlowCall {
            row: TallyRow::Ack,
            start_ns: 20 * ms,
            ns: 2 * ms,
        });
        calls.count = 1;
        let round = RoundCalls::on(
            &SlowRound {
                group: "g".to_string(),
                round: "paced".to_string(),
                calls,
                bridge_pid: 0,
                bridge: ring(10, [2, 3, 7]),
                bench: ring(129, 64..128),
                window: (10 * ms, 30 * ms),
                marks: vec![Mark {
                    at: 10 * ms,
                    bench: 116,
                    bridge_pid: 0,
                    bridge: 0,
                }],
            },
            true,
        );
        assert_eq!(
            round.rings,
            [
                RingLine {
                    process: "bridge",
                    kept: 3,
                    recorded: 10,
                    from: 0,
                    missed: 7,
                    missing: vec![(0, 1), (4, 6), (8, 9)],
                    running: 0,
                },
                RingLine {
                    process: "bench",
                    kept: 64,
                    recorded: 129,
                    from: 116,
                    missed: 1,
                    missing: vec![(128, 128)],
                    running: 0,
                },
            ]
        );
        for l in &round.rings {
            assert_eq!(
                l.missing.iter().map(|&(a, b)| b - a + 1).sum::<u64>(),
                l.missed,
                "{l:?}: the runs are the ones missed counts"
            );
        }
        let json = rounds_json(std::slice::from_ref(&round));
        assert_eq!(
            json[0]["flush_rings"][1]["missing"],
            json!([[128, 128]]),
            "{json}"
        );
        let md = slow_calls_md("g", std::slice::from_ref(&round));
        for line in [
            "- paced: the bridge ring lost 7 of the round's 10 slow flushes, indices 0 to 1, 4 to \
             6 and 8 to 9 (the window 0 to 9); it holds the other 3.",
            "- paced: the bench ring lost 1 of the round's 13 slow flushes, index 128 (the window \
             116 to 128); it holds the other 12.",
        ] {
            assert!(md.iter().any(|l| l == line), "{line}: {md:?}");
        }
        assert_eq!(
            [&[(5, 5)][..], &[(5, 6)], &[(0, 35), (40, 40)]].map(indices_text),
            ["index 5", "indices 5 to 6", "indices 0 to 35 and 40"]
        );
    }

    /// OFF THE MACHINE'S CLOCK A ROUND HAS NO BASELINE (E09): the round's span is on the bench's
    /// clock and the bridge's times on its own, so the rest of the round and the summary's
    /// chance are null and the table says `unavailable`; the bench's ring is left out, and the
    /// bridge's own ring still lines up with its stalls, judged from its process's start (no
    /// mark's clock compares with its times). NEGATIVE CONTROL: on the machine's clock the same
    /// round has its baseline and both rings.
    #[test]
    fn off_the_machines_clock_a_round_has_no_baseline() {
        use super::*;
        /// A round of `calls` with `bridge` (the bridge's ring at the round's reading, pid 0's) and
        /// `bench`, over `window`, after the readings `marks`.
        fn slow_round(
            round: &str,
            calls: wait_tally::SlowCalls,
            bridge: Flushed,
            bench: Flushed,
            window: (u64, u64),
            marks: Vec<Mark>,
        ) -> SlowRound {
            SlowRound {
                group: "g".to_string(),
                round: round.to_string(),
                calls,
                bridge_pid: 0,
                bridge,
                bench,
                window,
                marks,
            }
        }

        /// A slow flush of `kind` with its index.
        const fn flush(
            index: u64,
            kind: wait_tally::FlushKind,
            start_ns: u64,
            end_ns: u64,
        ) -> wait_tally::SlowFlush {
            wait_tally::SlowFlush {
                index,
                kind,
                start_ns,
                end_ns,
            }
        }

        /// A ring of `count` recorded slow flushes holding `held`, oldest first.
        fn ring(count: u64, held: &[wait_tally::SlowFlush]) -> Flushed {
            let mut r = Flushed {
                slow_count: count,
                ..Flushed::default()
            };
            for (slot, f) in r.slow.iter_mut().zip(held) {
                *slot = Some(*f);
            }
            r
        }

        /// One slow call of `row`, counted and kept.
        fn one_call(row: TallyRow, start_ns: u64, ns: u64) -> wait_tally::SlowCalls {
            let mut calls = wait_tally::SlowCalls::default();
            calls.kept[0] = Some(wait_tally::SlowCall { row, start_ns, ns });
            calls.count = 1;
            calls
        }
        use wait_tally::FlushKind::Full;
        let ms = 1_000_000;
        let bridge = ring(1, &[flush(0, Full, 20 * ms, 21 * ms)]);
        let bench = ring(1, &[flush(0, Full, 21 * ms, 22 * ms)]);
        let marks = vec![Mark {
            at: 10 * ms,
            bench: 0,
            bridge_pid: 0,
            bridge: 1,
        }];
        let r = slow_round(
            "1",
            one_call(TallyRow::Ack, 20 * ms, 2 * ms),
            bridge,
            bench,
            (10 * ms, 30 * ms),
            marks,
        );
        let off = RoundCalls::on(&r, false);
        assert!(off.rest.is_none());
        let named: Vec<&str> = off.stalls[0].under.iter().map(|u| u.process).collect();
        assert_eq!(
            named,
            ["bridge"],
            "the bridge's own flush, on its own clock"
        );
        assert_eq!(
            off.rings[0].from, 0,
            "off the machine's clock, judged from its start"
        );
        let summary = summary_json(std::slice::from_ref(&off));
        assert_eq!(
            (&summary["chance_ns"], &summary["chance_complete"]),
            (&Value::Null, &Value::Null)
        );
        assert_eq!(
            rounds_json(std::slice::from_ref(&off))[0]["rest_of_round"],
            Value::Null
        );
        let md = slow_calls_md("g", &[off]);
        assert!(
            md.iter().any(|l| l.contains(
                "| 1 | 2.000 | ack 2.000 | bridge full 1.00: 1.00 | 50% | unavailable |"
            )),
            "{md:?}"
        );
        assert!(
            md.iter()
                .any(|l| l.contains("their rounds have no baseline")),
            "{md:?}"
        );
        let on = RoundCalls::on(&r, true);
        assert!(
            on.rest.is_some() && on.stalls[0].under.len() == 2,
            "CONTROL: the machine's clock"
        );
        assert_eq!(
            on.rings[0].from, 1,
            "the mark's count, on the machine's clock"
        );
        assert!(summary_json(&[on])["chance_ns"].as_u64().is_some());
    }
}
