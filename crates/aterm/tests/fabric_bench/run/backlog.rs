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
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use aterm_link::ctl::Ctl;

use super::{await_note, list_since, newest_id, post, request};
use crate::clock::{Clock, ms};
use crate::harness::{FLEET, World, until};

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
    let bodies: Vec<&[u8]> = vec![&FILL_BODY[..]; n];
    let mut god = w.god();
    let t0 = Instant::now();
    let acks = god
        .publish_pipelined(producer, &subject, &bodies, FILL_WINDOW)
        .unwrap_or_else(|e| panic!("fill {n} records on {subject}: {e}"));
    assert_eq!(acks.len(), n, "every filler record acked");
    assert!(
        acks.iter().all(|(_, deduped)| !deduped),
        "a filler record was deduped: producer {producer:#x} was used before"
    );
    t0.elapsed().as_secs_f64() * 1000.0
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
}
