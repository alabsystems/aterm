// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use std::sync::{Arc, Condvar};

/// A gate that opens once `count` callers stand at it: each caller waits for the rest, and
/// says whether the gate opened. A caller the hang detector ends breaks it, and every later
/// caller finds it broken at once, so a run one after another fails in one detector's time.
struct Gate {
    /// How many callers arrived, and whether one gave up on the rest.
    state: Mutex<(usize, bool)>,
    changed: Condvar,
    count: usize,
}

impl Gate {
    fn new(count: usize) -> Arc<Self> {
        Arc::new(Self {
            state: Mutex::new((0, false)),
            changed: Condvar::new(),
            count,
        })
    }

    fn arrive(&self) -> bool {
        let mut state = self.state.lock().expect("gate");
        state.0 += 1;
        self.changed.notify_all();
        let (mut state, waited) = self
            .changed
            .wait_timeout_while(state, aterm_spec::wait::MUST_HAPPEN, |(arrived, broken)| {
                *arrived < self.count && !*broken
            })
            .expect("gate");
        if waited.timed_out() {
            state.1 = true;
            self.changed.notify_all();
        }
        !state.1
    }
}

/// Every item's exchange is under way before any of them ends, for a list as long as the
/// source, the defaults, a full browse and a policy naming a hundred servers ([`MOST_AT_ONCE`]):
/// each one waits at a gate that opens only once all of them stand at it, and every one of
/// them sees it open.
///
/// NEGATIVE CONTROL: run the items one after another (`items.iter().map(|item|
/// Some(exchange(item))).collect()`): the first exchange waits alone until the hang
/// detector ends it, every exchange answers `false`, and the assert fails. Start fewer
/// threads than items (`MOST_AT_ONCE = 4 * MAX_CANDIDATES`, as this module did first): the
/// gate never opens, and the assert fails the same way.
#[test]
fn every_exchange_is_under_way_at_once() {
    const POLICY_SERVERS: usize = 100;
    let servers =
        1 + crate::ALAB_DEFAULT_URLS.len() + crate::discover::MAX_CANDIDATES + POLICY_SERVERS;
    let items: Vec<usize> = (0..servers).collect();
    let gate = Gate::new(items.len());
    let opened = each_at_once("test-fan-out", &items, |&item| (item, gate.arrive()));
    let expected: Vec<Option<(usize, bool)>> =
        items.iter().map(|&item| Some((item, true))).collect();
    assert_eq!(
        opened, expected,
        "never: every exchange under way at once, each result in its item's place"
    );
}

/// A list longer than [`MOST_AT_ONCE`], which has fewer threads than items, still runs every
/// item, each result in its item's place: a thread whose exchange ends goes on to the next
/// item no thread has taken.
///
/// NEGATIVE CONTROL: let `work` return after its first exchange: the items past the
/// [`MOST_AT_ONCE`] the threads took first are never run, stay `None`, and the assert fails.
#[test]
fn a_list_past_the_bound_runs_every_item() {
    let items: Vec<usize> = (0..MOST_AT_ONCE * 2 + 3).collect();
    let results = each_at_once("test-fan-out", &items, |&item| item * 2);
    let expected: Vec<Option<usize>> = items.iter().map(|&item| Some(item * 2)).collect();
    assert_eq!(results, expected);
}

/// An exchange that panics leaves `None` in its place and every other item's result in
/// its own.
#[test]
fn a_panicking_exchange_loses_only_its_own_result() {
    let items = [0usize, 1, 2, 3];
    let results = each_at_once("test-fan-out", &items, |&item| {
        assert_ne!(item, 2, "the test's panic");
        item
    });
    assert_eq!(results, vec![Some(0), Some(1), None, Some(3)]);
}

/// Nothing to do starts nothing; one item runs on this thread.
#[test]
fn no_items_and_one_item_start_no_thread() {
    let none: [usize; 0] = [];
    assert!(each_at_once("test-fan-out", &none, |_| ()).is_empty());
    let here = std::thread::current().id();
    assert_eq!(
        each_at_once("test-fan-out", &[()], |()| std::thread::current().id()),
        vec![Some(here)]
    );
}

/// A post whose shared deadline passed before it began dials nothing and says it timed out:
/// the posts of one fan-out end by their deadline however long the list.
#[test]
fn a_records_post_past_its_deadline_dials_nothing() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind loopback");
    let url = format!("http://{}/v1/alab", listener.local_addr().expect("addr"));
    let past = Instant::now();
    let answered = transport::post_records_answered_until(
        &url,
        b"records",
        RecordsBound::uncrossed(),
        Some(past),
    );
    assert_eq!(
        answered.expect_err("timed out").kind(),
        crate::ErrorKind::TimedOut
    );
    assert!(
        aterm_uds::accept(
            &listener,
            aterm_uds::AcceptWait::Ready,
            aterm_uds::AcceptedMode::Blocking,
        )
        .is_err(),
        "a post past its deadline connected"
    );
}

/// A fan-out reads at most [`ENVELOPE_PULLS_AT_ONCE`] envelopes longer than
/// [`ENVELOPE_PEEK`] whole at once, however many servers it reads, and still reads each of
/// them whole and keeps it in its place: every answer is read first under the peek, all at
/// once, and only one longer than that is asked again and read whole, under a slot. Before,
/// every server's envelope was read whole at once, so a LAN host that advertised 16 instances
/// of itself, each answering with a 16 MiB body, had an admin command hold 256 MiB of bodies
/// nothing had verified. Here the read of each of 16 servers answers its first try with more
/// than the peek and holds its whole read until the test lets it go ([`held::HeldReads`]),
/// and the slots counted are the ones [`reads_within`] makes for its callers.
///
/// NEGATIVE CONTROL: no `slots.take()` in `reads_within` (every whole read at once, as
/// `bodies_within` read before): more whole reads in flight at once, `left: 16, right: 2`,
/// "whole reads in flight at once" (any count over 2 fails it). The slots `reads_within`
/// makes for as many reads at once as a browse names (`PullSlots::new(
/// crate::discover::MAX_CANDIDATES)`): the same.
#[test]
fn a_fan_out_reads_at_most_two_envelopes_whole_at_once() {
    const SERVERS: usize = 16;
    let held = held::HeldReads::new(ENVELOPES);
    let urls: Vec<String> = (0..SERVERS)
        .map(|server| format!("http://server-{server}/v1/alab"))
        .collect();
    let read = |url: &str, _left: Duration, limit: usize| {
        held.read(limit, Fetched::Body(url.as_bytes().to_vec()))
    };
    let keep = |_: &str, read: crate::Result<Pulled>| match read {
        Ok(Pulled::Fetched(Fetched::Body(body))) => Some(body),
        _ => None,
    };
    let counted = held.count(SERVERS, || {
        reads_within(
            ENVELOPES,
            &urls,
            aterm_spec::wait::MUST_HAPPEN * 2,
            read,
            keep,
        )
    });
    assert_eq!(
        counted.seen.in_flight, ENVELOPE_PULLS_AT_ONCE,
        "whole reads in flight at once"
    );
    assert_eq!(
        counted.seen.waiting,
        SERVERS - ENVELOPE_PULLS_AT_ONCE,
        "whole reads waiting for a slot"
    );
    assert_eq!(counted.most, ENVELOPE_PULLS_AT_ONCE);
    assert_eq!(
        held.peeks.load(Ordering::SeqCst),
        SERVERS,
        "every answer was peeked once"
    );
    let expected: Vec<Option<Option<Vec<u8>>>> = urls
        .iter()
        .map(|url| Some(Some(url.as_bytes().to_vec())))
        .collect();
    assert_eq!(
        counted.result, expected,
        "every server's envelope read whole, in its place"
    );
}

/// A loopback server that answers `times` requests, each with `body` in a `200`, and hands
/// back the request heads it read.
fn serving(body: &[u8], times: usize) -> (String, std::thread::JoinHandle<Vec<String>>) {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind loopback");
    let url = format!("http://{}/v1/alab", listener.local_addr().expect("addr"));
    let mut answer = format!(
        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    )
    .into_bytes();
    answer.extend_from_slice(body);
    let server = std::thread::spawn(move || {
        use std::io::{Read, Write};
        let mut heads = Vec::new();
        for _ in 0..times {
            let (mut stream, _) = aterm_uds::accept(
                &listener,
                aterm_uds::AcceptWait::Forever,
                aterm_uds::AcceptedMode::Blocking,
            )
            .expect("accept");
            stream
                .set_read_timeout(Some(aterm_spec::wait::MUST_HAPPEN))
                .expect("timeout");
            let mut head = Vec::new();
            let mut byte = [0u8; 1];
            while !head.ends_with(b"\r\n\r\n") {
                if stream.read(&mut byte).expect("read") == 0 {
                    break;
                }
                head.push(byte[0]);
            }
            heads.push(String::from_utf8_lossy(&head).into_owned());
            // A read under the peek hangs up once it sees the length: the rest goes nowhere.
            let _ = stream.write_all(&answer);
        }
        heads
    });
    (url, server)
}

/// An envelope longer than a first read takes ([`ENVELOPE_PEEK`]) is asked for again and read
/// whole, and one within it is read once: a vault that outgrows the peek still opens from
/// every server that serves it, through the read an admin command's open and a mirror's
/// heads make ([`bodies_within`]).
///
/// NEGATIVE CONTROL: hold the second read to [`ENVELOPE_PEEK`], as the first is
/// (`read_by(url, reads.peek)` in `reads_within`'s slotted `Ok(Pulled::Larger)` arm): it is
/// [`Pulled::Larger`] too, and the long body is lost: `left: [Some(64), None]`, "every body
/// kept whole".
#[test]
fn an_envelope_longer_than_a_first_read_is_asked_again_and_read_whole() {
    let short = vec![7u8; 64];
    let long = vec![9u8; ENVELOPE_PEEK + 1];
    let (short_url, short_server) = serving(&short, 1);
    let (long_url, long_server) = serving(&long, 2);
    let kept = bodies_within(
        &[short_url, long_url],
        aterm_spec::wait::MUST_HAPPEN,
        Some("age1reader"),
        Some,
    );
    let lengths: Vec<Option<usize>> = kept
        .iter()
        .map(|body| body.as_ref().map(Vec::len))
        .collect();
    assert_eq!(
        lengths,
        [Some(short.len()), Some(long.len())],
        "every body kept whole"
    );
    assert!(kept == [Some(short), Some(long)], "each body as served");
    for (server, asked) in [(short_server, 1), (long_server, 2)] {
        let heads = server.join().expect("the server");
        assert_eq!(heads.len(), asked);
        for head in heads {
            assert!(head.starts_with("GET /v1/alab "), "{head}");
            assert!(
                head.to_ascii_lowercase().contains("age1reader"),
                "each read names its reader: {head}"
            );
        }
    }
}
