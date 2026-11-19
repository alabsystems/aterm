// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use std::io::{BufRead as _, Read as _, Write as _};
use std::os::fd::AsFd as _;
use std::os::unix::net::UnixStream;

/// The subscription has actually crossed its socket, but the reader cannot take any reply
/// until the independent command-lane failure and final push bytes have been arranged.
fn gated_push(bridge: &mut Bridge) -> (UnixStream, std::sync::mpsc::Sender<()>) {
    let (peer, push) = UnixStream::pair().unwrap();
    peer.set_read_timeout(Some(aterm_spec::wait::MUST_HAPPEN))
        .unwrap();
    let (release, wait) = std::sync::mpsc::channel();
    bridge.event_reader = Some(
        spawn_event_reader_with(
            Ctl::from_stream(push).unwrap(),
            bridge.mailbox.clone(),
            move || match wait.recv_timeout(aterm_spec::wait::MUST_HAPPEN) {
                Ok(()) | Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {}
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                    panic!("the fixture did not release its reader gate")
                }
            },
        )
        .expect("the owned reader"),
    );
    let mut request = String::new();
    std::io::BufReader::new(peer.try_clone().unwrap())
        .read_line(&mut request)
        .unwrap();
    assert_eq!(request, "subscribe @* events,sessions\n");
    (peer, release)
}

fn lose_command(bridge: &mut Bridge) {
    bridge.ctl.shutdown(std::net::Shutdown::Both).unwrap();
    bridge
        .ctl_request("sessions")
        .expect_err("the command lane is lost");
    assert!(bridge.ctl.lost());
    assert!(bridge.mailbox.command_lost());
    assert_eq!(
        bridge.mailbox.aterm_end(),
        None,
        "the other lane has not ended"
    );
}

fn run_record(bridge: &Bridge) -> String {
    std::fs::read_to_string(bridge.state.root().join("run")).unwrap()
}

#[test]
fn command_loss_drains_a_queued_final_word_and_real_eof_before_ending_the_run() {
    for (word, bye, clean) in [("handoff", Bye::Handoff, false), ("quit", Bye::Quit, true)] {
        let (_lane, mut bridge) = FakeVerbLane::bridge(&[]);
        bridge.state.begin_run().unwrap();
        let open = run_record(&bridge);
        assert!(open.starts_with("open "));
        let (mut peer, release) = gated_push(&mut bridge);
        lose_command(&mut bridge);
        assert_eq!(bridge.bye, None);
        peer.write_all(format!("OK subscribe 0\nEVENT * bye {word}\n").as_bytes())
            .unwrap();
        drop(peer);
        release.send(()).unwrap();
        bridge.finish_aterm();
        assert!(bridge.event_reader.is_none(), "the reader was joined");
        assert_eq!(bridge.mailbox.aterm_end(), Some(End::Eof));
        assert_eq!(bridge.bye, Some(bye));
        assert_eq!(bridge.departure_uncertain, None);
        bridge.end_run();
        if clean {
            assert!(run_record(&bridge).starts_with("clean "));
        } else {
            assert_eq!(
                run_record(&bridge),
                open,
                "handoff keeps the original OPEN fence"
            );
        }
    }
}

#[test]
fn command_loss_never_turns_an_idle_cut_or_wordless_stream_into_a_clean_departure() {
    for (bytes, eof, end) in [
        (b"OK subscribe 0\n".as_slice(), true, End::Eof),
        (
            b"".as_slice(),
            false,
            End::Failed(io::ErrorKind::WouldBlock),
        ),
        (
            b"EVENT * bye handoff\n".as_slice(),
            false,
            End::Failed(io::ErrorKind::WouldBlock),
        ),
        (
            b"EVENT * bye handoff".as_slice(),
            true,
            End::Failed(io::ErrorKind::UnexpectedEof),
        ),
        (
            b"EVENT * bye handoff\ncut".as_slice(),
            true,
            End::Failed(io::ErrorKind::UnexpectedEof),
        ),
        (
            b"EVENT * bye handoff\n\xff\n".as_slice(),
            true,
            End::Failed(io::ErrorKind::InvalidData),
        ),
    ] {
        let (_lane, mut bridge) = FakeVerbLane::bridge(&[]);
        bridge.state.begin_run().unwrap();
        let open = run_record(&bridge);
        let (mut peer, release) = gated_push(&mut bridge);
        lose_command(&mut bridge);
        peer.write_all(bytes).unwrap();
        let live_peer = if eof {
            drop(peer);
            None
        } else {
            Some(peer)
        };
        release.send(()).unwrap();
        bridge.finish_aterm();
        assert_eq!(
            bridge.mailbox.aterm_end(),
            Some(end),
            "bytes={bytes:?} eof={eof}"
        );
        assert!(bridge.departure_uncertain.is_some());
        assert_eq!(
            bridge.bye, None,
            "a partial/error/live stream is no final disposition"
        );
        bridge.end_run();
        assert_eq!(
            run_record(&bridge),
            open,
            "uncertainty retains the exact OPEN record"
        );
        drop(live_peer);
    }
}

#[test]
fn terminal_drain_releases_a_reader_already_blocked_on_event_queue_room() {
    let (_lane, mut bridge) = FakeVerbLane::bridge(&[]);
    for _ in 0..crate::mailbox::QUEUE_ROWS_MAX {
        bridge.mailbox.push_event("EVENT 1 post 1".into());
    }
    let (mut peer, release) = gated_push(&mut bridge);
    peer.write_all(b"EVENT 1 post 2\nEVENT * bye handoff\n")
        .unwrap();
    drop(peer);
    release.send(()).unwrap();
    aterm_spec::wait::until_within(
        aterm_spec::wait::MUST_HAPPEN,
        "the reader waits for event room",
        || (bridge.mailbox.event_waiters() == 1).then_some(()),
    );
    lose_command(&mut bridge);
    bridge.finish_aterm();
    assert_eq!(bridge.mailbox.event_waiters(), 0);
    assert_eq!(bridge.mailbox.aterm_end(), Some(End::Eof));
    assert_eq!(bridge.bye, Some(Bye::Handoff));
}

#[test]
fn dropping_a_bridge_joins_and_retires_its_idle_native_event_reader() {
    let (_lane, mut bridge) = FakeVerbLane::bridge(&[]);
    let (mut peer, release) = gated_push(&mut bridge);
    let mailbox = bridge.mailbox.clone();
    release.send(()).unwrap();
    drop(bridge);
    assert!(matches!(
        mailbox.aterm_end(),
        Some(End::Failed(io::ErrorKind::WouldBlock))
    ));
    assert_eq!(
        peer.read(&mut [0]).unwrap(),
        0,
        "the joined owner closed its descriptor"
    );
}

#[test]
fn a_consumed_genuine_bye_survives_an_explicit_simulated_eof() {
    for word in ["handoff", "quit"] {
        let (_lane, mut bridge) = FakeVerbLane::bridge(&[]);
        bridge.on_event(&format!("EVENT * bye {word}"));
        let known = bridge.bye;
        assert!(known.is_some());
        assert_eq!(
            bridge.mailbox.bye(),
            None,
            "on_event already consumed the word"
        );
        bridge.mailbox.push_aterm_closed();
        bridge.finish_aterm();
        assert_eq!(bridge.departure_uncertain, None);
        assert_eq!(bridge.bye, known);
    }
}

#[test]
fn event_framing_rejects_a_complete_oversized_line_and_accepts_exact_drain_eof() {
    let limit = 64;
    let bye = b"EVENT * bye handoff\n";
    for (draining, wire, valid) in [
        (false, [vec![b'x'; limit + 1], vec![b'\n']].concat(), false),
        (false, [vec![b'x'; limit], vec![b'\n']].concat(), true),
        (
            true,
            [vec![b'x'; limit - bye.len() - 1], vec![b'\n'], bye.to_vec()].concat(),
            true,
        ),
        (
            true,
            [vec![b'x'; limit - bye.len()], vec![b'\n'], bye.to_vec()].concat(),
            false,
        ),
    ] {
        let (near, mut peer) = UnixStream::pair().unwrap();
        let mailbox = Mailbox::default();
        if draining {
            mailbox.drain_aterm();
        }
        peer.write_all(&wire).unwrap();
        drop(peer);
        let wake = aterm_uds::ready::Doorbell::new().unwrap();
        let result = aterm_uds::ready::with_nonblocking(near.as_fd(), || {
            read_aterm_events(&near, &mailbox, &wake, limit)
        });
        assert_eq!(
            result.is_ok(),
            valid,
            "draining={draining} bytes={} result={result:?}",
            wire.len()
        );
        if draining && valid {
            assert_eq!(mailbox.bye(), Some(Bye::Handoff));
        }
        if let Err(error) = result {
            assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        }
    }
}
