// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The broker's rate limit on a live world (critic item E06): it is a bucket per producer, so a
//! producer that floods the bus is throttled and a person's post lands all the same; and the
//! broker `aterm link broker` serves is under it.

use std::path::Path;

use crate::harness::{self, FLEET, World, kv, until};

/// **`aterm link broker` SERVES UNDER ATERM'S DEFAULT LIMIT** (critic item E06's review): the
/// command's start line names the limit its broker serves, read back from that broker
/// (`aterm_link::broker_rules::rate_word`), and it is aterm's default. Every other test's broker
/// is opened by the harness, which sets the limit itself ([`harness::open_broker`]); this one is
/// the command's own.
#[test]
fn the_broker_command_serves_under_the_default_limit() {
    let dir = aterm_tempfile::Builder::new()
        .prefix("atl-")
        .tempdir_in("/tmp")
        .expect("scratch dir");
    let said = |dir: &Path| std::fs::read_to_string(dir.join("broker.err")).unwrap_or_default();
    let err = std::fs::File::create(dir.path().join("broker.err")).expect("broker.err");
    let sock = dir.path().join("b.sock").to_string_lossy().into_owned();
    let log = dir.path().join("b.log").to_string_lossy().into_owned();
    let mut broker = harness::spawn_broker(dir.path(), &[&sock, &log], |cmd| {
        cmd.stdout(aterm_uds::spawnfd::Stdio::piped()).stderr(err);
    });
    let first = harness::first_line_within(&mut broker.child, "its readiness line");
    assert_eq!(
        first.trim_end(),
        format!("listening {sock}"),
        "{}",
        said(dir.path())
    );
    // Whole: the broker writes the line in pieces, so one read can meet it part written.
    let start = until("the broker's whole start line", || {
        said(dir.path())
            .split_inclusive('\n')
            .find(|line| {
                line.starts_with("aterm link broker: unix socket on") && line.ends_with('\n')
            })
            .map(str::to_string)
    });
    harness::kill(i32::try_from(broker.child.id()).expect("pid"), 15);
    until("the broker to stop", || {
        broker.child.try_wait().ok().flatten()
    });
    let default = aterm_link::broker_rules::rate_word(Some(aterm_link::broker_rules::rate_limit()));
    assert!(
        start.contains(&format!("; {default}; ")),
        "the command's broker serves under aterm's default limit ({default}): {start}"
    );
}

/// The flooding producer's id, a program's of its own on the world's broker.
const FLOODER: u64 = 7_400;
/// The size of each flood record's body.
const FLOOD_BODY: usize = 256 * 1024;
/// The flood records the lowered limit's byte burst takes: a few, and room for the bridge's
/// small rows through the whole test.
const IN_BURST: u64 = 8;

/// **A FLOODING PRODUCER IS THROTTLED WHILE A PERSON'S POST LANDS.** The world's broker, opened
/// as `aterm link broker` opens one ([`harness::open_broker`]), is under aterm's default limit
/// ([`aterm_link::broker_rules::limit`]). The test lowers it to a
/// limit whose byte bucket a flood outruns at once: a burst of exactly [`IN_BURST`] flood
/// records (astream counts a record's `subject + body`) refilled at one byte a second, with room
/// for a thousand records. Whether the flood is refused then does not depend on the machine's
/// speed. A producer publishes [`FLOOD_BODY`] records to a lane on another node. The burst's
/// records land; every one after is refused `ERR_RATE_LIMITED`, naming a wait of days. A
/// person's note to the world's session `a` then lands in `a`'s inbox: its own producer's bucket
/// was never touched. The flooder is refused again after that note landed, and the broker
/// counted every refusal (`BrokerStats::rate_limited`).
#[test]
fn a_flooding_producer_is_throttled_while_a_persons_post_lands() {
    let w = World::boot("rate", &[]);
    w.wait_ready();
    let broker = w.broker.as_ref().expect("the world serves its broker");
    assert_eq!(
        broker.rate_limit(),
        Some(aterm_link::broker_rules::rate_limit()),
        "the world's broker is under aterm's default limit, as the command's is"
    );
    let (a, _b) = w.two_sessions();
    let subject = format!("/f/{FLEET}/in/n-0000000000000f10/s-flood/a-flood/note");
    let body = format!("v=1 t=1 text={}", "f".repeat(FLOOD_BODY)).into_bytes();
    let record = u64::try_from(subject.len() + body.len()).expect("a record's size");
    let limit = astream_broker::RateLimit::records(1_000, 1_000)
        .and_then(|limit| limit.with_bytes(1, IN_BURST * record))
        .expect("astream takes a byte bucket of a byte a second");
    broker
        .set_rate_limit(Some(limit))
        .expect("lower the broker's limit");
    let before = broker.stats().rate_limited;

    let mut god = w.god();
    let mut flood = |seq: u64| match god.publish(FLOODER, seq, &subject, &body) {
        Ok(_) => None,
        Err(e) => {
            let refusal = astream_broker::broker_refusal(&e)
                .filter(|r| r.is_rate_limited())
                .unwrap_or_else(|| panic!("the flood's publish {seq} was refused otherwise: {e}"));
            let wait = refusal
                .retry_after_ms()
                .unwrap_or_else(|| panic!("the refusal names its wait: {e}"));
            assert!(
                wait >= 24 * 60 * 60 * 1000,
                "an empty bucket refilled a byte a second leaves the flood days to wait, not \
                 {wait} ms: {e}"
            );
            Some(wait)
        }
    };
    let tries = 2 * IN_BURST;
    let refused = (1..=tries).filter_map(&mut flood).count();
    assert_eq!(
        u64::try_from(refused).expect("a count"),
        tries - IN_BURST,
        "the burst takes the flood's first {IN_BURST} records, and refuses every one after"
    );

    let cap = w.person_cap("h-x");
    let cap = cap.to_string_lossy();
    let to = format!("to=@{a}@{}", w.node);
    w.fabric_as_person(&[
        "post",
        "--cap-file",
        &cap,
        &to,
        "kind=note",
        "through-the-flood",
    ]);
    let row = until("the person's note in a's inbox", || {
        w.inbox(&a).into_iter().find(|row| row.contains("from=h-x"))
    });
    assert!(
        row.ends_with("text=through-the-flood") && kv(&row, "trust") == Some("human"),
        "the person's note landed whole: {row}"
    );

    assert!(
        flood(tries + 1).is_some(),
        "the flood is still throttled once the person's note has landed"
    );
    let counted = broker.stats().rate_limited - before;
    assert!(
        counted > u64::try_from(refused).expect("a count"),
        "the broker counted each of the flood's {} refusals: {counted}",
        refused + 1
    );
}
