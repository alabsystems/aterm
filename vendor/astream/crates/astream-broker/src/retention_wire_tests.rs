// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! R4 ON THE WIRE (6E-1c): what a client is told of the rules. A `Fetch` below the oldest
//! offset its filter can reach is refused `ERR retained-from=<off>` (`proto::RETAINED_FROM_PREFIX`)
//! where it was clamped to the base, while one of a held subject whose records were carried
//! reads on; a head query's `Mark` says that oldest (`Client::oldest`); and Hello's extension
//! block advertises the rules (`Client::retention_rules`). A tail below that oldest is sent a
//! gap notice before any later record, where it asked (R4's addendum, A9; 6E-1d). Over a real
//! socket, on the broker these tests' parent module drives; the cut is a hand-asked tick on
//! the broker's own clock.
//!
//! NEGATIVE CONTROLS (each a planted break, scratch-reverted, that fails the test named): a
//! `Fetch` that clamps silently (`a_fetch_below_the_cut_is_told_retained_from_and_a_held_mail_reads_on`);
//! a Hello copy that leaves the rules out, and one that advertises the rules a reopened log
//! read back (`hello_advertises_the_rules_the_broker_retains_by`); a tail told nothing, and
//! one that writes the notice after the chunk
//! (`a_group_whose_unread_mail_was_given_up_is_told_the_range_and_count_first`); a tail that
//! looks for a notice only when it starts
//! (`a_tail_a_cut_passes_while_it_reads_is_told_before_its_next_record`); a park that drops
//! the notice its wake read (`a_parked_tail_a_cut_passes_is_told_before_the_records_it_wakes_to`);
//! and a broker that tells every tail (`a_client_that_did_not_ask_is_never_sent_a_notice`).

use super::*;
use crate::client::broker_refusal;

const PRESENCE: &str = "/f/F/pub/n1/s1/presence";
const STREAM: &str = "/f/F/pub/*/*/ev";

/// A client on `sock` whose every read gives up after a minute: a hang detector, never a
/// budget.
fn client(sock: &std::path::Path) -> crate::Client {
    let s = std::os::unix::net::UnixStream::connect(sock).unwrap();
    s.set_read_timeout(Some(MINUTE)).unwrap();
    crate::Client::from_stream(s)
}

/// R4'S ACCEPTANCE OVER THE WIRE: the bulk stream retained at 100 records, 251 published (the
/// presence record at 3, an unread mail at 5, the rest the stream). After the tick cut at 151
/// a `Fetch{from: 5}` of the stream is refused `retained-from=151`, permanently, and read from
/// 151 it reads on; the head query on it says oldest 151; the mail, carried, reads at its own
/// offset 5 with no error; and `Last` of the presence subject answers its record at 3. NC: a
/// `Fetch` that clamps (the store's `fetch`) answers a page from 151, and no `retained-from=`.
#[test]
fn a_fetch_below_the_cut_is_told_retained_from_and_a_held_mail_reads_on() {
    let root = Scratch::new("wire-retained-from");
    let path = root.path().join("log");
    let sock = root.path().join("sock");
    let b = Broker::open(&path).unwrap();
    b.set_retention_interval(HOUR);
    for seq in 0..=250u64 {
        let subject = match seq {
            3 => PRESENCE,
            5 => MAIL,
            _ => EV,
        };
        assert_eq!(publish(&b, seq, subject), seq);
    }
    // The rules set on the 251 records: their first tick (the thread's, or this one, which
    // waits for it) cuts at the 100th newest record of the stream, the stream far past its
    // bytes. The broker's own clock, as the thread's: the mail's age reads from it.
    b.set_retention(rules(100)).unwrap();
    b.retention_tick_at(b.now_ms()).unwrap();
    assert_eq!(b.base(), 151);
    let mut h = b.serve(&sock).unwrap();
    let mut c = client(&sock);

    let refused = c
        .fetch(5, STREAM, 10)
        .expect_err("a fetch below the cut answered a page");
    let why = broker_refusal(&refused).expect("a broker refusal");
    assert_eq!(why.retained_from(), Some(151), "{why:?}");
    assert!(why.is_permanent(), "{why:?}");
    let (page, (_, head)) = c.fetch(151, STREAM, 10).unwrap();
    assert_eq!(page.first().map(|r| r.0), Some(151));
    assert_eq!((head, c.seen_oldest()), (251, Some(151)));
    assert_eq!(c.oldest(STREAM).unwrap(), (Some(151), 251));

    let (mail, _) = c.fetch(5, "/f/F/in/>", 10).unwrap();
    let mail: Vec<(u64, &str)> = mail.iter().map(|r| (r.0, r.1.as_str())).collect();
    assert_eq!(mail, vec![(5, MAIL)], "the held mail reads on");
    let (oldest, _) = c.oldest("/f/F/in/>").unwrap();
    assert!(oldest.is_some_and(|o| o <= 5), "{oldest:?}");

    let (last, _) = c.last("/f/F/pub/*/*/presence", "", 10).unwrap();
    let last: Vec<(u64, &str)> = last.iter().map(|r| (r.0, r.1.as_str())).collect();
    assert_eq!(last, vec![(3, PRESENCE)]);
    drop(c);
    h.shutdown();
}

/// HELLO ADVERTISES THE RULES THE BROKER RETAINS BY: before any, a client's `Hello` reads
/// none; once the broker is given them, a later `Hello` on a connection made before reads
/// them back exactly, the default rule included; a broker reopened on the log, whose held
/// store kept them but which retains by nothing until they are set again, advertises none
/// (an embedder that turns a mirror on by Hello's rules must not find them on a log nothing
/// compacts); and set again, they are advertised. NC: a Hello copy that leaves the rules
/// out reads none; one that advertises the rules read back advertises them from the open.
#[test]
fn hello_advertises_the_rules_the_broker_retains_by() {
    let root = Scratch::new("wire-hello-rules");
    let path = root.path().join("log");
    let b = Broker::open(&path).unwrap();
    let mut h = b.serve(root.path().join("sock")).unwrap();
    let mut c = client(&root.path().join("sock"));
    c.hello().unwrap();
    assert_eq!(c.retention_rules(), None, "no rules, none advertised");
    b.set_retention_interval(HOUR);
    b.set_retention(rules(3)).unwrap();
    c.hello().unwrap();
    assert_eq!(c.retention_rules(), Some(rules(3)));
    drop(c);
    h.shutdown();
    drop(h);
    drop(b);

    let b = Broker::open(&path).unwrap();
    assert_eq!(b.retention(), None, "read back, not retained by");
    assert_eq!(
        b.shared.log.lock().unwrap().retention(),
        Some(&rules(3)),
        "the held store kept them, and reads answer by them"
    );
    let mut h = b.serve(root.path().join("sock2")).unwrap();
    let mut c = client(&root.path().join("sock2"));
    c.hello().unwrap();
    assert_eq!(c.retention_rules(), None, "a broker retaining by nothing");
    b.set_retention(rules(3)).unwrap();
    assert_eq!(b.retention(), Some(rules(3)));
    c.hello().unwrap();
    assert_eq!(c.retention_rules(), Some(rules(3)), "set again: advertised");
    drop(c);
    h.shutdown();
}

const INBOX: &str = "/f/F/cur/n1/node/inbox";
const MAILS: &str = "/f/F/in/>";

/// The stream the prefix, keeping its newest 2 `ev` records by their bytes; mail held until
/// its node's inbox group reads it or for 7 days, and at most 3 mails' bytes: past that the
/// oldest mail is given up unread.
fn gap_rules() -> Retention {
    Retention::new(
        vec![
            Rule::prefix(STREAM).unwrap().max_bytes(2 * rb(EV)),
            Rule::hold(MAILS)
                .unwrap()
                .max_bytes(3 * rb(MAIL))
                .max_age(Duration::from_secs(7 * 86_400))
                .until_read("/f/{2}/cur/{4}/node/inbox")
                .unwrap(),
        ],
        Rule::default_hold(),
    )
    .unwrap()
}

/// Five unread mails at 0 to 4, then three stream records at 5 to 7.
fn publish_mail_then_stream(b: &Broker) {
    for (seq, subject) in [MAIL, MAIL, MAIL, MAIL, MAIL, EV, EV, EV]
        .into_iter()
        .enumerate()
    {
        assert_eq!(publish(b, seq as u64, subject), seq as u64);
    }
}

/// [`gap_rules`] set and a tick on the broker's own clock: the base at 6 (the newest two
/// stream records kept), the three newest mails held, and the mails at 0 and 1 given up.
fn cut_by_gap_rules(b: &Broker) {
    b.set_retention(gap_rules()).unwrap();
    b.retention_tick_at(b.now_ms()).unwrap();
    assert_eq!(b.base(), 6);
    assert_eq!(b.stats().log.held_records, 3);
}

/// What a tail delivered or was told, its marks skipped.
#[derive(Debug, PartialEq, Eq)]
enum Seen {
    Gap(crate::proto::Gap),
    Rec(u64),
}

/// The next `n` deliveries and notices on `sub`, the marks between them skipped, within a
/// minute: a hang detector, never a budget.
fn seen(sub: &mut crate::client::Subscription, n: usize) -> Vec<Seen> {
    let deadline = Instant::now() + MINUTE;
    let mut out = Vec::new();
    while out.len() < n {
        assert!(Instant::now() < deadline, "not within a minute: {out:?}");
        match sub.recv_event().unwrap() {
            Some(crate::client::Event::Delivery { offset, .. }) => out.push(Seen::Rec(offset)),
            Some(crate::client::Event::Gap(g)) => out.push(Seen::Gap(g)),
            Some(crate::client::Event::Mark { .. }) => {}
            None => panic!("the broker closed the subscription: {out:?}"),
        }
    }
    out
}

fn told(from: u64, to: u64, records: Option<u64>) -> Seen {
    Seen::Gap(crate::proto::Gap { from, to, records })
}

/// A GROUP IS TOLD WHAT RETENTION GAVE UP BEFORE IT READ (R4's addendum, A9): the inbox
/// group never read its five mails, and the mail class's bytes gave up the two oldest.
/// Subscribed, it is told the range `0..2` and the count, 2, before its first mail, then
/// delivered the three held. NC: a tail that is told nothing (no `Told`) reads the mails
/// alone; one that writes the notice after the chunk reads the mail at 2 first.
#[test]
fn a_group_whose_unread_mail_was_given_up_is_told_the_range_and_count_first() {
    let root = Scratch::new("wire-gap-group");
    let path = root.path().join("log");
    let sock = root.path().join("sock");
    let b = Broker::open(&path).unwrap();
    b.set_retention_interval(HOUR);
    publish_mail_then_stream(&b);
    cut_by_gap_rules(&b);
    let mut h = b.serve(&sock).unwrap();
    let mut sub = client(&sock).subscribe_group(INBOX, MAILS).unwrap();
    assert_eq!(
        seen(&mut sub, 4),
        vec![
            told(0, 2, Some(2)),
            Seen::Rec(2),
            Seen::Rec(3),
            Seen::Rec(4)
        ]
    );
    assert_eq!(
        sub.take_gap(),
        Some(crate::proto::Gap {
            from: 0,
            to: 2,
            records: Some(2)
        })
    );
    drop(sub);
    h.shutdown();
}

/// A TAIL A CUT PASSES WHILE IT READS is told before its next record: paused after its first
/// chunk of one record (the mail at 0, delivered), the rules land and give up the mails at
/// 0 and 1. Resumed at 1, it is told `1..2`, counting 1 (the mail at 0 it was delivered is
/// not counted), then delivered the three held mails. NC: a tail that looks for a notice
/// only when it starts reads the mail at 2 with nothing told.
#[test]
fn a_tail_a_cut_passes_while_it_reads_is_told_before_its_next_record() {
    let root = Scratch::new("wire-gap-midway");
    let path = root.path().join("log");
    let sock = root.path().join("sock");
    let b = Broker::open(&path).unwrap();
    b.set_retention_interval(HOUR);
    publish_mail_then_stream(&b);
    let (reached_tx, reached) = mpsc::channel();
    let (resume, resume_rx) = mpsc::channel();
    *b.shared.tail_pause.lock().unwrap() = Some(TailPause {
        chunk: 1,
        at: PauseAt::FirstChunk,
        reached: reached_tx,
        resume: resume_rx,
    });
    let mut h = b.serve(&sock).unwrap();
    let mut sub = client(&sock).subscribe(0, MAILS).unwrap();
    assert_eq!(seen(&mut sub, 1), vec![Seen::Rec(0)]);
    assert_eq!(
        reached.recv_timeout(MINUTE),
        Ok(1),
        "paused after its first chunk"
    );
    cut_by_gap_rules(&b);
    resume.send(()).unwrap();
    assert_eq!(
        seen(&mut sub, 4),
        vec![
            told(1, 2, Some(1)),
            Seen::Rec(2),
            Seen::Rec(3),
            Seen::Rec(4)
        ]
    );
    drop(sub);
    h.shutdown();
}

/// A PARKED TAIL IS TOLD BEFORE THE RECORDS IT WAKES TO: subscribed to the mail of an empty
/// log, the tail is caught up and stops on its way to park. Then the five mails and three
/// stream records land, and the rules give up the mails at 0 and 1, all before it parks. Its
/// park sees the head past it at once and reads on under the same lock: it is told `0..2`,
/// counting 2, then delivered the three held mails. NC: a park that drops its notice reads
/// the mail at 2 with nothing told.
#[test]
fn a_parked_tail_a_cut_passes_is_told_before_the_records_it_wakes_to() {
    let root = Scratch::new("wire-gap-park");
    let path = root.path().join("log");
    let sock = root.path().join("sock");
    let b = Broker::open(&path).unwrap();
    b.set_retention_interval(HOUR);
    let (reached_tx, reached) = mpsc::channel();
    let (resume, resume_rx) = mpsc::channel();
    *b.shared.tail_pause.lock().unwrap() = Some(TailPause {
        chunk: TAIL_CHUNK,
        at: PauseAt::Park,
        reached: reached_tx,
        resume: resume_rx,
    });
    let mut h = b.serve(&sock).unwrap();
    let mut sub = client(&sock).subscribe(0, MAILS).unwrap();
    assert_eq!(
        reached.recv_timeout(MINUTE),
        Ok(0),
        "caught up on the empty log, about to park"
    );
    publish_mail_then_stream(&b);
    cut_by_gap_rules(&b);
    resume.send(()).unwrap();
    assert_eq!(
        seen(&mut sub, 4),
        vec![
            told(0, 2, Some(2)),
            Seen::Rec(2),
            Seen::Rec(3),
            Seen::Rec(4)
        ]
    );
    drop(sub);
    h.shutdown();
}

/// A NOTICE IS NEVER SENT TO A CLIENT THAT DID NOT ASK: a connection that asked for idle
/// marks alone, as a client that predates gap notices does, is delivered the held mail at 2
/// first, as before. Hello announces the verb. NC: a broker that tells every tail reads the
/// notice first.
#[test]
fn a_client_that_did_not_ask_is_never_sent_a_notice() {
    use crate::proto::{decode_response, encode_request, read_frame, write_frame};
    let root = Scratch::new("wire-gap-unasked");
    let path = root.path().join("log");
    let sock = root.path().join("sock");
    let b = Broker::open(&path).unwrap();
    b.set_retention_interval(HOUR);
    publish_mail_then_stream(&b);
    cut_by_gap_rules(&b);
    let mut h = b.serve(&sock).unwrap();
    let mut c = client(&sock);
    c.hello().unwrap();
    assert!(c.supports_gap_notices(), "Hello announces the verb");
    drop(c);

    let mut s = std::os::unix::net::UnixStream::connect(&sock).unwrap();
    s.set_read_timeout(Some(MINUTE)).unwrap();
    let next = |s: &mut std::os::unix::net::UnixStream| {
        decode_response(&read_frame(s).unwrap().expect("a frame within a minute"))
    };
    write_frame(&mut s, &encode_request(&Request::IdleMarks)).unwrap();
    assert!(matches!(next(&mut s), Some(Response::Mark { .. })));
    let group = Request::SubscribeGroup {
        group: INBOX.into(),
        filter: MAILS.into(),
    };
    write_frame(&mut s, &encode_request(&group)).unwrap();
    let first = next(&mut s);
    assert!(
        matches!(first, Some(Response::Delivery { offset: 2, .. })),
        "{first:?}"
    );
    drop(s);
    h.shutdown();
}
