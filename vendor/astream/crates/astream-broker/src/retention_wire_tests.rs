// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! R4 ON THE WIRE (6E-1c): what a client is told of the rules. A `Fetch` below the oldest
//! offset its filter can reach is refused `ERR retained-from=<off>` (`proto::RETAINED_FROM_PREFIX`)
//! where it was clamped to the base, while one of a held subject whose records were carried
//! reads on; a head query's `Mark` says that oldest (`Client::oldest`); and Hello's extension
//! block advertises the rules (`Client::retention_rules`). Over a real socket, on the broker
//! these tests' parent module drives; the cut is a hand-asked tick on the broker's own clock.
//!
//! NEGATIVE CONTROLS (each a planted break, scratch-reverted, that fails the test named): a
//! `Fetch` that clamps silently (`a_fetch_below_the_cut_is_told_retained_from_and_a_held_mail_reads_on`);
//! a Hello copy that leaves the rules out, and one that advertises the rules a reopened log
//! read back (`hello_advertises_the_rules_the_broker_retains_by`).

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
