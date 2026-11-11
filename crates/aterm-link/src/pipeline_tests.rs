// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

/// Serve a scratch broker and connect a [`Link`] to it, with a second plain
/// client for reading what landed.
fn served() -> (
    aterm_tempfile::TempDir,
    astream_broker::BrokerHandle,
    Link<u32>,
    Conn,
) {
    let dir = aterm_tempfile::Builder::new()
        .prefix("al-p")
        .suffix(".noindex")
        .tempdir_in("/tmp")
        .expect("a scratch dir");
    let socket = dir.path().join("b.sock");
    let broker = crate::test_broker(dir.path().join("b.log")).expect("a broker");
    let server = broker.serve(&socket).expect("serve");
    let connect = || {
        let (conn, _) = crate::transport::connect(
            &socket.to_string_lossy(),
            std::time::Duration::from_secs(60),
        )
        .expect("dial");
        conn
    };
    (dir, server, Link::from(connect()), connect())
}

/// **A BURST GOES OUT WHOLE, AND EACH ANSWER FINDS ITS OWN RECORD.** Ten
/// publishes are written before one ack is read; while any is owed the
/// client is not handed out for a request that reads its own reply; the
/// acks come back in send order, each naming its own sequence and its own
/// offset; and once they are read the connection serves a `Fetch` that
/// sees all ten.
#[test]
fn a_burst_is_sent_before_any_ack_and_every_answer_is_its_own() {
    let (_dir, _server, mut link, mut reader) = served();
    for i in 0..10u32 {
        let subject = if i == 4 {
            "/f/f//empty-segment".to_string()
        } else {
            format!("/f/f/x/{i}")
        };
        link.send(7, u64::from(i) + 1, true, &subject, b"b", i, Instant::now())
            .map_err(|(e, _)| e)
            .expect("written");
    }
    assert_eq!(link.unacked(), 10);
    assert_eq!(link.peak(), 10, "all ten were on the wire at once");
    assert!(
        link.ready().is_none(),
        "no request may read a reply past an owed ack"
    );
    let mut answered = Vec::new();
    while let Some((owed, answer)) = link.recv() {
        assert_eq!(owed.seq, u64::from(owed.tag) + 1);
        match answer {
            Ok((off, deduped)) => {
                assert!(!deduped);
                answered.push((owed.tag, Some(off)));
            }
            Err(e) => {
                let refusal = astream_broker::broker_refusal(&e).expect("a refusal");
                assert_eq!(refusal.code, astream_broker::proto::ERR_BAD_SUBJECT);
                answered.push((owed.tag, None));
            }
        }
    }
    assert_eq!(
        answered.iter().map(|(tag, _)| *tag).collect::<Vec<_>>(),
        (0..10).collect::<Vec<_>>(),
        "in send order"
    );
    assert_eq!(answered[4].1, None, "the refusal is the bad subject's own");
    let landed: Vec<u64> = answered.iter().filter_map(|(_, off)| *off).collect();
    assert_eq!(landed.len(), 9);
    assert!(
        landed.windows(2).all(|w| w[0] < w[1]),
        "one connection, in order"
    );
    let conn = link
        .ready()
        .expect("nothing owed: the client is handed out");
    let (rows, _) = conn
        .fetch(0, "/f/f/x/*", 64)
        .expect("a fetch after the acks");
    assert_eq!(
        rows.iter().map(|(off, _, _)| *off).collect::<Vec<_>>(),
        landed,
        "the fetch reads records, not a stray ack"
    );
    let (rows, _) = reader
        .fetch(0, "/f/f/x/*", 64)
        .expect("a second client agrees");
    assert_eq!(rows.len(), 9);
}

/// **A REQUEST BEHIND AN OWED ACK FAILS LOUDLY, AND SENDS NOTHING** — the
/// FIRST guard, astream's own, reached the one way nothing
/// in this crate can: past [`Link::ready`], straight to the client.
#[test]
fn a_request_behind_an_owed_ack_fails_loudly_and_sends_nothing() {
    let (_dir, _server, mut link, _reader) = served();
    link.send(7, 1, true, "/f/f/owed/a", b"a", 0, Instant::now())
        .map_err(|(e, _)| e)
        .expect("A written");
    assert!(link.ready().is_none(), "the second guard holds the client");
    let fetched = link
        .client
        .fetch(0, "/f/f/owed/*", 64)
        .expect_err("a fetch behind an owed ack is refused, never answered");
    assert_eq!(
        astream_broker::acks_owed(&fetched),
        Some(&astream_broker::AcksOwed { owed: 1 }),
        "astream's typed refusal, naming the ack owed: {fetched}"
    );
    assert_eq!(fetched.kind(), io::ErrorKind::ResourceBusy);
    let committed = link
        .client
        .commit("/f/f/owed/g", 0)
        .expect_err("a commit behind an owed ack is refused");
    assert!(
        astream_broker::acks_owed(&committed).is_some(),
        "{committed}"
    );

    let (owed, answer) = link.recv().expect("A's ack is still owed");
    let (a_at, deduped) = answer.expect("A's own ack, not a fetch's rows");
    assert_eq!((owed.tag, deduped), (0, false));
    let conn = link.ready().expect("nothing owed");
    let (b_at, _) = conn
        .publish(7, 2, "/f/f/owed/b", b"b")
        .expect("in step: nothing the refusals wrote is on the stream");
    assert_eq!(b_at, a_at + 1);
    let (rows, _) = conn
        .fetch(0, "/f/f/owed/*", 64)
        .expect("NEGATIVE CONTROL: the same fetch, nothing owed, is answered");
    assert_eq!(
        rows.iter().map(|(off, _, _)| *off).collect::<Vec<_>>(),
        [a_at, b_at]
    );
}

/// **A PIPELINED COMMIT IS OWED LIKE A PUBLISH, IN ORDER, AND A TRY WAITS
/// FOR NOTHING** — on a real broker.
#[test]
fn a_pipelined_commit_is_owed_in_order_and_a_try_waits_for_nothing() {
    let dir = aterm_tempfile::Builder::new()
        .prefix("al-p")
        .suffix(".noindex")
        .tempdir_in("/tmp")
        .expect("a scratch dir");
    let socket = dir.path().join("b.sock");
    let broker = crate::test_broker(dir.path().join("b.log")).expect("a broker");
    let _server = broker.serve(&socket).expect("serve");
    let dial = || {
        crate::transport::connect(
            &socket.to_string_lossy(),
            std::time::Duration::from_secs(60),
        )
        .expect("dial")
        .0
    };
    let mut link: Link<&str> = Link::from(dial());
    let (group, filter) = ("/f/f/cur/n/node/inbox", "/f/f/c/*");

    link.send(7, 1, true, "/f/f/c/a", b"a", "a", Instant::now())
        .map_err(|(e, _)| e)
        .expect("A written");
    let (owed, answer) = link.recv().expect("A's ack");
    let (a_at, _) = answer.expect("A landed");
    assert_eq!(owed.tag, "a");

    let held = broker.pause_next_acks();
    link.send_commit(group, a_at, "commit", Instant::now())
        .map_err(|(e, _)| e)
        .expect("the commit written");
    assert!(held.wait(), "the commit's batch is held unanswered");
    link.send(7, 2, true, "/f/f/c/b", b"b", "b", Instant::now())
        .map_err(|(e, _)| e)
        .expect("B written behind the commit");
    assert_eq!(link.unacked(), 2);
    assert!(link.ready().is_none(), "nothing reads past an owed commit");
    let blocking = link
        .client
        .commit(group, a_at)
        .expect_err("NEGATIVE CONTROL: the blocking commit behind owed acks");
    assert!(
        astream_broker::acks_owed(&blocking).is_some(),
        "refused, nothing sent: {blocking}"
    );
    assert!(
        link.try_recv().is_none(),
        "the commit's answer is held: a try does not wait for it"
    );
    assert_eq!(link.unacked(), 2, "a try that found nothing pays nothing");
    held.resume();

    let (owed, answer) = link.recv().expect("the commit's ack is owed first");
    assert_eq!(owed.tag, "commit");
    assert_eq!((owed.seq, owed.first), (0, false));
    let (commit_at, deduped) = answer.expect("the commit landed");
    assert!(!deduped);
    let (owed, answer) = link.recv().expect("B's ack after it");
    assert_eq!(owed.tag, "b");
    let (b_at, _) = answer.expect("B landed");
    assert!(
        a_at < commit_at && commit_at < b_at,
        "in send order: A @{a_at}, the commit @{commit_at}, B @{b_at}"
    );
    assert!(link.try_recv().is_none(), "nothing owed: nothing tried");

    let first = |group: &str| {
        let mut sub = dial().subscribe_group(group, filter).expect("subscribe");
        sub.recv().expect("a delivery").expect("not the end").0
    };
    assert_eq!(first(group), b_at, "the commit moved the group past A");
    assert_eq!(
        first("/f/f/cur/m/node/inbox"),
        a_at,
        "NEGATIVE CONTROL: a group nobody committed starts at A"
    );
}

/// **A COMMIT AND A BURST OF PUBLISHES GO IN ONE WRITE AND ONE BATCH, THE COMMIT FIRST**
/// (critic item C13) — on a real broker: their acks come back in the burst's order, the
/// commit's offset below every publish's, all in one writer batch, and the group resumes
/// past what the commit named. A burst with no commit owes its publishes alone.
#[test]
fn a_commit_and_a_burst_of_publishes_go_in_one_write_and_one_batch() {
    let dir = aterm_tempfile::Builder::new()
        .prefix("al-p")
        .suffix(".noindex")
        .tempdir_in("/tmp")
        .expect("a scratch dir");
    let socket = dir.path().join("b.sock");
    let broker = crate::test_broker(dir.path().join("b.log")).expect("a broker");
    let _server = broker.serve(&socket).expect("serve");
    let dial = || {
        crate::transport::connect(
            &socket.to_string_lossy(),
            std::time::Duration::from_secs(60),
        )
        .expect("dial")
        .0
    };
    let mut link: Link<&str> = Link::from(dial());
    let (group, filter) = ("/f/f/cur/n/node/inbox", "/f/f/c/*");
    link.send(7, 1, true, "/f/f/c/a", b"a", "a", Instant::now())
        .map_err(|(e, _)| e)
        .expect("A written");
    let (_, answer) = link.recv().expect("A's ack");
    let (a_at, _) = answer.expect("A landed");

    let publishes = ["s1", "s2", "s3"]
        .into_iter()
        .zip(2u64..)
        .map(|(tag, seq)| Publish {
            producer_id: 7,
            seq,
            first: true,
            subject: "/f/f/pub/n/s/ev",
            body: tag.as_bytes(),
            tag,
        })
        .collect();
    let batches = broker.stats().writer_batches;
    link.send_burst(
        Some(HeldCommit {
            group,
            upto: a_at,
            tag: "commit",
        }),
        publishes,
        Instant::now(),
    )
    .map_err(|unsent| unsent.error)
    .expect("the burst written");
    assert_eq!(link.unacked(), 4, "the commit and three publishes owed");
    let answered: Vec<(&str, u64)> = std::iter::from_fn(|| link.recv())
        .map(|(owed, answer)| (owed.tag, answer.expect("landed").0))
        .collect();
    assert_eq!(
        answered.iter().map(|(tag, _)| *tag).collect::<Vec<_>>(),
        ["commit", "s1", "s2", "s3"],
        "in the burst's order"
    );
    assert!(
        answered.windows(2).all(|w| w[0].1 < w[1].1),
        "the commit below every publish: {answered:?}"
    );
    assert_eq!(
        broker.stats().writer_batches - batches,
        1,
        "one writer batch for the whole burst"
    );
    let mut sub = dial().subscribe_group(group, filter).expect("subscribe");
    link.send_burst(
        None,
        vec![Publish {
            producer_id: 7,
            seq: 5,
            first: true,
            subject: "/f/f/c/b",
            body: b"b",
            tag: "b",
        }],
        Instant::now(),
    )
    .map_err(|unsent| unsent.error)
    .expect("B written alone");
    let (owed, answer) = link.recv().expect("B's ack, and no other");
    assert_eq!(owed.tag, "b");
    let (b_at, _) = answer.expect("B landed");
    assert_eq!(
        sub.recv().expect("a delivery").expect("not the end").0,
        b_at,
        "the commit moved the group past A"
    );
}

/// **A RIDER COMES BACK WITH THE ACK IT WAS HUNG ON, AND ONLY THEN.** It
/// hangs on the newest owed ack its predicate names — not on a newer one
/// it does not — keeps the larger of two values, comes back with that
/// ack's answer and with no other, and finds nothing to hang on while
/// only acks it does not name are owed.
#[test]
fn a_rider_comes_back_with_the_ack_it_was_hung_on() {
    let (_dir, _server, mut link, _reader) = served();
    let send = |link: &mut Link<u32>, tag: u32| {
        link.send(
            7,
            u64::from(tag),
            true,
            &format!("/f/f/r/{tag}"),
            b"b",
            tag,
            Instant::now(),
        )
        .map_err(|(e, _)| e)
        .expect("written");
    };
    assert!(!link.ride(1, |_| true), "nothing owed: nothing to wait on");
    send(&mut link, 2);
    send(&mut link, 4);
    send(&mut link, 5);
    assert!(link.ride(10, |tag| tag % 2 == 0));
    assert!(link.ride(9, |tag| tag % 2 == 0), "the larger value stays");
    assert!(link.riding());
    let riders: Vec<(u32, Option<u64>)> = std::iter::from_fn(|| link.recv())
        .map(|(owed, answer)| {
            answer.expect("landed");
            (owed.tag, owed.rider)
        })
        .collect();
    assert_eq!(
        riders,
        [(2, None), (4, Some(10)), (5, None)],
        "on the newest ack it waits on, not on the newer one it does not"
    );
    assert!(!link.riding());
    send(&mut link, 7);
    assert!(
        !link.ride(11, |tag| tag % 2 == 0),
        "only an ack it does not wait on is owed"
    );
    send(&mut link, 8);
    assert!(link.ride(12, |tag| tag % 2 == 0));
}

/// **A REFUSED BATCH LANDS THE PUBLISH SENT AFTER IT** on a broker that does not gap-fence —
/// the hazard that makes a caller whose records must land in its own order keep one of
/// them in flight at a time there (`Bridge::settle_sender`), shown on a REAL Strict broker
/// rather than a scripted one. Its `Hello` does not say it gap-fences.
#[test]
fn a_refused_batch_lands_the_publish_sent_after_it() {
    let dir = aterm_tempfile::Builder::new()
        .prefix("al-p")
        .suffix(".noindex")
        .tempdir_in("/tmp")
        .expect("a scratch dir");
    let socket = dir.path().join("b.sock");
    let broker = astream_broker::Broker::open(dir.path().join("b.log")).expect("a broker");
    let _server = broker.serve(&socket).expect("serve");
    let (conn, _) = crate::transport::connect(
        &socket.to_string_lossy(),
        std::time::Duration::from_secs(60),
    )
    .expect("dial");
    let mut link: Link<char> = Link::from(conn);
    link.ready()
        .expect("nothing owed")
        .hello()
        .expect("a Hello");
    assert!(!link.gap_fences(), "its Hello says nothing of a fence");
    let (a, b) = ("/f/f/order/a", "/f/f/order/b");

    broker.inject_faults(astream_broker::InjectedFaults {
        write: false,
        sync: true,
        truncate: false,
    });
    let held = broker.pause_next_acks();
    link.send(7, 1, true, a, b"a", 'a', Instant::now())
        .map_err(|(e, _)| e)
        .expect("A written");
    assert!(held.wait(), "A's batch was tried and is held unanswered");
    broker.inject_faults(astream_broker::InjectedFaults::default());
    link.send(7, 2, true, b, b"b", 'b', Instant::now())
        .map_err(|(e, _)| e)
        .expect("B written while A's answer is held");
    held.resume();

    let (owed, answer) = link.recv().expect("A's ack is owed");
    assert_eq!(owed.tag, 'a');
    let refused = answer.expect_err("A's batch failed its fsync");
    assert_eq!(
        astream_broker::broker_refusal(&refused).map(|r| r.code),
        Some(astream_broker::proto::ERR_REFUSED),
        "refused, and not known to be for good: {refused}"
    );
    let (owed, answer) = link.recv().expect("B's ack is owed");
    assert_eq!(owed.tag, 'b');
    let (b_at, deduped) = answer.expect("B landed after A's refusal");
    assert!(!deduped);

    let conn = link.ready().expect("nothing owed");
    let (a_at, deduped) = conn.publish(7, 1, a, b"a").expect("A's retry lands");
    assert!(!deduped, "A never landed: its retry appends");
    assert!(
        a_at > b_at,
        "A was sent first and is on the log after B (@{a_at} > @{b_at})"
    );
}

/// **A GAP-FENCED BROKER REFUSES THE PUBLISH SENT AFTER A REFUSED BATCH**, and its producer
/// lifts the fence once it has read both answers: then the refused one's retry lands first
/// and the later one's after it, in send order. The broker offers it in its `Hello` and the
/// link asks for it ([`Link::gap_fences`]), which is what lets the bridge keep several of a
/// sender's posts in flight; the control is [`a_refused_batch_lands_the_publish_sent_after_it`]'s broker,
/// opened without the fence, whose `Hello` says nothing.
#[test]
fn a_gap_fenced_broker_holds_the_publish_sent_after_a_refused_batch() {
    let dir = aterm_tempfile::Builder::new()
        .prefix("al-g")
        .suffix(".noindex")
        .tempdir_in("/tmp")
        .expect("a scratch dir");
    let socket = dir.path().join("b.sock");
    let broker = astream_broker::Broker::open_opts(
        dir.path().join("b.log"),
        astream_broker::OpenOptions::new(astream_broker::Durability::Strict)
            .with_sticky_batch_refusal(true),
        || Ok(astream_broker::Prepared::Nothing),
    )
    .expect("a broker");
    let _server = broker.serve(&socket).expect("serve");
    let (conn, _) = crate::transport::connect(
        &socket.to_string_lossy(),
        std::time::Duration::from_secs(60),
    )
    .expect("dial");
    let mut link: Link<char> = Link::from(conn);
    link.ready()
        .expect("nothing owed")
        .hello()
        .expect("a Hello");
    assert!(
        !link.gap_fences(),
        "its Hello offers the fence, and the link has not asked"
    );
    link.ask_gap_fences().expect("the link asks for the fence");
    assert!(link.gap_fences(), "it asked where the Hello offered it");
    let (a, b) = ("/f/f/order/a", "/f/f/order/b");

    broker.inject_faults(astream_broker::InjectedFaults {
        write: false,
        sync: true,
        truncate: false,
    });
    let held = broker.pause_next_acks();
    link.send(7, 1, true, a, b"a", 'a', Instant::now())
        .map_err(|(e, _)| e)
        .expect("A written");
    assert!(held.wait(), "A's batch was tried and is held unanswered");
    broker.inject_faults(astream_broker::InjectedFaults::default());
    link.send(7, 2, true, b, b"b", 'b', Instant::now())
        .map_err(|(e, _)| e)
        .expect("B written while A's answer is held");
    held.resume();

    let (owed, answer) = link.recv().expect("A's ack is owed");
    assert_eq!(owed.tag, 'a');
    answer.expect_err("A's batch failed its fsync");
    link.note_refused();
    assert!(link.fenced());
    let (owed, answer) = link.recv().expect("B's ack is owed");
    assert_eq!(owed.tag, 'b');
    let fenced = answer.expect_err("B is held behind A's refusal");
    assert_eq!(
        astream_broker::broker_refusal(&fenced)
            .and_then(|r| astream_broker::sticky_fence::parse_gap_fenced_seq(&r.msg)),
        Some(1),
        "refused behind A's sequence: {fenced}"
    );

    link.lift_gap_fences()
        .expect("nothing owed")
        .expect("the broker lifts the fence");
    assert!(!link.fenced());
    let conn = link.ready().expect("nothing owed");
    let (a_at, deduped) = conn.publish(7, 1, a, b"a").expect("A's retry lands");
    assert!(!deduped, "A never landed: its retry appends");
    let (b_at, deduped) = conn.publish(7, 2, b, b"b").expect("B's retry lands");
    assert!(!deduped, "B never landed: its retry appends");
    assert!(a_at < b_at, "A is on the log before B (@{a_at} < @{b_at})");
}
