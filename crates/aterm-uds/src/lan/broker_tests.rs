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
