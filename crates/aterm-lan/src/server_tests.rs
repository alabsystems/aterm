// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use std::cell::RefCell;
use std::net::{Ipv4Addr, SocketAddr, TcpListener};

use super::*;

thread_local! {
    static LINES: RefCell<Vec<String>> = const { RefCell::new(Vec::new()) };
    static CLOCK: RefCell<Option<Instant>> = const { RefCell::new(None) };
}

fn clock() -> Instant {
    CLOCK.with_borrow(|now| now.expect("test retry clock"))
}

fn record(words: std::fmt::Arguments<'_>) {
    LINES.with_borrow_mut(|lines| lines.push(words.to_string()));
}

struct NoBonjour;

impl Bonjour for NoBonjour {
    fn start(&self, _: &Request) -> Result<Box<dyn bonjour::Live>, Failure> {
        panic!("a connect uses no Bonjour operation")
    }
}

fn request(addr: SocketAddr) -> Request {
    Request::Connect {
        target: Target::Addr(addr),
        port: addr.port(),
        within_ms: 60_000,
        bound_if: None,
        source: None,
        hop_limit: None,
    }
}

/// The real request path shares a failed destination across clients. Even after the port
/// starts listening, its immediate retry returns the original error and writes no line.
/// NEGATIVE CONTROL: advancing its retry clock reaches the very same listening port. Even
/// an abandoned descriptor handoff ends the outage, so the next caller can connect at once.
/// This would fail if only an unused helper backed off or handoff failures poisoned peers.
#[test]
fn request_retries_use_the_shared_peer_clock_and_not_the_requesters_pid() {
    aterm_uds::lan::seam_here(
        aterm_uds::lan::Seam {
            loopback_is_local: true,
            ..aterm_uds::lan::Seam::default()
        },
        || {
            CLOCK.with_borrow_mut(|now| *now = Some(Instant::now()));
            let mut shared = Shared::new(Admission::SameUid, Box::new(NoBonjour), record);
            shared.clock = clock;
            let target = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
            let addr = target.local_addr().unwrap();
            drop(target);
            let (server, _client) = CtlStream::pair().unwrap();
            let before = answer(&shared, &server, request(addr), &mut || {}).unwrap_err();
            assert_eq!(before.0, libc::ECONNREFUSED);
            let target = TcpListener::bind(addr).unwrap();
            let (server, _client) = CtlStream::pair().unwrap();
            assert_eq!(
                answer(&shared, &server, request(addr), &mut || {}).unwrap_err(),
                before
            );
            LINES.with_borrow(|lines| assert_eq!(lines.len(), 1, "{lines:?}"));

            // The retry becomes due by advancing only the peer clock. A successful dial
            // followed by a vanished recipient clears the outage before the failed handoff.
            CLOCK.with_borrow_mut(|now| *now = Some(now.unwrap() + Duration::from_secs(1)));
            let (server, client) = CtlStream::pair().unwrap();
            let abandoning = std::thread::spawn(move || {
                let mut reader = Reader::new(1);
                assert!(matches!(
                    reader
                        .frame(&client, Some(Instant::now() + TAKE_HANG))
                        .unwrap(),
                    Some(Frame::Socket { .. })
                ));
                assert!(reader.take_fd().is_some());
            });
            assert!(answer(&shared, &server, request(addr), &mut || {}).is_err());
            abandoning.join().unwrap();
            LINES.with_borrow(|lines| {
                assert_eq!(lines.len(), 2, "{lines:?}");
                assert!(lines[1].ends_with("reachable"));
            });
            let (server, client) = CtlStream::pair().unwrap();
            let taking = std::thread::spawn(move || {
                let mut reader = Reader::new(1);
                assert!(matches!(
                    reader
                        .frame(&client, Some(Instant::now() + TAKE_HANG))
                        .unwrap(),
                    Some(Frame::Socket { .. })
                ));
                let socket = reader.take_fd().expect("the connected descriptor");
                wire::took(&client).unwrap();
                socket
            });
            answer(&shared, &server, request(addr), &mut || {}).unwrap();
            drop(taking.join().unwrap());
            drop(target);
            LINES.with_borrow(|lines| assert_eq!(lines.len(), 2));
        },
    );
}
