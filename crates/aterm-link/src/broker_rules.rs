// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Default broker retention rules ([`retain`](crate::broker_rules::retain), [`crate::mirror::NODE_WINDOW_BYTES`],
//! [`crate::subject::inbox_group`]).
//!
//! The mail rule ([`MAIL_FILTER`]) carries unread mail below the cut until [`MAIL_READER`] reads
//! it, for [`MAX_AGE`] within [`MAIL_BYTES`]; read mail goes with the cut. Mail it gives up unread
//! is logged by astream (subject and count), answers a `Fetch` below it `ERR retained-from=`, and
//! is TOLD to the node's inbox group subscription (astream's gap notice, R4's addendum A9): the
//! bridge says `link gap from=<a> to=<b> [records=<n>]`, and every session the node serves names
//! it in `inbox` (`given_up=`) and marks it in its log (`mail-gap`). Read mail the cut took before
//! its session handled it is told to that session when the bridge's own read of its lane meets the
//! floor (its refill from `seen`, its deadline sweep): `link gap … sid=<sid>`. `aterm fabric`
//! lists both under WARNINGS (the node's once per instance, from the count its `fabric status`
//! keeps, `mail_gaps=`), and its `doctor` fix names these rules' bounds.

use std::io;
use std::time::Duration;

use astream_broker::{Retention, Rule};

/// Subject pattern for the bulk `ev` stream.
pub const EV_FILTER: &str = "/f/*/pub/*/*/ev";
/// Every presence row, node and session.
pub const PRESENCE_FILTER: &str = "/f/*/pub/*/*/presence";
/// Every mail lane.
pub const MAIL_FILTER: &str = "/f/*/in/>";
/// The group whose read releases a mail.
pub const MAIL_READER: &str = "/f/{2}/cur/{4}/node/inbox";
/// The mesh's mail lanes (§11 C3), the fleet `alab`'s ([`crate::federate::FLEET`]).
pub const MESH_MAIL_FILTER: &str = "/f/alab/in/>";
/// The group whose read also releases the mesh's mail on a Mac's `bus.log` ([`mesh_rules`]): the
/// person carry's ([`crate::federate::carry_group`]), one name on every Mac, since nothing in a
/// person's post names the Mac that carries it.
pub const CARRY_READER: &str = "/f/alab/cur/n-carry/node/carry";

/// How long any record is kept.
pub const MAX_AGE: Duration = Duration::from_secs(7 * 24 * 60 * 60);
/// The bulk stream's bytes.
pub const EV_BYTES: u64 = 256 * 1024 * 1024;
/// The mail lanes' bytes.
pub const MAIL_BYTES: u64 = 64 * 1024 * 1024;
/// Every other subject's bytes.
pub const DEFAULT_BYTES: u64 = 32 * 1024 * 1024;

/// The rules every broker aterm opens retains its log by.
///
/// # Panics
/// Never: every filter and template here is a constant astream's grammar takes.
#[must_use]
pub fn default_rules() -> Retention {
    rules().unwrap_or_else(|e| unreachable!("the default retention rules are refused: {e}"))
}

/// [`default_rules`] with the bulk stream's, the mail lanes' and every other subject's bytes
/// scaled to `ev`, `mail` and `default`: the same filters, classes, reader and ages, so a test
/// reaches the bounds the tick cuts at with a few records.
///
/// # Panics
/// Never: astream checks the rules' filters, classes and readers, the same constants as
/// [`default_rules`]', and takes any byte bound.
#[cfg(any(test, debug_assertions))]
#[must_use]
pub fn rules_scaled(ev: u64, mail: u64, default: u64) -> Retention {
    rules_with(ev, mail, default)
        .unwrap_or_else(|e| unreachable!("the scaled retention rules are refused: {e}"))
}

/// The rules a Mac's mesh `bus.log` (§11 C3, which the node service hosts) retains its log by:
/// [`default_rules`], with the mesh's mail ([`MESH_MAIL_FILTER`]) under a rule of its own ahead of
/// the mail rule, held until the person carry read it too ([`CARRY_READER`]), so a person's post
/// outlives a carry outage across a cut as unread mail outlives a push's (§11 C3d). The rule
/// forgets ([`Rule::forgetting`]): a lane whose every record retention gave up leaves the
/// last-value index, and the subject it was charged to its producer goes back to that producer's
/// budget, so the carry, which opens a lane per session and kind it carries to and gives none
/// back itself, never fills its budget with lanes nothing of which is left. Any other fleet's
/// mail, which no carry reads, keeps the mail rule.
///
/// # Panics
/// Never: every filter and template here is a constant astream's grammar takes.
#[must_use]
pub fn mesh_rules() -> Retention {
    mesh_rules_with(EV_BYTES, MAIL_BYTES, DEFAULT_BYTES, true)
        .unwrap_or_else(|e| unreachable!("the mesh retention rules are refused: {e}"))
}

/// [`mesh_rules`] with its bytes scaled, as [`rules_scaled`] scales [`default_rules`].
///
/// # Panics
/// Never, as [`rules_scaled`].
#[cfg(any(test, debug_assertions))]
#[must_use]
pub fn mesh_rules_scaled(ev: u64, mail: u64, default: u64) -> Retention {
    mesh_rules_with(ev, mail, default, true)
        .unwrap_or_else(|e| unreachable!("the scaled mesh retention rules are refused: {e}"))
}

/// [`mesh_rules_scaled`] whose mesh rule does not forget: a lane retention gave up whole keeps
/// its last-value row and its charge, the rules before §11 C3d's release of them (a negative
/// control).
///
/// # Panics
/// Never, as [`rules_scaled`].
#[cfg(any(test, debug_assertions))]
#[must_use]
pub fn mesh_rules_scaled_unforgetting(ev: u64, mail: u64, default: u64) -> Retention {
    mesh_rules_with(ev, mail, default, false)
        .unwrap_or_else(|e| unreachable!("the scaled mesh retention rules are refused: {e}"))
}

fn rules() -> io::Result<Retention> {
    rules_with(EV_BYTES, MAIL_BYTES, DEFAULT_BYTES)
}

fn rules_with(ev: u64, mail: u64, default: u64) -> io::Result<Retention> {
    rules_holding(ev, None, mail, default)
}

/// The mesh rules, whose mesh rule forgets where `forget` says so.
fn mesh_rules_with(ev: u64, mail: u64, default: u64, forget: bool) -> io::Result<Retention> {
    let mesh = Rule::hold(MESH_MAIL_FILTER)?
        .until_read(MAIL_READER)?
        .until_read(CARRY_READER)?
        .max_age(MAX_AGE)
        .max_bytes(mail);
    let mesh = if forget { mesh.forgetting() } else { mesh };
    rules_holding(ev, Some(mesh), mail, default)
}

/// The rules, with `mesh` ahead of the mail rule where given.
fn rules_holding(ev: u64, mesh: Option<Rule>, mail: u64, default: u64) -> io::Result<Retention> {
    let mut rules = vec![
        Rule::prefix(EV_FILTER)?.max_age(MAX_AGE).max_bytes(ev),
        Rule::hold(PRESENCE_FILTER)?.last_only(),
    ];
    rules.extend(mesh);
    rules.push(
        Rule::hold(MAIL_FILTER)?
            .until_read(MAIL_READER)?
            .max_age(MAX_AGE)
            .max_bytes(mail),
    );
    Retention::new(
        rules,
        Rule::default_keeping_all()
            .max_age(MAX_AGE)
            .max_bytes(default),
    )
}

/// Whether `rules` (a broker's, as its `Hello` advertised them) retain this node's `ev` faces.
#[must_use]
pub fn retains_ev(rules: &Retention, fleet: &str, node: &str) -> bool {
    let Ok(faces) = astream_wire::Filter::new(format!("/f/{fleet}/pub/{node}/*/ev")) else {
        return false;
    };
    rules.rules().iter().any(|rule| {
        rule.class() == astream_broker::Class::Prefix
            && rule
                .filter()
                .and_then(|f| astream_wire::Filter::new(f).ok())
                .is_some_and(|f| f.contains(&faces))
    })
}

/// The rules in a few words, for `aterm link broker`'s start line.
#[must_use]
pub fn summary() -> String {
    const MIB: u64 = 1024 * 1024;
    const DAY: u64 = 24 * 60 * 60;
    format!(
        "{} days, ev {} MiB, mail {} MiB, other {} MiB",
        MAX_AGE.as_secs() / DAY,
        EV_BYTES / MIB,
        MAIL_BYTES / MIB,
        DEFAULT_BYTES / MIB
    )
}

/// Retain `broker`'s log by [`default_rules`], from this open: called at once after the open,
/// with no flush first. astream sets the rules between the writer's batches, so they wait out
/// the batch the open's fired wills are in rather than meeting it in its write window.
///
/// # Errors
/// Astream's refusal: a poisoned log, or a held store that cannot be written.
pub fn retain(broker: &astream_broker::Broker) -> io::Result<()> {
    broker.set_retention(default_rules())
}

/// Retain a Mac's mesh `bus.log` (`broker`) by [`mesh_rules`], from this open, as [`retain`].
///
/// # Errors
/// As [`retain`].
pub fn retain_mesh(broker: &astream_broker::Broker) -> io::Result<()> {
    broker.set_retention(mesh_rules())
}

/// THE BROKER'S RATE LIMIT (critic item E06): astream's per-producer token bucket
/// (`DESIGN-aterm-fabric` T9), keyed by the producer id, which on a guarded broker is the
/// attached capability's principal. A node's bridge publishes every session of its node, its bus
/// mirror's slots included, under one producer; each person's and agent's client publishes under
/// its own. So it bounds a runaway bridge loop, the mirror, or a direct client, and not one
/// session inside a bridge.
///
/// From the shipped traffic as E06 measured it (`docs/measured/fabric/README.md`, "A default
/// broker rate limit": one node, aterm's ordered broker, `--tally`, four runs at a load of 4.5 to
/// 15.6 on 18 cores): the 5-sender burst of 500 posts is acked in 43.1 to 50.7 ms, its 510
/// publishes from the one bridge producer (the 500 notes and 10 `ev` rows; an inbox commit is no
/// publish) at up to 11,830 a second; the paced notes are about three publishes each, 1.1 s
/// apart; and the mirror paces itself at [`crate::mirror::RATE_PER_SEC`] with a burst of
/// [`crate::mirror::RATE_BURST`] slots. The bucket is at least four times each:
/// [`RATE_RECORDS_PER_SEC`] over the burst's fastest rate (4.2 times), [`RATE_BURST_RECORDS`]
/// over the mirror's whole burst and the post burst together, which so take at most a quarter of
/// it however fast a machine sends them. [`RATE_BURST_BYTES`] is four frames at
/// astream's frame bound, so no record the wire carries is ever past the byte burst (astream
/// refuses such a record at every try, `record bytes exceed burst_bytes`); the largest the
/// measured traffic wrote is 198 bytes. [`RATE_BYTES_PER_SEC`] is a whole frame a second: the
/// burst's notes are 136 bytes each (`2026-11-05-fabric-6065`), 1.61 MB/s at its fastest record
/// rate, a tenth of it.
pub const RATE_RECORDS_PER_SEC: u64 = 50_000;
/// The record bucket's burst ([`RATE_RECORDS_PER_SEC`]).
pub const RATE_BURST_RECORDS: u64 = 4 * (crate::mirror::RATE_BURST + 512);
/// The byte bucket's sustained rate, of `subject + body` ([`RATE_RECORDS_PER_SEC`]).
pub const RATE_BYTES_PER_SEC: u64 = FRAME_BYTES;
/// The byte bucket's burst ([`RATE_RECORDS_PER_SEC`]).
pub const RATE_BURST_BYTES: u64 = 4 * FRAME_BYTES;

/// astream's frame bound, which no record's `subject + body` reaches.
const FRAME_BYTES: u64 = astream_wire::MAX_PAYLOAD_LEN as u64;

/// The rate limit every broker aterm opens publishes under ([`RATE_RECORDS_PER_SEC`]).
///
/// # Panics
/// Never: each bound is a constant within astream's `MAX_RATE_BUDGET`.
#[must_use]
pub fn rate_limit() -> astream_broker::RateLimit {
    astream_broker::RateLimit::records(RATE_RECORDS_PER_SEC, RATE_BURST_RECORDS)
        .and_then(|limit| limit.with_bytes(RATE_BYTES_PER_SEC, RATE_BURST_BYTES))
        .unwrap_or_else(|| unreachable!("the default rate limit is refused"))
}

/// Limit `broker`'s publishes by [`rate_limit`], from this open: called at once after the open,
/// beside [`retain`], before the broker serves.
///
/// # Errors
/// Astream's refusal of the limit's bounds.
pub fn limit(broker: &astream_broker::Broker) -> io::Result<()> {
    broker.set_rate_limit(Some(rate_limit()))
}

/// The limit a broker serves under (`Broker::rate_limit`, read back after [`limit`]) in a few
/// words, for `aterm link broker`'s start line: aterm's default ([`rate_summary`]), another, or
/// none.
#[must_use]
pub fn rate_word(served: Option<astream_broker::RateLimit>) -> String {
    match served {
        Some(served) if served == rate_limit() => {
            format!("rate-limited by aterm's default ({})", rate_summary())
        }
        Some(served) if served.bytes_per_sec == 0 => format!(
            "rate-limited at {} records/s (burst {}) per producer",
            served.records_per_sec, served.burst_records
        ),
        Some(served) => format!(
            "rate-limited at {} records/s (burst {}), {} bytes/s (burst {}) per producer",
            served.records_per_sec, served.burst_records, served.bytes_per_sec, served.burst_bytes
        ),
        None => "unlimited".to_string(),
    }
}

/// The limit in a few words, for `aterm link broker`'s start line.
#[must_use]
pub fn rate_summary() -> String {
    const MIB: u64 = 1024 * 1024;
    format!(
        "{RATE_RECORDS_PER_SEC} records/s (burst {RATE_BURST_RECORDS}), {} MiB/s (burst {} MiB) \
         per producer",
        RATE_BYTES_PER_SEC / MIB,
        RATE_BURST_BYTES / MIB
    )
}

#[cfg(test)]
#[path = "broker_rules_tests.rs"]
mod tests;
