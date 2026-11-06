// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE ATTACH BACKLOG (B09): what a bridge's refill costs when every session's
//! refill watermark (`seen/<sid>`) trails the head.
//!
//! A new bridge refills each session it serves from that session's watermark
//! (`aterm-link` `Bridge::refill`): pages of `Fetch` over the session's lane,
//! each scanning up to astream's `FETCH_SCAN_MAX` records under the broker's
//! log lock. A session whose mail was all handled long ago trails the head by
//! everything the node wrote since, so every attach (a crash's relaunch, a
//! seamless update's successor) scans that much once per session. The attach
//! world's `backlog=` knob builds that state: each population's sessions are
//! each sent a note they mark seen, the bridge persists each watermark there,
//! and the bench then fills `backlog` records on an `ev` face no session owns.
//! A paced publisher on the same broker times its acks across the attaches,
//! so a refill's hold of the log lock shows as the writer's stall.

use std::collections::HashMap;
use std::io::{self, Read, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use aterm_link::ctl::Ctl;

use super::{await_note, list_since, newest_id, post, request};
use crate::clock::{Clock, ms};
use crate::harness::{DEADLINE, FLEET, World, until};

/// The filler's producer id: one per population, `+ size index`.
pub const FILL_PRODUCER: u64 = 0x0fab_0b09_0000_0000;

/// The paced publisher's producer id.
const STEADY_PRODUCER: u64 = 0x0fab_0b09_5eed_0000;

/// How often the paced publisher publishes.
pub const STEADY_EVERY: Duration = Duration::from_millis(5);

/// One filler record's length in bytes: about a mirrored turn's size (the bus
/// mirror's 64 MiB node window holds some 500k of them).
pub const FILL_BODY_LEN: usize = 128;

/// One filler record's body.
const FILL_BODY: [u8; FILL_BODY_LEN] = [b'b'; FILL_BODY_LEN];

/// Filler publishes in flight on one connection.
const FILL_WINDOW: usize = 512;

/// The filler's subject: an `ev` face, under aterm's bulk rule, that no
/// session owns and no bridge face reads.
#[must_use]
pub fn fill_subject(node: &str) -> String {
    format!("/f/{FLEET}/pub/{node}/backlog/ev")
}

/// The paced publisher's subject: no aterm face reads it.
#[must_use]
pub fn steady_subject() -> String {
    format!("/f/{FLEET}/bench/steady")
}

/// Every one of `sids` is sent a note from `mailer`, lists it and marks it
/// seen, and the bridge persists each one's refill watermark at that note's
/// offset (its state dir's `seen/<sid>`). Returns each session's newest row
/// id afterwards, where its next wait starts.
pub fn seed_seen(
    w: &World,
    ctl: &mut Ctl,
    clock: Clock,
    mailer: &str,
    sids: &[String],
    tag: &str,
) -> HashMap<String, u64> {
    let mut newest = HashMap::new();
    for (i, sid) in sids.iter().enumerate() {
        let since = newest_id(ctl, sid);
        let text = format!("seed-{tag}-{i}");
        post(ctl, clock, mailer, sid, &text);
        let (id, _) = await_note(ctl, clock, sid, since)
            .unwrap_or_else(|e| panic!("the seed note `{text}` to {sid}: {e}"));
        let row = list_since(ctl, sid, since)
            .into_iter()
            .find(|r| r.id == id)
            .unwrap_or_else(|| panic!("{sid} woke at row {id} and does not list it"));
        assert_eq!(row.text, text, "{sid}'s row {id}");
        let line = format!("@{sid} inbox seen {id}");
        let reply = request(ctl, &line);
        assert!(reply.ok(), "{line}: {}", reply.header());
        let path = w.state.join("seen").join(sid);
        until(
            &format!(
                "the bridge to persist {sid}'s refill watermark at @{}",
                row.off
            ),
            || {
                std::fs::read_to_string(&path)
                    .ok()
                    .and_then(|s| s.trim().parse::<u64>().ok())
                    .filter(|off| *off >= row.off)
                    .map(|_| ())
            },
        );
        newest.insert(sid.clone(), id);
    }
    newest
}

/// `n` filler records on [`fill_subject`] under producer `producer`,
/// pipelined: how long the fill took, in ms.
pub fn fill(w: &World, producer: u64, n: usize) -> f64 {
    let subject = fill_subject(&w.node);
    let mut god = w.god();
    let t0 = Instant::now();
    fill_records(&mut god, producer, &subject, n)
        .unwrap_or_else(|e| panic!("fill {n} records on {subject}: {e}"));
    t0.elapsed().as_secs_f64() * 1000.0
}

/// Fill in bounded coalesced windows. Drain every response before backoff and
/// retry only refused sequences: later successes in the same window stay paid.
/// A minute without a new acknowledgement is the hang detector, not a total
/// fill budget; a large backlog may legitimately take longer under a quota.
pub fn fill_records<S: Read + Write>(
    client: &mut astream_broker::Client<S>,
    producer: u64,
    subject: &str,
    n: usize,
) -> io::Result<()> {
    use astream_broker::{Pipelined, broker_refusal};

    if client.owed_acks() != 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "fill owes earlier acknowledgements",
        ));
    }
    let window = FILL_WINDOW.min(astream_broker::Client::<S>::MAX_PIPELINE_WINDOW);
    let mut progress = Instant::now();
    for start in (0..n).step_by(window) {
        let mut pending: Vec<u64> = (start..n.min(start.saturating_add(window)))
            .map(|index| index as u64 + 1)
            .collect();
        while !pending.is_empty() {
            if progress.elapsed() >= DEADLINE {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "backlog fill made no progress",
                ));
            }
            let burst: Vec<_> = pending
                .iter()
                .map(|&producer_seq| Pipelined::Publish {
                    producer_id: producer,
                    producer_seq,
                    subject,
                    body: &FILL_BODY,
                })
                .collect();
            client.send_pipelined(&burst)?;
            let mut retry = Vec::new();
            let mut wait = Duration::ZERO;
            let mut failure = None;
            for seq in pending {
                match client.recv_publish_ack() {
                    Ok((_, false)) => progress = Instant::now(),
                    Ok((_, true)) => {
                        failure.get_or_insert_with(|| {
                            io::Error::new(
                                io::ErrorKind::InvalidData,
                                "a filler record was deduped: its producer was used before",
                            )
                        });
                    }
                    Err(error) => {
                        let Some(refusal) = broker_refusal(&error) else {
                            return Err(error);
                        };
                        if let Some(ms) = refusal.retry_after_ms().filter(|&ms| ms > 0) {
                            wait = wait.max(Duration::from_millis(ms));
                            retry.push(seq);
                        } else {
                            failure.get_or_insert(error);
                        }
                    }
                }
            }
            if let Some(error) = failure {
                return Err(error);
            }
            if !retry.is_empty() {
                std::thread::sleep(wait.min(DEADLINE.saturating_sub(progress.elapsed())));
            }
            pending = retry;
        }
    }
    Ok(())
}

/// One paced publish: when it was sent, and when its ack came, on the run's clock.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Beat {
    pub sent: u64,
    pub acked: u64,
}

impl Beat {
    /// Its ack latency in ms.
    #[must_use]
    pub fn ack_ms(self) -> f64 {
        ms(self.sent, self.acked)
    }

    /// Whether any of it lies inside `[lo, hi]`.
    #[must_use]
    pub fn meets(self, lo: u64, hi: u64) -> bool {
        self.sent <= hi && self.acked >= lo
    }
}

/// Publish on [`steady_subject`] every [`STEADY_EVERY`] until `stop`, timing
/// each ack; `first_seq` and up are its producer sequence numbers.
pub fn steady(w: &World, clock: Clock, first_seq: u64, stop: &AtomicBool) -> Vec<Beat> {
    let subject = steady_subject();
    let mut god = w.god();
    let mut beats = Vec::new();
    let mut seq = first_seq;
    let mut due = Instant::now();
    while !stop.load(Ordering::Relaxed) {
        let sent = clock.us();
        god.publish(STEADY_PRODUCER, seq, &subject, b"steady")
            .unwrap_or_else(|e| panic!("steady publish {seq}: {e}"));
        beats.push(Beat {
            sent,
            acked: clock.us(),
        });
        seq += 1;
        due += STEADY_EVERY;
        let now = Instant::now();
        if due > now {
            std::thread::sleep(due - now);
        } else {
            due = now;
        }
    }
    beats
}

/// `beats`' ack latencies split by whether each meets one of `windows`: `(inside, outside)`.
#[must_use]
pub fn split(beats: &[Beat], windows: &[(u64, u64)]) -> (Vec<f64>, Vec<f64>) {
    let (inside, outside): (Vec<&Beat>, Vec<&Beat>) = beats
        .iter()
        .partition(|b| windows.iter().any(|&(lo, hi)| b.meets(lo, hi)));
    (
        inside.into_iter().map(|b| b.ack_ms()).collect(),
        outside.into_iter().map(|b| b.ack_ms()).collect(),
    )
}

/// What the world's broker holds in memory at one moment (D10): the retained records it keeps
/// decoded (`Broker::residency`), and the physical footprint of this process, which serves it
/// in-process beside the bench's own clients.
#[derive(Debug, Clone, Copy)]
pub struct Held {
    /// Retained records the broker keeps decoded.
    pub resident_records: usize,
    /// The bytes its residency budget charges them.
    pub resident_bytes: usize,
    /// Every record it retains.
    pub retained: usize,
    /// The records it has read back from its log file since the open because the budget left
    /// them cold (`Broker::cold_reads`, D10d): 0 with no budget.
    pub cold_reads: u64,
    /// This process's physical footprint now, bytes (macOS `ri_phys_footprint`); `None` where
    /// the process table does not say.
    pub footprint: Option<u64>,
    /// The largest footprint this process has had since it started (macOS
    /// `ri_lifetime_max_phys_footprint`); `None` where unread.
    pub peak_footprint: Option<u64>,
}

/// [`Held`] of `broker` now.
#[must_use]
pub fn held(broker: &astream_broker::Broker) -> Held {
    let (resident_records, resident_bytes, retained) = broker.residency();
    let cold_reads = broker.cold_reads();
    let me = std::process::id();
    let mine = aterm_sysprobe::procs::processes()
        .and_then(|sweep| sweep.procs.into_iter().find(|p| p.pid == me));
    Held {
        resident_records,
        resident_bytes,
        retained,
        cold_reads,
        footprint: mine.as_ref().and_then(|p| p.footprint),
        peak_footprint: mine.as_ref().and_then(|p| p.peak_footprint),
    }
}

/// The broker's residency tier's tally since its open (astream's `TierTally`, D10f): its
/// pushes, its one-record reads by index (a cold read two), its walks over a range of summaries
/// and the rows they read (a filtered read's pass, `Tier::passing`, is a walk and the records it
/// reads are rows), the pages a filtered read passed whole and the times it asked its filter of a
/// subject (F11c), and the values it read from its wide maps. It counts no subject walk since
/// F19: the pending wills come from astream's latest-will map, and `Tier::naming` is astream's
/// own test read-back. Read around a restart as its cold reads are, so an attach's cost is put down to the
/// pages' code, or not, by a count. A build with debug assertions counts it (the `opt`
/// profile's); a build without them (`--profile shipped`) counts nothing and reads `None`.
#[cfg(debug_assertions)]
#[must_use]
pub fn tier_tally(broker: &astream_broker::Broker) -> Option<astream_broker::TierTally> {
    Some(broker.tier_tally())
}

/// The broker's residency tier's tally: `None`, since a build without debug assertions counts
/// nothing.
#[cfg(not(debug_assertions))]
#[must_use]
pub fn tier_tally(_broker: &astream_broker::Broker) -> Option<astream_broker::TierTally> {
    None
}

/// One count of a tally as the report carries it: its key in the JSON, its column in the
/// markdown table, and the count.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TierCount {
    pub key: &'static str,
    pub column: &'static str,
    pub n: u64,
}

/// Every count of `t`, in the order the report's table prints them. The pattern names each of
/// astream's fields, so a count astream adds fails to compile here until the report carries
/// it; the table's heading and the JSON's keys are read from the same list.
#[must_use]
pub fn tier_counts(t: astream_broker::TierTally) -> [TierCount; 7] {
    let astream_broker::TierTally {
        pushes,
        lookups,
        walks,
        rows,
        passed_pages,
        verdicts,
        wide,
    } = t;
    let c = |key, column, n| TierCount { key, column, n };
    [
        c("pushes", "pushes", pushes),
        c("lookups", "lookups", lookups),
        c("walks", "walks", walks),
        c("rows", "rows walked", rows),
        c("passed_pages", "pages passed", passed_pages),
        c("verdicts", "verdicts", verdicts),
        c("wide", "wide", wide),
    ]
}

/// `t` as the report's JSON carries it: an object of [`tier_counts`] by their keys.
#[must_use]
pub fn tier_json(t: astream_broker::TierTally) -> aterm_json::Value {
    aterm_json::Value::Object(
        tier_counts(t)
            .into_iter()
            .map(|c| (c.key.to_string(), aterm_json::Value::from(c.n)))
            .collect(),
    )
}

#[cfg(test)]
#[path = "backlog_fill_tests.rs"]
mod fill_tests;

#[cfg(test)]
mod tests {
    /// The spec's bound on the backlog is astream's and aterm's own arithmetic: a filler
    /// record's bytes as retention counts them, at the bench's node names' length
    /// (`n-<16 hex>`), and the bytes aterm's `ev` rule keeps.
    #[test]
    fn the_spec_bounds_the_fill_by_the_record_and_the_rule_it_is_cut_by() {
        use super::*;

        let node = format!("n-{:016x}", u64::MAX);
        let rec = astream_broker::BrokerRecord {
            seq: 0u64.into(),
            producer_id: FILL_PRODUCER,
            producer_seq: 1,
            subject: fill_subject(&node),
            body: FILL_BODY.to_vec(),
            commit: None,
        };
        assert_eq!(
            astream_broker::record_bytes(&rec),
            crate::spec::FILL_RECORD_BYTES as u64
        );
        assert_eq!(
            aterm_link::broker_rules::EV_BYTES,
            crate::spec::EV_RULE_BYTES as u64
        );
    }

    #[test]
    fn a_beat_is_inside_a_window_it_overlaps_at_all() {
        use super::*;

        let beat = Beat {
            sent: 100,
            acked: 300,
        };
        assert!((beat.ack_ms() - 0.2).abs() < 1e-9);
        for (lo, hi, inside) in [
            (0, 99, false),
            (0, 100, true),
            (150, 200, true),
            (300, 400, true),
            (301, 400, false),
            (0, 1_000, true),
        ] {
            assert_eq!(beat.meets(lo, hi), inside, "[{lo}, {hi}]");
        }
        let beats = [
            beat,
            Beat {
                sent: 500,
                acked: 1_500,
            },
        ];
        let (inside, outside) = split(&beats, &[(1_000, 2_000)]);
        assert_eq!((inside, outside), (vec![1.0], vec![0.2]));
        let (inside, outside) = split(&beats, &[]);
        assert_eq!((inside.len(), outside.len()), (0, 2));
    }

    /// Each of a tally's counts reaches the report once, under a JSON key and a table column of
    /// its own: a tally whose counts all differ comes back as each of them once, and the JSON
    /// holds each under its key.
    #[test]
    fn every_count_of_a_tally_reaches_the_report_once() {
        use super::*;

        let t = astream_broker::TierTally {
            pushes: 1,
            lookups: 2,
            wide: 3,
            walks: 4,
            rows: 5,
            passed_pages: 6,
            verdicts: 7,
        };
        let counts = tier_counts(t);
        let mut ns: Vec<u64> = counts.iter().map(|c| c.n).collect();
        ns.sort_unstable();
        assert_eq!(ns, (1..=7).collect::<Vec<u64>>());
        let keys: std::collections::BTreeSet<_> = counts.iter().map(|c| c.key).collect();
        let columns: std::collections::BTreeSet<_> = counts.iter().map(|c| c.column).collect();
        assert_eq!((keys.len(), columns.len()), (counts.len(), counts.len()));
        let json = tier_json(t);
        for c in counts {
            assert_eq!(
                json.get(c.key).and_then(aterm_json::Value::as_u64),
                Some(c.n),
                "{c:?}"
            );
        }
    }
}
