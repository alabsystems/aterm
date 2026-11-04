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
use aterm_link::wait_tally::{self, Hist, Row as TallyRow, Tally, Unit};
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

/// Both tallies, read one after the other.
#[derive(Debug, Clone, Copy)]
pub(super) struct Reading {
    waits: Tally,
    handlers: HandlerTally,
}

/// One round's rows.
struct Counted {
    /// The rounds this one is reported with ("attach, 8 sessions").
    group: String,
    /// The round within it.
    round: String,
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
        Reading { waits, handlers }
    }

    /// Count the next round from `now`.
    pub(super) fn rebase(&mut self, now: Reading) {
        self.last = Some(now);
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
            group: group.to_string(),
            round,
            waits,
            new_bridge,
            handlers,
            new_aterm,
        });
    }

    /// Every round's rows, for the report's JSON.
    pub(super) fn json(&self) -> Value {
        let rounds: Vec<Value> = self
            .rounds
            .iter()
            .map(|c| {
                json!({
                    "group": c.group,
                    "round": c.round,
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
        })
    }

    /// Every round's rows, a table for each group of rounds, for the entry.
    pub(super) fn md(&self) -> Vec<String> {
        let mut md = Vec::new();
        let mut groups: Vec<&str> = Vec::new();
        for c in &self.rounds {
            if !groups.contains(&c.group.as_str()) {
                groups.push(c.group.as_str());
            }
        }
        for group in groups {
            let rounds: Vec<&Counted> = self.rounds.iter().filter(|c| c.group == group).collect();
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
                    c.round,
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
                        .map(|c| (c.round.as_str(), *c.waits.row(row)))
                        .collect();
                    md.extend(table_rows(&format!("bridge: {}", describe(row)), &each));
                }
            }
            for row in HandlerRow::ALL {
                let each: Vec<(&str, Hist)> = rounds
                    .iter()
                    .map(|c| (c.round.as_str(), *c.handlers.row(row)))
                    .collect();
                md.extend(table_rows(
                    &format!("aterm: {}", describe_handler(row)),
                    &each,
                ));
            }
            md.push(String::new());
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
        let off = crate::spec::parse("out=/o").expect("the default run");
        for own in [&[][..], &[quit_bye][..], &[(wait_tally::SEAM, "1")][..]] {
            assert_eq!(world_env(&off, own), own.to_vec(), "{own:?}");
        }
        assert!(
            !world_env(&off, &[quit_bye])
                .iter()
                .any(|&(k, _)| k == wait_tally::SEAM || k == HANDLER_SEAM),
            "the default handoff boot counts nothing"
        );
        let on = crate::spec::parse("out=/o tally=on").expect("the knob");
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
            Reading { waits, handlers }
        };
        t.rebase(reading(10, 7, &[5_000], &[2_000]));
        t.round("g", "1".into(), reading(10, 7, &[5_000, 9_000], &[2_000]));
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
}
