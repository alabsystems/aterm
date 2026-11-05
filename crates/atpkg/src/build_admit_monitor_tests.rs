// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Native socket framing, persistence, history and resource-bound controls.

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
        exchange.wait(exchange.deadline())?;
    }
    Ok(())
}
fn publication(source: &Source, revision: u64, first: u64, count: usize) {
    let mut state = source.observer.0.lock().unwrap();
    state.snapshot = Arc::new(Snapshot {
        generation: source.generation,
        revision,
        closed: false,
        jobs: Vec::new(),
        epochs: Vec::new(),
    });
    state.events = (0..count)
        .map(|index| Record {
            revision: first + index as u64,
            attempt: None,
            event: Event::StateChanged,
        })
        .collect();
    state.limit = 256;
    drop(state);
    source.observer.1.notify_all();
}
fn response(read: &super::super::Read, requested: Option<Cursor>, sequence: u64) -> Vec<u8> {
    let body = encode_body(read, requested).unwrap();
    let mut bytes = Vec::from(*RESPONSE);
    bytes.extend_from_slice(&sequence.to_be_bytes());
    bytes.extend_from_slice(&(body.len() as u32).to_be_bytes());
    bytes.extend_from_slice(&0u64.to_be_bytes());
    bytes.extend_from_slice(&body);
    bytes
}
fn raw_pair(source: &Source) -> (CtlStream, Exchange) {
    let (client, server) = source.pair(deadline()).unwrap();
    (client.into_stream(), server)
}

#[test]
fn one_read_preserves_original_age_closed_owner_and_releases_its_permit() {
    let (owner, source) = source();
    assert!(
        owner.monitor_source().is_err(),
        "unfenced owners cannot mint public transport"
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
    assert_eq!(reply.request_sequence, 1);
    assert_eq!(reply.next.revision, reply.snapshot.revision);
    assert_eq!(reply.history.last().unwrap().kind, EventKind::OwnerClosed);
    let age = reply.age_bounds().unwrap();
    assert!(age.lower >= Duration::from_secs(7));
    assert!(age.upper >= age.lower);
    assert_eq!(source.observer.latest().unwrap().1, original);
    assert_eq!(source.active.load(Ordering::Acquire), 0);
}

#[test]
fn one_persistent_capability_reads_publication_changes_and_unchanged_heartbeat_age() {
    let (_owner, source) = source();
    let original = Instant::now() - Duration::from_secs(7);
    source.observer.0.lock().unwrap().at = original;
    let (mut client, server) = source.pair(deadline()).unwrap();
    std::thread::scope(|scope| {
        let server = scope.spawn(|| serve(server));
        let first = client.read_next(deadline()).unwrap();
        assert_eq!(first.snapshot.revision, 0);
        assert_eq!(source.active.load(Ordering::Acquire), 1);
        publication(&source, 1, 1, 1);
        let changed = client.read_next(deadline()).unwrap();
        assert_eq!(changed.snapshot.revision, 1);
        assert_eq!(changed.next.revision, 1);
        assert_eq!(changed.request_sequence, 2);
        let heartbeat = client.read_next(deadline()).unwrap();
        assert_eq!(heartbeat.request_sequence, 3);
        assert_eq!(heartbeat.snapshot, changed.snapshot);
        assert_eq!(heartbeat.next, changed.next);
        assert!(heartbeat.history.is_empty());
        assert!(heartbeat.gap.is_none());
        assert!(heartbeat.age_bounds().unwrap().lower >= Duration::from_secs(7));
        assert_eq!(source.observer.latest().unwrap().1, original);
        drop(client);
        server.join().unwrap().unwrap();
    });
    assert_eq!(source.active.load(Ordering::Acquire), 0);
}

#[test]
fn bounded_history_pages_keep_cursor_separate_from_snapshot_and_report_exact_gap() {
    let (_owner, source) = source();
    publication(&source, 200, 1, 200);
    let (mut client, server) = source.pair(deadline()).unwrap();
    std::thread::scope(|scope| {
        let server = scope.spawn(|| serve(server));
        let first = client.read_next(deadline()).unwrap();
        assert_eq!(first.snapshot.revision, 200);
        assert_eq!(first.next.revision, 64);
        assert_eq!(first.history.len(), 64);
        publication(&source, 200, 101, 100);
        let second = client.read_next(deadline()).unwrap();
        assert_eq!(
            second.gap,
            Some(Gap {
                first: 65,
                last: 100
            })
        );
        assert_eq!(second.history.len(), 64);
        assert_eq!(second.history.first().unwrap().revision, 101);
        assert_eq!(second.next.revision, 164);
        let third = client.read_next(deadline()).unwrap();
        assert_eq!(third.history.len(), 36);
        assert_eq!(third.history.first().unwrap().revision, 165);
        assert_eq!(third.next.revision, 200);
        assert!(third.gap.is_none());
        drop(client);
        server.join().unwrap().unwrap();
    });
}

#[test]
fn closed_publication_keeps_all_retained_pages_available_before_terminal_cursor() {
    let (owner, source) = source();
    publication(&source, 200, 1, 200);
    drop(owner);
    let (mut client, server) = source.pair(deadline()).unwrap();
    std::thread::scope(|scope| {
        let server = scope.spawn(|| serve(server));
        let mut revisions = Vec::new();
        loop {
            let frame = client.read_next(deadline()).unwrap();
            assert!(frame.snapshot.closed);
            revisions.extend(frame.history.iter().map(|record| record.revision));
            if frame.next.revision == frame.snapshot.revision {
                assert_eq!(frame.history.last().unwrap().kind, EventKind::OwnerClosed);
                break;
            }
        }
        assert_eq!(revisions, (1..=201).collect::<Vec<_>>());
        drop(client);
        server.join().unwrap().unwrap();
    });
}

#[test]
fn unknown_partial_pipelined_stale_and_noncanonical_requests_fail_without_publication_changes() {
    let (_owner, source) = source();
    let before = source.observer.latest().unwrap();
    for mode in 0..7 {
        let (mut stream, mut server) = raw_pair(&source);
        let mut request = Vec::from(encode_request(source.generation, 1, None));
        match mode {
            0 => request[0] = b'?',
            1 => request.truncate(3),
            2 => request.push(1),
            3 => request[8] ^= 1,
            4 => request[31] = 2,
            5 => request[32] = 1,
            6 => request[47] = 1,
            _ => unreachable!(),
        }
        write_all(&mut stream, &request, deadline()).unwrap();
        if mode == 1 {
            stream.shutdown(Shutdown::Write).unwrap();
        }
        assert!(server.step().is_err(), "mode {mode}");
        assert_eq!(server.step().unwrap(), Progress::Complete);
    }
    let after = source.observer.latest().unwrap();
    assert_eq!(before.0, after.0);
    assert_eq!(before.1, after.1);
    assert_eq!(source.active.load(Ordering::Acquire), 0);
}

#[test]
fn shared_capacity_partial_deadlines_and_completed_exchange_idle_renewal_are_bounded() {
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
    let (mut stream, mut server) = raw_pair(&second);
    write_all(&mut stream, b"ATM", deadline()).unwrap();
    let original = server.deadline();
    assert_eq!(server.step().unwrap(), Progress::Pending);
    assert_eq!(
        server.deadline(),
        original,
        "partial bytes cannot extend admission"
    );
    server.deadline = Instant::now();
    assert_eq!(server.step().unwrap_err().kind(), io::ErrorKind::TimedOut);
    assert_eq!(server.step().unwrap(), Progress::Complete);
    drop(held);
    assert_eq!(source.active.load(Ordering::Acquire), 0);

    let initial = Instant::now() + Duration::from_secs(1);
    let (client, mut server) = source.pair(initial).unwrap();
    let mut stream = client.into_stream();
    write_all(
        &mut stream,
        &encode_request(source.generation, 1, None),
        deadline(),
    )
    .unwrap();
    assert_eq!(server.step().unwrap(), Progress::Pending);
    assert!(matches!(server.phase, Phase::Request { used: 0, .. }));
    assert!(
        server.deadline() > initial,
        "a complete frame renews idle, not a fixed connection lifetime"
    );
    // Consume the completed frame before graceful close. Dropping unread reply
    // bytes can correctly produce a peer reset rather than idle EOF.
    let mut header = [0; HEADER_LEN];
    read_exact(&mut stream, &mut header, deadline()).unwrap();
    let len = u32::from_be_bytes(header[16..20].try_into().unwrap()) as usize;
    let mut body = vec![0; len];
    read_exact(&mut stream, &mut body, deadline()).unwrap();
    drop(stream);
    assert_eq!(server.step().unwrap(), Progress::Complete);
}

#[test]
fn large_snapshot_writes_remain_bounded_and_disconnect_has_no_native_effect() {
    let (_owner, source) = source();
    publication(&source, 1, 1, 1);
    source.observer.0.lock().unwrap().snapshot = Arc::new(Snapshot {
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
        epochs: Vec::new(),
    });
    let original = source.observer.latest().unwrap().0;
    assert!(source.observer.latest().unwrap().0.encode().unwrap().len() > WRITE_TURN);
    let (mut stream, mut server) = raw_pair(&source);
    write_all(
        &mut stream,
        &encode_request(source.generation, 1, None),
        deadline(),
    )
    .unwrap();
    assert_eq!(server.step().unwrap(), Progress::Pending);
    assert!(matches!(&server.phase, Phase::Reply { sent, .. } if *sent > 0 && *sent <= WRITE_TURN));
    drop(stream);
    // A peer close may leave buffered writes successful before the next write
    // fails or the completed reply reaches idle EOF. Pump the actual transport
    // to either disconnect outcome; the deadline detects a hang, not latency.
    let until = deadline();
    loop {
        assert!(Instant::now() < until, "monitor did not observe disconnect");
        match server.step() {
            Ok(Progress::Complete) => break,
            Ok(Progress::Pending) => server.wait(until).unwrap(),
            Err(error) => {
                assert!(
                    matches!(
                        error.kind(),
                        io::ErrorKind::BrokenPipe
                            | io::ErrorKind::ConnectionReset
                            | io::ErrorKind::ConnectionAborted
                    ),
                    "expected disconnect, got {error:?}"
                );
                break;
            }
        }
    }
    assert_eq!(server.step().unwrap(), Progress::Complete);
    assert_eq!(source.active.load(Ordering::Acquire), 0);
    assert_eq!(source.observer.latest().unwrap().0, original);
    assert!(!original.closed);
}

#[test]
fn malformed_frames_poison_the_capability_and_cannot_be_retried() {
    let (_owner, source) = source();
    for mode in 0..7 {
        let read = source.observer.read(None).unwrap();
        let mut bytes = response(&read, None, 1);
        match mode {
            0 => bytes[0] = b'?',
            1 => bytes[16..20].copy_from_slice(&(MAX_FRAME_BYTES as u32).to_be_bytes()),
            2 => {
                bytes.pop();
            }
            3 => bytes.push(1),
            4 => bytes[15] = 2,
            5 => bytes[HEADER_LEN] = 1,
            6 => bytes[HEADER_LEN + 17] = 9,
            _ => unreachable!(),
        }
        let (stream, mut fake) = aterm_uds::kept_out(CtlStream::pair).unwrap();
        fake.set_nonblocking(true).unwrap();
        write_all(&mut fake, &bytes, deadline()).unwrap();
        fake.shutdown(Shutdown::Write).unwrap();
        let mut client = Client::from_trusted_stream(stream, source.generation).unwrap();
        assert!(client.read_next(deadline()).is_err(), "mode {mode}");
        assert!(client.stream.is_none());
        assert_eq!(
            client.read_next(deadline()).unwrap_err().kind(),
            io::ErrorKind::NotConnected
        );
        assert!(
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| client.into_stream()))
                .is_err()
        );
    }
}

#[test]
fn partial_reply_timeout_shuts_down_duplicates_and_rejects_late_suffix_bytes() {
    let (_owner, source) = source();
    let read = source.observer.read(None).unwrap();
    let bytes = response(&read, None, 1);
    let (stream, mut fake) = aterm_uds::kept_out(CtlStream::pair).unwrap();
    fake.set_nonblocking(true).unwrap();
    let duplicate = aterm_uds::kept_out(|| stream.try_clone()).unwrap();
    let mut client = Client::from_trusted_stream(stream, source.generation).unwrap();
    let partial = HEADER_LEN + 2;
    write_all(&mut fake, &bytes[..partial], deadline()).unwrap();
    assert_eq!(
        client
            .read_next(Instant::now() + Duration::from_millis(100))
            .unwrap_err()
            .kind(),
        io::ErrorKind::TimedOut,
    );
    assert!(client.stream.is_none());
    // Darwin may accept a buffered peer write after SHUT_RDWR while another
    // descriptor retains the connection. Either kernel write outcome is valid;
    // the capability must remain poisoned and cannot consume that late suffix.
    let _ = write_all(&mut fake, &bytes[partial..], deadline());
    let mut duplicate = duplicate;
    assert_eq!(
        duplicate.read(&mut [0]).unwrap(),
        0,
        "poison shutdown applies to retained duplicates"
    );
    assert_eq!(
        client.read_next(deadline()).unwrap_err().kind(),
        io::ErrorKind::NotConnected
    );
}

#[test]
fn decoder_rejects_whole_history_gaps_empty_fresh_history_and_noncontiguous_pages() {
    let (_owner, source) = source();
    publication(&source, 10, 6, 5);
    let cursor = Some(Cursor {
        generation: source.generation,
        revision: 3,
    });
    let read = source.observer.read(cursor).unwrap();
    let valid = encode_body(&read, cursor).unwrap();
    assert!(decode_body(&valid, source.generation, 1, cursor).is_ok());
    for mode in 0..6 {
        let mut bytes = valid.clone();
        match mode {
            0 => bytes[18..26].copy_from_slice(&5u64.to_be_bytes()),
            1 => bytes[26..34].copy_from_slice(&10u64.to_be_bytes()),
            2 => bytes[9..17].copy_from_slice(&9u64.to_be_bytes()),
            3 => {
                let end = META_LEN + read.snapshot.encode().unwrap().len();
                bytes[end..end + 8].copy_from_slice(&7u64.to_be_bytes());
            }
            4 => {
                let end = META_LEN + read.snapshot.encode().unwrap().len();
                bytes[end + 16] = 99;
            }
            5 => bytes[35] = 65,
            _ => unreachable!(),
        }
        assert!(
            decode_body(&bytes, source.generation, 1, cursor).is_err(),
            "mode {mode}"
        );
    }
    let end = META_LEN + read.snapshot.encode().unwrap().len();
    let mut skipped = valid[..end].to_vec();
    skipped[34..36].copy_from_slice(&0u16.to_be_bytes());
    skipped[26..34].copy_from_slice(&10u64.to_be_bytes());
    assert!(decode_body(&skipped, source.generation, 1, cursor).is_err());
    skipped[0] = 0;
    skipped[1..9].fill(0);
    skipped[17..34].fill(0);
    assert!(decode_body(&skipped, source.generation, 1, None).is_err());
}

#[test]
fn same_revision_snapshot_mutation_is_refused_on_a_persistent_connection() {
    let (_owner, source) = source();
    publication(&source, 1, 1, 1);
    let read = source.observer.read(None).unwrap();
    let first = response(&read, None, 1);
    let cursor = Some(read.next);
    let mut changed = source.observer.read(cursor).unwrap();
    Arc::make_mut(&mut changed.snapshot).jobs.push(Job {
        attempt: 1,
        phase: JobPhase::Waiting,
        reserved_bytes: 1,
        failure: None,
    });
    let second = response(&changed, cursor, 2);
    let (stream, mut fake) = aterm_uds::kept_out(CtlStream::pair).unwrap();
    fake.set_nonblocking(true).unwrap();
    let mut client = Client::from_trusted_stream(stream, source.generation).unwrap();
    std::thread::scope(|scope| {
        let fake = scope.spawn(move || {
            let mut request = [0; REQUEST_LEN];
            read_exact(&mut fake, &mut request, deadline()).unwrap();
            write_all(&mut fake, &first, deadline()).unwrap();
            read_exact(&mut fake, &mut request, deadline()).unwrap();
            write_all(&mut fake, &second, deadline()).unwrap();
        });
        client.read_next(deadline()).unwrap();
        assert!(client.read_next(deadline()).is_err());
        fake.join().unwrap();
    });
    assert!(client.stream.is_none());
}
