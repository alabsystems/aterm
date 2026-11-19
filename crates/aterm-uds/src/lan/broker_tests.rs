// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

fn hello(protocol: u32) -> Vec<u8> {
    wire::encode(&Frame::Hello {
        protocol,
        build: "kernel-hello".into(),
    })
    .unwrap()
}

#[test]
fn valid_local_broker_dial_survives_scheduling_delay() {
    let dir = aterm_tempfile::Builder::new().tempdir_in("/tmp").unwrap();
    let path = dir.path().join("b.sock");
    let ours = super::super::protocol();
    let listener = crate::kept_out(|| crate::CtlListener::bind(&path)).unwrap();
    let worker = std::thread::spawn(move || {
        let (stream, _) = crate::accept(
            &listener,
            crate::AcceptWait::Forever,
            crate::AcceptedMode::Blocking,
        )
        .unwrap();
        if io::Write::write_all(&mut &stream, &hello(ours)).is_err() {
            return false;
        }
        matches!(Reader::new(0).frame(&stream, Some(Instant::now() + Duration::from_secs(60))),
            Ok(Some(Frame::Hello { protocol, .. })) if protocol == ours)
    });
    // A refused dial fails here with its own error, the worker left in its accept.
    let started = Instant::now() - Duration::from_secs(11);
    let session = open_started_at(None, &path, ours, Absent::NotInstalled, started).unwrap();
    assert_eq!(session.broker.protocol, ours);
    assert!(worker.join().unwrap());
}

#[test]
fn an_expired_local_broker_dial_is_refused_before_it_dials() {
    // No broker here: a dial that looked would read it as not installed.
    let dir = aterm_tempfile::Builder::new().tempdir_in("/tmp").unwrap();
    let path = dir.path().join("b.sock");
    let started = Instant::now() - Duration::from_secs(61);
    assert!(matches!(
        open_started_at(None, &path, super::super::protocol(), Absent::NotInstalled, started),
        Err(LanError::Io(error)) if error.kind() == io::ErrorKind::TimedOut
    ));
}

#[test]
fn valid_local_broker_hello_survives_its_check_and_scheduling_delay() {
    let (client, server) = crate::kept_out(CtlStream::pair).unwrap();
    let ours = super::super::protocol();
    io::Write::write_all(&mut &server, &hello(ours)).unwrap();
    let started = Instant::now() - Duration::from_secs(91);
    let result = exchange_at(client, None, ours, None, started, None);
    assert_eq!(result.unwrap().broker.protocol, ours);

    // A real peer has received the header; cancellation must end the owned connection
    // while either the broker's hello or its socket answer is still outstanding.
    for waiting_for_socket in [false, true] {
        let (client, server) = crate::kept_out(CtlStream::pair).unwrap();
        let stop = crate::stop::Stop::new();
        let request = Request::Connect {
            target: Target::Addr("127.0.0.1:1".parse().unwrap()),
            port: 1,
            within_ms: 20_000,
            bound_if: None,
            source: None,
            hop_limit: None,
        };
        if waiting_for_socket {
            wire::send(
                &server,
                &Frame::Hello {
                    protocol: ours,
                    build: "held-answer".into(),
                },
            )
            .unwrap();
        }
        std::thread::scope(|scope| {
            let caller_stop = &stop;
            let (entered, observed) = std::sync::mpsc::sync_channel(1);
            let worker = scope.spawn(move || -> Result<(), LanError> {
                let cancel = Some(caller_stop.bell()?);
                let mut session = exchange_at(
                    client,
                    Some(&request),
                    ours,
                    None,
                    Instant::now() - Duration::from_secs(60),
                    cancel,
                )?;
                entered.send(()).unwrap();
                let _ =
                    session.socket_cancelled(Instant::now() + Duration::from_secs(60), cancel)?;
                Ok(())
            });
            let mut reader = Reader::new(0);
            let deadline = Some(Instant::now() + Duration::from_secs(60));
            assert!(matches!(
                reader.frame(&server, deadline).unwrap(),
                Some(Frame::Hello { .. })
            ));
            assert!(matches!(
                reader.frame(&server, deadline).unwrap(),
                Some(Frame::Request(Request::Connect { .. }))
            ));
            if waiting_for_socket {
                observed.recv_timeout(Duration::from_secs(60)).unwrap();
            }
            assert!(stop.set());
            assert!(
                matches!(worker.join().unwrap(), Err(LanError::Io(error)) if error.kind() == io::ErrorKind::Interrupted)
            );
            assert!(
                reader.frame(&server, deadline).unwrap().is_none(),
                "cancellation closed the owned control socket"
            );
        });
    }
}

#[test]
fn a_queued_broker_verdict_wins_when_the_client_hello_cannot_be_written() {
    for refused in [false, true] {
        let (client, server) = crate::kept_out(CtlStream::pair).unwrap();
        let words = "the broker's verdict".to_owned();
        let frame = if refused {
            Frame::Refused {
                words: words.clone(),
            }
        } else {
            Frame::Unchecked {
                words: words.clone(),
            }
        };
        io::Write::write_all(&mut &server, &wire::encode(&frame).unwrap()).unwrap();
        server.shutdown(std::net::Shutdown::Write).unwrap();
        client.shutdown(std::net::Shutdown::Write).unwrap();
        assert!(io::Write::write_all(&mut &client, b"unwritable hello").is_err());
        // The check took its whole minute. Its queued answer wins over the closed
        // write half's error; deleting that precedence is the negative control.
        let started = Instant::now() - Duration::from_secs(61);
        let result = exchange_at(client, None, super::super::protocol(), None, started, None);
        let expected = if refused {
            Unavailable::Refused(words)
        } else {
            Unavailable::Unchecked(words)
        };
        assert!(matches!(result, Err(LanError::Unavailable(said)) if said == expected));
    }
}

#[test]
fn an_expired_local_broker_hello_is_still_refused() {
    let (client, server) = crate::kept_out(CtlStream::pair).unwrap();
    let ours = super::super::protocol();
    io::Write::write_all(&mut &server, &hello(ours)).unwrap();
    assert!(matches!(
        exchange_at(client, None, ours, None, Instant::now() - Duration::from_secs(121), None),
        Err(LanError::Io(error)) if error.kind() == io::ErrorKind::TimedOut
    ));
}

/// A stand-in broker at `path` that says its hello, reads the client's hello and request, then
/// answers with `answer` (none: it closes, as a broker whose wire predates the request does
/// once it cannot read it). It returns the request it read.
fn stand_in(path: &Path, answer: Option<Frame>) -> std::thread::JoinHandle<Option<Request>> {
    let listener = crate::kept_out(|| crate::CtlListener::bind(path)).unwrap();
    let ours = super::super::protocol();
    std::thread::spawn(move || {
        let (stream, _) = crate::accept(
            &listener,
            crate::AcceptWait::Forever,
            crate::AcceptedMode::Blocking,
        )
        .unwrap();
        io::Write::write_all(&mut &stream, &hello(ours)).unwrap();
        let mut reader = Reader::new(0);
        let deadline = Some(Instant::now() + Duration::from_secs(60));
        assert!(matches!(
            reader.frame(&stream, deadline),
            Ok(Some(Frame::Hello { .. }))
        ));
        let request = match reader.frame(&stream, deadline) {
            Ok(Some(Frame::Request(request))) => Some(request),
            _ => None,
        };
        if let Some(answer) = answer {
            wire::send(&stream, &answer).unwrap();
        }
        request
    })
}

/// The Spotlight request goes to the broker alone and reads each of its answers: a broker
/// that closes after its hello (one whose wire predates the request) is unserved, naming its
/// build, as no broker at all is; a count is what it excluded; a refusal keeps its words.
/// NEGATIVE CONTROL: reading a close after the hello as nothing excluded fails the first case.
#[test]
fn the_spotlight_request_reads_each_answer_and_an_older_broker_as_unserved() {
    let dir = aterm_tempfile::Builder::new().tempdir_in("/tmp").unwrap();
    let ours = super::super::protocol();
    let path = dir.path().join("old.sock");
    let older = stand_in(&path, None);
    match scope_home_at(&path, ours) {
        ScopeAnswer::Unserved(why) => {
            assert!(
                why.contains("build kernel-hello") && why.contains("predates"),
                "{why}"
            );
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(older.join().unwrap(), Some(Request::ScopeHome));

    let path = dir.path().join("new.sock");
    let current = stand_in(&path, Some(Frame::Scoped { added: 3 }));
    assert_eq!(scope_home_at(&path, ours), ScopeAnswer::Excluded(3));
    assert_eq!(current.join().unwrap(), Some(Request::ScopeHome));

    let path = dir.path().join("refuses.sock");
    let refuses = stand_in(
        &path,
        Some(Frame::Error {
            code: libc::EPERM,
            words: "not the console's".into(),
        }),
    );
    assert_eq!(
        scope_home_at(&path, ours),
        ScopeAnswer::Refused("not the console's".into())
    );
    refuses.join().unwrap();

    match scope_home_at(&dir.path().join("none.sock"), ours) {
        ScopeAnswer::Unserved(why) => assert!(why.contains("not installed"), "{why}"),
        other => panic!("{other:?}"),
    }
}

/// A browse's add of instance `name`, as the broker streams it.
fn added(name: &str) -> Event {
    Event::Added(wire::Instance {
        name: name.to_owned(),
        service: "_alab-vault._tcp.".to_owned(),
        domain: "local.".to_owned(),
        interface: 4,
    })
}

/// A request's end of a broker connection, its hellos done, and the broker's end.
fn events_pair() -> (Events, CtlStream) {
    let (client, broker) = crate::kept_out(CtlStream::pair).unwrap();
    let events = Events {
        stream: client,
        reader: Reader::new(1),
        ended: false,
    };
    (events, broker)
}

/// `Events::next_beside` hands over every event already read off the connection before it
/// looks at its cancel; with none read, a readable cancel wins over what the connection holds,
/// which the next call takes; and a cancel ends no request. NEGATIVE CONTROLS: a cancel looked
/// at before the events already read answers the second call `None` with `m2` held back; a
/// connection looked at before the cancel answers the fourth with `m3`; a cancel that ends the
/// request fails the fifth.
#[test]
fn next_beside_returns_buffered_frames_before_cancel_and_keeps_the_stream() {
    let hang = Duration::from_secs(60);
    let (mut events, broker) = events_pair();
    let cancel = crate::ready::Doorbell::new().unwrap();
    let mut two = wire::encode(&Frame::Event(added("m1"))).unwrap();
    two.extend(wire::encode(&Frame::Event(added("m2"))).unwrap());
    // One write: the first receive takes both frames in.
    io::Write::write_all(&mut &broker, &two).unwrap();
    assert_eq!(events.next_beside(cancel.fd()).unwrap(), Some(added("m1")));
    assert!(events.buffered(), "both frames came in one receive");
    cancel.ring();
    assert_eq!(events.next_beside(cancel.fd()).unwrap(), Some(added("m2")));
    assert_eq!(events.next_beside(cancel.fd()).unwrap(), None, "cancelled");
    wire::send(&broker, &Frame::Event(added("m3"))).unwrap();
    assert_eq!(
        crate::ready::any_until(&[events.as_fd()], Instant::now() + hang).unwrap(),
        [0],
        "m3 waits on the connection"
    );
    assert_eq!(
        events.next_beside(cancel.fd()).unwrap(),
        None,
        "the cancel wins over what the connection holds"
    );
    assert!(cancel.clear());
    assert_eq!(events.next_beside(cancel.fd()).unwrap(), Some(added("m3")));
    assert!(!events.ended, "no cancel ended the request");
}

/// The broker's end of a request ends it, under `Events::next_beside` as under every other
/// wait: once, then `UnexpectedEof` for good; a cancel never does. NEGATIVE CONTROL: a
/// cancel that sets `ended` fails the first assertion's follow-up read.
#[test]
fn next_beside_ends_the_stream_on_eof_not_on_cancel() {
    let (mut events, broker) = events_pair();
    let cancel = crate::ready::Doorbell::new().unwrap();
    cancel.ring();
    assert_eq!(events.next_beside(cancel.fd()).unwrap(), None);
    assert!(!events.ended, "a cancel ends no request");
    assert!(cancel.clear());
    wire::send(&broker, &Frame::Event(added("m1"))).unwrap();
    assert_eq!(events.next_beside(cancel.fd()).unwrap(), Some(added("m1")));
    drop(broker);
    let ended = |result: Result<Option<Event>, LanError>| matches!(result, Err(LanError::Io(error)) if error.kind() == io::ErrorKind::UnexpectedEof);
    assert!(ended(events.next_beside(cancel.fd())), "the broker's end");
    assert!(events.ended);
    cancel.ring();
    assert!(ended(events.next_beside(cancel.fd())), "ended for good");
}
