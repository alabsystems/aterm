// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Native socket framing and resource bounds; no person's service or credentials.

use super::super::Owner;
use super::*;
use aterm_capacity::host_protocol::{Job, Phase as JobPhase};

fn source() -> (Owner, Source) {
    let owner = Owner::new(1, 2).unwrap();
    let observer = owner.observer();
    let generation = observer.latest().unwrap().0.generation;
    let source = Source::new(observer, generation, Arc::clone(&owner.monitor_connections));
    (owner, source)
}

fn deadline() -> Instant {
    Instant::now() + aterm_spec::wait::MUST_HAPPEN
}

pub(super) fn serve(mut exchange: Exchange) -> io::Result<()> {
    while exchange.step()? == Progress::Pending {
        let (fd, interest) = exchange.readiness().expect("pending exchange");
        ready::until(fd, interest, exchange.deadline())?;
    }
    Ok(())
}

#[test]
fn one_shot_snapshot_preserves_original_age_and_closed_owner_state() {
    let (owner, source) = source();
    assert!(
        owner.monitor_source().is_err(),
        "unfenced owner cannot delegate the public transport"
    );
    let original = Instant::now() - Duration::from_secs(7);
    source.observer.0.lock().unwrap().at = original;
    drop(owner);
    let (client, server) = source.pair(deadline()).unwrap();
    let reply = std::thread::scope(|scope| {
        let server = scope.spawn(|| serve(server));
        let reply = client.read_until(deadline()).unwrap();
        server.join().unwrap().unwrap();
        reply
    });
    assert!(reply.snapshot.closed);
    assert_eq!(reply.snapshot.generation, source.generation);
    let age = reply.age_bounds().unwrap();
    assert!(age.lower >= Duration::from_secs(7));
    assert!(age.upper >= age.lower);
    assert_eq!(source.observer.latest().unwrap().1, original);
    assert_eq!(source.active.load(Ordering::Acquire), 0);
    let (client, server) = source.pair(deadline()).unwrap();
    let later = std::thread::scope(|scope| {
        let server = scope.spawn(|| serve(server));
        let reply = client.read_until(deadline()).unwrap();
        server.join().unwrap().unwrap();
        reply
    });
    assert!(later.age_bounds().unwrap().lower >= age.lower);
    assert!(later.snapshot.closed);
    assert_eq!(source.observer.latest().unwrap().1, original);
}

#[test]
fn unknown_partial_extra_and_stale_requests_fail_without_mutating_publication() {
    let (_owner, source) = source();
    let before = source.observer.latest().unwrap();
    for mode in 0..4 {
        let (client, mut server) = source.pair(deadline()).unwrap();
        let mut stream = client.into_stream();
        let mut request = Vec::from(*REQUEST);
        request.extend_from_slice(&source.generation.0);
        match mode {
            0 => request[0] = b'?',
            1 => request.truncate(3),
            2 => request.push(1),
            3 => request[8] ^= 1,
            _ => unreachable!(),
        }
        write_all(&mut stream, &request, deadline()).unwrap();
        stream.shutdown(Shutdown::Write).unwrap();
        assert!(server.step().is_err());
        assert_eq!(server.step().unwrap(), Progress::Complete);
    }
    let after = source.observer.latest().unwrap();
    assert_eq!(before.0, after.0);
    assert_eq!(before.1, after.1);
    assert_eq!(source.active.load(Ordering::Acquire), 0);
}

#[test]
fn capacity_is_shared_across_delegations_and_slow_io_never_waits_or_renews_deadline() {
    let (owner, source) = source();
    let second = Source::new(
        owner.observer(),
        source.generation,
        Arc::clone(&owner.monitor_connections),
    );
    let mut held = Vec::new();
    for _ in 0..MAX_CONNECTIONS {
        held.push(source.pair(deadline()).unwrap());
    }
    assert!(matches!(second.pair(deadline()), Err(e) if e.kind() == io::ErrorKind::WouldBlock));
    for (_, server) in &mut held {
        let original = server.deadline();
        assert_eq!(server.step().unwrap(), Progress::Pending);
        assert_eq!(server.deadline(), original);
    }
    held.pop();
    let (client, mut server) = second.pair(deadline()).unwrap();
    let mut stream = client.into_stream();
    write_all(&mut stream, b"ATM", deadline()).unwrap();
    assert_eq!(server.step().unwrap(), Progress::Pending);
    server.deadline = Instant::now();
    assert_eq!(server.step().unwrap_err().kind(), io::ErrorKind::TimedOut);
    assert_eq!(server.step().unwrap(), Progress::Complete);
    drop(held);
    assert_eq!(source.active.load(Ordering::Acquire), 0);
}

#[test]
fn large_snapshot_writes_are_bounded_per_step_and_disconnect_is_local() {
    let (_owner, source) = source();
    let snapshot = Snapshot {
        generation: source.generation,
        revision: 1,
        closed: false,
        jobs: (1..=256)
            .map(|attempt| Job {
                attempt,
                phase: JobPhase::Waiting,
                reserved_bytes: u64::MAX,
                failure: None,
            })
            .collect(),
    };
    assert!(snapshot.encode().unwrap().len() > WRITE_TURN);
    source.observer.0.lock().unwrap().snapshot = Arc::new(snapshot);
    let (client, mut server) = source.pair(deadline()).unwrap();
    let mut stream = client.into_stream();
    let mut request = Vec::from(*REQUEST);
    request.extend_from_slice(&source.generation.0);
    write_all(&mut stream, &request, deadline()).unwrap();
    stream.shutdown(Shutdown::Write).unwrap();
    assert_eq!(server.step().unwrap(), Progress::Pending);
    assert!(matches!(&server.phase, Phase::Reply { sent, .. } if *sent <= WRITE_TURN));
    drop(stream);
    assert!(server.step().is_err());
    assert_eq!(source.active.load(Ordering::Acquire), 0);
    assert!(!source.observer.latest().unwrap().0.closed);
}

#[test]
fn client_rejects_unknown_oversized_truncated_extra_and_foreign_replies() {
    let (_owner, source) = source();
    for mode in 0..5 {
        let mut snapshot = (*source.observer.latest().unwrap().0).clone();
        if mode == 4 {
            snapshot.generation.0[0] ^= 1;
        }
        let body = snapshot.encode().unwrap();
        let mut bytes = Vec::from(*RESPONSE);
        bytes.extend_from_slice(
            &(if mode == 1 {
                MAX_FRAME_BYTES as u32 + 1
            } else {
                body.len() as u32
            })
            .to_be_bytes(),
        );
        bytes.extend_from_slice(&0u64.to_be_bytes());
        bytes.extend_from_slice(&body);
        match mode {
            0 => bytes[0] = b'?',
            2 => {
                bytes.pop();
            }
            3 => bytes.push(1),
            _ => {}
        }
        let (client_stream, mut fake) = aterm_uds::kept_out(CtlStream::pair).unwrap();
        fake.set_nonblocking(true).unwrap();
        let client = Client::from_trusted_stream(client_stream, source.generation).unwrap();
        std::thread::scope(|scope| {
            let fake = scope.spawn(move || {
                let mut request = [0; REQUEST_LEN];
                read_exact(&mut fake, &mut request, deadline()).unwrap();
                read_end(&mut fake, deadline()).unwrap();
                // An early-rejected peer may close before this write finishes.
                let _ = write_all(&mut fake, &bytes, deadline());
            });
            assert!(client.read_until(deadline()).is_err());
            fake.join().unwrap();
        });
    }
}
